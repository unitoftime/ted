//! The user's `init.rhai` script.
//!
//! The script runs in a sandboxed Rhai engine and only *records* operations. The editor
//! applies them afterwards (`Editor::apply_config`), so scripts never hold editor state and
//! can be re-run at any time (`M-x reload-init`). What a script can call:
//!
//! - `set(name, value)`: a setting.
//! - `bind(keys, command)` / `bind(keymap, keys, command)`: a key binding, global or in a
//!   keymap (a mode's is named after it: `"git-log"`). `keys` may be an array of key
//!   sequences, and `command` may carry an argument (`"describe-prefix C-c"`).
//! - `unbind(keys)` / `unbind(keymap, keys)`.
//! - `face(name, #{ fg, bg, bold, italic, underline })`: restyles a face.
//! - `mode(name, #{ .. })`: the mode's `comment`, `extensions`, `line_numbers` and
//!   `highlight_line`, and any setting, which then applies to the mode's buffers.
//!
//! Every mistake is reported with its line, and the rest of the script still applies.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use rhai::{Dynamic, Engine, Map, NativeCallContext};

use crate::face::Face;
use crate::frame::Color;
use crate::mode::keymap_name;
use crate::settings::{self, Value};

fn value_from_dynamic(value: Dynamic) -> Result<Value, String> {
    if value.is_array() {
        let items = value.into_array().unwrap_or_default();
        items.into_iter().map(value_from_dynamic).collect::<Result<_, _>>().map(Value::List)
    } else if value.is_map() {
        map_from_rhai(value.cast::<Map>()).map(Value::Map)
    } else if let Ok(b) = value.as_bool() {
        Ok(Value::Bool(b))
    } else if let Ok(i) = value.as_int() {
        Ok(Value::Int(i))
    } else if let Ok(f) = value.as_float() {
        Ok(Value::Float(f))
    } else {
        value.into_string().map(Value::Str).map_err(|t| format!("Unsupported value type {}", t))
    }
}

fn map_from_rhai(map: Map) -> Result<settings::Map, String> {
    map.into_iter().map(|(key, v)| Ok((key.to_string(), value_from_dynamic(v)?))).collect()
}

/// One operation recorded by the init script.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigOp {
    Set {
        key: String,
        value: Value,
    },
    /// `command` may carry an argument after a space, e.g. `layout-restore 2`.
    Bind {
        keymap: String,
        keys: String,
        command: String,
    },
    Unbind {
        keymap: String,
        keys: String,
    },
    /// Overrides a face, e.g. `face("keyword", #{ fg: "#c586c0", bold: true })`.
    Face {
        name: String,
        face: Face,
    },
    /// Changes a mode's properties and settings, e.g. `mode("rust", #{ tab_width: 2 })`.
    Mode {
        name: String,
        values: settings::Map,
    },
}

/// What running the init script recorded: operations for the editor to apply, and the
/// calls it couldn't make sense of, each with the line of the script it came from.
#[derive(Debug, Default)]
pub struct Script {
    pub ops: Vec<(usize, ConfigOp)>,
    pub errors: Vec<(usize, String)>,
}

pub fn default_init_script() -> Option<PathBuf> {
    config_dir().map(|d| d.join("init.rhai"))
}

pub fn config_dir() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(xdg).join("ted"));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config/ted"))
}

/// Runs `path` and returns what it recorded. A missing file records nothing.
pub fn run_init_script(path: &Path) -> Result<Script, String> {
    if !path.exists() {
        return Ok(Script::default());
    }
    let source = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    Ok(run_script(&source))
}

/// Runs `source`. A call that fails is recorded as an error and the script goes on; an
/// error that stops the script keeps what it recorded before it.
pub fn run_script(source: &str) -> Script {
    let script = Arc::new(Mutex::new(Script::default()));
    let mut engine = Engine::new();

    let s = script.clone();
    engine.register_fn("set", move |ctx: NativeCallContext, key: &str, value: Dynamic| {
        let op = value_from_dynamic(value).map(|value| ConfigOp::Set { key: key.to_string(), value });
        push(&s, &ctx, op.map(|op| vec![op]).map_err(|e| format!("set(\"{}\"): {}", key, e)));
    });

    // `bind(keys, command)` binds globally, `bind(keymap, keys, command)` in any keymap
    // (a mode's is named after it); `keys` may be an array of sequences.
    let binds = |keymap: String, keys: &Dynamic, command: &str| {
        let bind = |keys| ConfigOp::Bind { keymap: keymap.clone(), keys, command: command.to_string() };
        key_list(keys).map(|list| list.into_iter().map(bind).collect())
    };
    let unbinds = |keymap: String, keys: &Dynamic| {
        key_list(keys)
            .map(|list| list.into_iter().map(|keys| ConfigOp::Unbind { keymap: keymap.clone(), keys }).collect())
    };
    let s = script.clone();
    engine.register_fn("bind", move |ctx: NativeCallContext, keys: Dynamic, command: &str| {
        push(&s, &ctx, binds("global".into(), &keys, command));
    });
    let s = script.clone();
    engine.register_fn("bind", move |ctx: NativeCallContext, keymap: &str, keys: Dynamic, command: &str| {
        push(&s, &ctx, binds(keymap_name(keymap), &keys, command));
    });
    let s = script.clone();
    engine.register_fn("unbind", move |ctx: NativeCallContext, keys: Dynamic| {
        push(&s, &ctx, unbinds("global".into(), &keys));
    });
    let s = script.clone();
    engine.register_fn("unbind", move |ctx: NativeCallContext, keymap: &str, keys: Dynamic| {
        push(&s, &ctx, unbinds(keymap_name(keymap), &keys));
    });

    let s = script.clone();
    engine.register_fn("face", move |ctx: NativeCallContext, name: &str, spec: Map| {
        let op = parse_face(&spec).map(|face| vec![ConfigOp::Face { name: name.to_string(), face }]);
        push(&s, &ctx, op.map_err(|e| format!("face(\"{}\"): {}", name, e)));
    });

    let s = script.clone();
    engine.register_fn("mode", move |ctx: NativeCallContext, name: &str, spec: Map| {
        let op = map_from_rhai(spec).map(|values| vec![ConfigOp::Mode { name: name.to_string(), values }]);
        push(&s, &ctx, op.map_err(|e| format!("mode(\"{}\"): {}", name, e)));
    });

    if let Err(mut e) = engine.run(source) {
        let line = e.take_position().line().unwrap_or(0);
        script.lock().errors.push((line, e.to_string()));
    }
    let recorded = std::mem::take(&mut *script.lock());
    recorded
}

/// Records the operations of the call `ctx`, or its error, under the call's line.
fn push(script: &Mutex<Script>, ctx: &NativeCallContext, ops: Result<Vec<ConfigOp>, String>) {
    let line = ctx.call_position().line().unwrap_or(0);
    let mut script = script.lock();
    match ops {
        Ok(ops) => script.ops.extend(ops.into_iter().map(|op| (line, op))),
        Err(e) => script.errors.push((line, e)),
    }
}

fn string(value: &Dynamic) -> Result<String, String> {
    value.clone().into_string().map_err(|t| format!("expected a string, got {}", t))
}

/// A key sequence, or an array of them.
fn key_list(keys: &Dynamic) -> Result<Vec<String>, String> {
    match keys.clone().try_cast::<rhai::Array>() {
        Some(list) => list.iter().map(string).collect(),
        None => string(keys).map(|keys| vec![keys]),
    }
}

/// Reads a face from a Rhai map with optional `fg`, `bg` (`"#rrggbb"`), `bold`, `italic`
/// and `underline` (`true`, or a `"#rrggbb"` underline color) entries.
fn parse_face(spec: &Map) -> Result<Face, String> {
    let color = |key: &str| -> Result<Option<Color>, String> {
        match spec.get(key) {
            None => Ok(None),
            Some(v) => {
                let s = v.clone().into_string().map_err(|_| format!("'{}' must be a string", key))?;
                Color::parse_hex(&s).map(Some).ok_or_else(|| format!("'{}' is not a #rrggbb color", key))
            }
        }
    };
    let flag = |key: &str| spec.get(key).and_then(|v| v.as_bool().ok()).unwrap_or(false);
    for key in spec.keys() {
        if !["fg", "bg", "bold", "italic", "underline"].contains(&key.as_str()) {
            return Err(format!("unknown face attribute '{}'", key));
        }
    }
    let underline_color = match spec.get("underline") {
        Some(v) if v.is_string() => color("underline")?,
        _ => None,
    };
    Ok(Face {
        fg: color("fg")?,
        bg: color("bg")?,
        bold: flag("bold"),
        italic: flag("italic"),
        underline: flag("underline") || underline_color.is_some(),
        underline_color,
    })
}
