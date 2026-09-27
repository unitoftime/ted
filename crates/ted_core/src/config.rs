//! The user's `init.rhai` script.
//!
//! The script runs in a sandboxed Rhai engine and only *records* operations (`set`, `bind`,
//! `unbind`, ...). The editor applies them afterwards (`Editor::apply_config`), so scripts
//! never hold editor state and can be re-run at any time (`M-x reload-init`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use rhai::{Dynamic, Engine, Map};

use crate::face::Face;
use crate::frame::Color;
use crate::mode::{keymap_name, IndentStyle, ModeOverrides};
use crate::settings::Value;

fn value_from_dynamic(value: Dynamic) -> Result<Value, String> {
    if value.is_array() {
        let items = value.into_array().unwrap_or_default();
        items.into_iter().map(value_from_dynamic).collect::<Result<_, _>>().map(Value::List)
    } else if value.is_map() {
        let map = value.cast::<Map>();
        map.into_iter()
            .map(|(key, v)| Ok((key.to_string(), value_from_dynamic(v)?)))
            .collect::<Result<_, String>>()
            .map(Value::Map)
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
    /// Changes a mode's settings, e.g. `mode("rust", #{ tab_width: 2, indent: "tabs" })`.
    Mode {
        name: String,
        overrides: ModeOverrides,
    },
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

/// Runs `path` and returns the operations it recorded. A missing file yields no operations.
pub fn run_init_script(path: &Path) -> Result<Vec<ConfigOp>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let source = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    run_script(&source)
}

pub fn run_script(source: &str) -> Result<Vec<ConfigOp>, String> {
    let ops = Arc::new(Mutex::new(Vec::new()));
    let errors = Arc::new(Mutex::new(Vec::new()));
    let mut engine = Engine::new();

    let (o, e) = (ops.clone(), errors.clone());
    engine.register_fn("set", move |key: &str, value: Dynamic| match value_from_dynamic(value) {
        Ok(value) => o.lock().push(ConfigOp::Set { key: key.to_string(), value }),
        Err(err) => e.lock().push(format!("set(\"{}\"): {}", key, err)),
    });
    let o = ops.clone();
    engine.register_fn("bind", move |keys: &str, command: &str| {
        o.lock().push(ConfigOp::Bind { keymap: "global".into(), keys: keys.into(), command: command.into() });
    });
    let o = ops.clone();
    engine.register_fn("bind_mode", move |keymap: &str, keys: &str, command: &str| {
        o.lock().push(ConfigOp::Bind { keymap: keymap_name(keymap), keys: keys.into(), command: command.into() });
    });
    let o = ops.clone();
    engine.register_fn("unbind", move |keys: &str| {
        o.lock().push(ConfigOp::Unbind { keymap: "global".into(), keys: keys.into() });
    });
    let o = ops.clone();
    engine.register_fn("unbind_mode", move |keymap: &str, keys: &str| {
        o.lock().push(ConfigOp::Unbind { keymap: keymap_name(keymap), keys: keys.into() });
    });

    let (o, e) = (ops.clone(), errors.clone());
    engine.register_fn("face", move |name: &str, spec: Map| match parse_face(&spec) {
        Ok(face) => o.lock().push(ConfigOp::Face { name: name.to_string(), face }),
        Err(err) => e.lock().push(format!("face(\"{}\"): {}", name, err)),
    });

    let (o, e) = (ops.clone(), errors.clone());
    engine.register_fn("mode", move |name: &str, spec: Map| match parse_mode(&spec) {
        Ok(overrides) => o.lock().push(ConfigOp::Mode { name: name.to_string(), overrides }),
        Err(err) => e.lock().push(format!("mode(\"{}\"): {}", name, err)),
    });

    engine.run(source).map_err(|e| e.to_string())?;
    if let Some(err) = errors.lock().first() {
        return Err(err.clone());
    }
    let ops = std::mem::take(&mut *ops.lock());
    Ok(ops)
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

/// Reads mode overrides from a Rhai map with optional `tab_width` (integer), `indent`
/// (`"spaces"` or `"tabs"`), `comment` (line comment prefix), `extensions` (array of
/// file extensions, replacing the mode's own), `line_numbers` and `highlight_line` (bools).
fn parse_mode(spec: &Map) -> Result<ModeOverrides, String> {
    let mut overrides = ModeOverrides::default();
    for (key, value) in spec {
        let value = value.clone();
        match key.as_str() {
            "tab_width" => {
                let width = value.as_int().ok().filter(|w| (1..=16).contains(w));
                overrides.tab_width = Some(width.ok_or("'tab_width' must be an integer from 1 to 16")? as usize);
            }
            "indent" => {
                overrides.indent = Some(match value.into_string().as_deref() {
                    Ok("spaces") => IndentStyle::Spaces,
                    Ok("tabs") => IndentStyle::Tabs,
                    _ => return Err("'indent' must be \"spaces\" or \"tabs\"".to_string()),
                });
            }
            "comment" => overrides.comment = Some(value.into_string().map_err(|_| "'comment' must be a string")?),
            "extensions" => {
                let list = value
                    .into_typed_array::<rhai::ImmutableString>()
                    .map_err(|_| "'extensions' must be an array of strings")?;
                overrides.extensions = Some(list.iter().map(|e| e.trim_start_matches('.').to_string()).collect());
            }
            "line_numbers" => {
                overrides.line_numbers = Some(value.as_bool().map_err(|_| "'line_numbers' must be true or false")?)
            }
            "highlight_line" => {
                overrides.highlight_line = Some(value.as_bool().map_err(|_| "'highlight_line' must be true or false")?)
            }
            other => return Err(format!("unknown mode attribute '{}'", other)),
        }
    }
    Ok(overrides)
}
