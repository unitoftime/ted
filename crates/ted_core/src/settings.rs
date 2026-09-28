//! Settings: named, typed, documented values, like faces and commands.
//!
//! Core settings have fixed handles (`settings::WRAP_LINES`); plugins `define` their own and
//! keep the returned handle. Reading through a handle is an index plus a type-checked match.
//! `init.rhai` and `M-x set-setting` change settings by name, and `Editor::watch_setting`
//! runs code when one changes (the theme reapplies faces, `wrap_lines` rewraps windows).
//!
//! Any setting can also have a value in a mode, which its buffers use instead: a mode
//! definition sets the language's own (`Mode::set`, e.g. tabs in Makefiles), and
//! `init.rhai` the user's (`mode("go", #{ format_on_save: true })`). Code reading a
//! setting on behalf of a buffer uses `Settings::get_in` with the buffer's mode.

use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::rc::Rc;

use crate::editor::Editor;
use crate::mode::Mode;

/// Named values of a map setting, e.g. a language server's configuration.
pub type Map = BTreeMap<String, Value>;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Value>),
    Map(Map),
}

impl Value {
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Bool(_) => "bool",
            Value::Int(_) => "integer",
            Value::Float(_) => "number",
            Value::Str(_) => "string",
            Value::List(_) => "list",
            Value::Map(_) => "map",
        }
    }

    /// `self` converted to the kind of `like`, if they are compatible (integers are
    /// accepted for numbers).
    fn coerce_to(&self, like: &Value) -> Option<Value> {
        match (like, self) {
            (Value::Float(_), Value::Int(i)) => Some(Value::Float(*i as f64)),
            (a, b) if std::mem::discriminant(a) == std::mem::discriminant(b) => Some(b.clone()),
            _ => None,
        }
    }

    /// Parses user input (`M-x set-setting`) as a value of the same kind as `like`. Lists
    /// and maps are only set from `init.rhai`.
    pub fn parse_like(text: &str, like: &Value) -> Option<Value> {
        let text = text.trim();
        match like {
            Value::Bool(_) => match text {
                "true" | "t" | "yes" | "on" => Some(Value::Bool(true)),
                "false" | "nil" | "no" | "off" => Some(Value::Bool(false)),
                _ => None,
            },
            Value::Int(_) => text.parse().ok().map(Value::Int),
            Value::Float(_) => text.parse().ok().map(Value::Float),
            Value::Str(_) => Some(Value::Str(text.trim_matches('"').to_string())),
            Value::List(_) | Value::Map(_) => None,
        }
    }
}

/// Values print as `init.rhai` writes them, so help can show the line that sets one.
impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Bool(b) => write!(f, "{}", b),
            Value::Int(i) => write!(f, "{}", i),
            Value::Float(x) => write!(f, "{:?}", x),
            Value::Str(s) => write!(f, "{:?}", s),
            Value::List(items) => {
                write!(f, "[")?;
                for (i, item) in items.iter().enumerate() {
                    write!(f, "{}{}", if i > 0 { ", " } else { "" }, item)?;
                }
                write!(f, "]")
            }
            Value::Map(map) if map.is_empty() => write!(f, "#{{}}"),
            Value::Map(map) => {
                write!(f, "#{{ ")?;
                for (i, (key, value)) in map.iter().enumerate() {
                    let plain = key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                    let sep = if i > 0 { ", " } else { "" };
                    if plain {
                        write!(f, "{}{}: {}", sep, key, value)?;
                    } else {
                        write!(f, "{}{:?}: {}", sep, key, value)?;
                    }
                }
                write!(f, " }}")
            }
        }
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

impl From<i64> for Value {
    fn from(i: i64) -> Self {
        Value::Int(i)
    }
}

impl From<f64> for Value {
    fn from(x: f64) -> Self {
        Value::Float(x)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Str(s.to_string())
    }
}

impl From<Map> for Value {
    fn from(map: Map) -> Self {
        Value::Map(map)
    }
}

/// A Rust type a setting can hold; `Ref` is what reading it returns.
pub trait SettingType: 'static {
    type Ref<'a>;
    fn read(value: &Value) -> Self::Ref<'_>;
}

impl SettingType for bool {
    type Ref<'a> = bool;
    fn read(value: &Value) -> bool {
        matches!(value, Value::Bool(true))
    }
}

impl SettingType for i64 {
    type Ref<'a> = i64;
    fn read(value: &Value) -> i64 {
        match value {
            Value::Int(i) => *i,
            _ => 0,
        }
    }
}

impl SettingType for f64 {
    type Ref<'a> = f64;
    fn read(value: &Value) -> f64 {
        match value {
            Value::Float(x) => *x,
            _ => 0.0,
        }
    }
}

impl SettingType for String {
    type Ref<'a> = &'a str;
    fn read(value: &Value) -> &str {
        match value {
            Value::Str(s) => s,
            _ => "",
        }
    }
}

impl SettingType for Map {
    type Ref<'a> = &'a Map;
    fn read(value: &Value) -> &Map {
        static EMPTY: Map = BTreeMap::new();
        match value {
            Value::Map(map) => map,
            _ => &EMPTY,
        }
    }
}

/// A typed handle to a setting.
pub struct Setting<T> {
    index: u16,
    _type: PhantomData<fn() -> T>,
}

impl<T> Clone for Setting<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Setting<T> {}

impl<T> Setting<T> {
    const fn at(index: u16) -> Self {
        Self { index, _type: PhantomData }
    }

    pub(crate) fn index(self) -> u16 {
        self.index
    }
}

pub struct Entry {
    pub name: String,
    pub doc: String,
    pub default: Value,
    pub value: Value,
    /// Accepted values of a string setting, when it is an enumeration.
    pub choices: &'static [&'static str],
}

impl Entry {
    fn accept(&self, value: &Value) -> Result<Value, String> {
        let invalid = || format!("Invalid value {} for '{}' (expected {})", value, self.name, self.expected());
        let value = value.coerce_to(&self.default).ok_or_else(invalid)?;
        match &value {
            Value::Str(s) if !self.choices.is_empty() && !self.choices.contains(&s.as_str()) => Err(invalid()),
            _ => Ok(value),
        }
    }

    /// The kind of value accepted, e.g. `integer` or `"box" | "line"`.
    pub fn expected(&self) -> String {
        if self.choices.is_empty() {
            return self.default.kind().to_string();
        }
        self.choices.iter().map(|c| format!("{:?}", c)).collect::<Vec<_>>().join(" | ")
    }
}

type Watcher = Rc<dyn Fn(&mut Editor)>;

pub struct Settings {
    entries: Vec<Entry>,
    watchers: Vec<(u16, Watcher)>,
}

macro_rules! builtin_settings {
    ($($id:ident: $ty:ty = $name:literal, $default:expr, $choices:expr, $doc:literal;)*) => {
        #[allow(non_camel_case_types, clippy::upper_case_acronyms)]
        #[repr(u16)]
        enum Builtin { $($id,)* }

        $(pub const $id: Setting<$ty> = Setting::at(Builtin::$id as u16);)*

        fn builtin_entries() -> Vec<Entry> {
            vec![$(Entry {
                name: $name.to_string(),
                doc: $doc.to_string(),
                default: Value::from($default),
                value: Value::from($default),
                choices: $choices,
            },)*]
        }
    };
}

builtin_settings! {
    THEME: String = "theme", "tango-dark", crate::theme::NAMES, "Color theme (M-x toggle-theme cycles them)";
    WRAP_LINES: bool = "wrap_lines", true, &[], "Wrap long lines at the window edge";
    FONT_SIZE: f64 = "font_size", 15.0, &[], "Font size in pixels, before zooming (C-= / C-- / C-0)";
    LINE_HEIGHT: f64 = "line_height", 22.0, &[], "Line height in pixels, before zooming (scales with the font)";
    CURSOR_SHAPE: String = "cursor_shape", "box", &["box", "line", "underline"], "Shape of the text cursor";
    CURSOR_WIDTH: f64 = "cursor_width", 2.0, &[], "Width of the line cursor in pixels";
    SPLIT_SEPARATOR_SIZE: f64 = "split_separator_size", 2.0, &[], "Width of the divider between windows";
    TAB_WIDTH: i64 = "tab_width", 4_i64, &[], "Indentation width (from 1 to 16)";
    INDENT: String = "indent", "spaces", &["spaces", "tabs"], "What indenting inserts";
}

impl Default for Settings {
    fn default() -> Self {
        Self { entries: builtin_entries(), watchers: Vec::new() }
    }
}

impl Settings {
    /// Declares a setting (or returns the existing one of that name). `init.rhai` can then
    /// `set` it to values of the same type.
    pub fn define<T: SettingType>(&mut self, name: &str, default: impl Into<Value>, doc: &str) -> Setting<T> {
        self.define_entry(name, default.into(), &[], doc)
    }

    /// Declares a string setting that only accepts one of `choices`.
    pub fn define_choice(
        &mut self,
        name: &str,
        default: &str,
        choices: &'static [&'static str],
        doc: &str,
    ) -> Setting<String> {
        debug_assert!(choices.contains(&default));
        self.define_entry(name, Value::from(default), choices, doc)
    }

    fn define_entry<T>(
        &mut self,
        name: &str,
        default: Value,
        choices: &'static [&'static str],
        doc: &str,
    ) -> Setting<T> {
        if let Some(index) = self.index(name) {
            return Setting::at(index as u16);
        }
        let value = default.clone();
        self.entries.push(Entry { name: name.to_string(), doc: doc.to_string(), default, value, choices });
        Setting::at((self.entries.len() - 1) as u16)
    }

    pub fn get<T: SettingType>(&self, setting: Setting<T>) -> T::Ref<'_> {
        T::read(&self.entries[setting.index as usize].value)
    }

    /// The value of `setting` in buffers of `mode`: the mode's own if it has one.
    pub fn get_in<'a, T: SettingType>(&'a self, setting: Setting<T>, mode: &'a Mode) -> T::Ref<'a> {
        match mode.value(setting.index) {
            Some(value) => T::read(value),
            None => self.get(setting),
        }
    }

    fn index(&self, name: &str) -> Option<usize> {
        self.entries.iter().position(|e| e.name == name)
    }

    /// Checks `value` for setting `name`, returning the setting's index and the value as
    /// stored (an integer given for a number becomes one).
    pub(crate) fn accept(&self, name: &str, value: &Value) -> Result<(u16, Value), String> {
        let index = self.index(name).ok_or_else(|| format!("Unknown setting '{}'", name))?;
        Ok((index as u16, self.entries[index].accept(value)?))
    }

    pub(crate) fn name(&self, index: u16) -> &str {
        &self.entries[index as usize].name
    }

    pub fn entry(&self, name: &str) -> Option<&Entry> {
        self.entries.get(self.index(name)?)
    }

    pub(crate) fn entry_index(&self, name: &str) -> Option<(u16, &Entry)> {
        let index = self.index(name)?;
        Some((index as u16, &self.entries[index]))
    }

    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter()
    }

    /// Sets `name` without running watchers; `Editor::set_setting` is the usual way in.
    /// Returns the setting's index when the value was accepted.
    pub(crate) fn set(&mut self, name: &str, value: &Value) -> Result<u16, String> {
        let (index, value) = self.accept(name, value)?;
        self.entries[index as usize].value = value;
        Ok(index)
    }

    pub(crate) fn values(&self) -> Vec<Value> {
        self.entries.iter().map(|e| e.value.clone()).collect()
    }

    /// Restores values saved by `values`, returning the indices of the settings that changed.
    pub(crate) fn restore(&mut self, values: &[Value]) -> Vec<u16> {
        let mut changed = Vec::new();
        for (index, (entry, value)) in self.entries.iter_mut().zip(values).enumerate() {
            if entry.value != *value {
                entry.value = value.clone();
                changed.push(index as u16);
            }
        }
        changed
    }
}

impl Editor {
    /// Changes setting `name` and runs its watchers.
    pub fn set_setting(&mut self, name: &str, value: &Value) -> Result<(), String> {
        let index = self.settings.set(name, value)?;
        self.run_setting_watchers(index);
        Ok(())
    }

    pub(crate) fn run_setting_watchers(&mut self, index: u16) {
        let watchers: Vec<Watcher> =
            self.settings.watchers.iter().filter(|(i, _)| *i == index).map(|(_, w)| w.clone()).collect();
        for watcher in watchers {
            watcher(self);
        }
    }

    /// Changes setting `name` in buffers of mode `mode` only, and runs its watchers. A
    /// plugin calling this from `init` sets the mode's default, as `Mode::set` does.
    pub fn set_mode_setting(&mut self, mode: &str, name: &str, value: &Value) -> Result<(), String> {
        let (index, value) = self.settings.accept(name, value)?;
        self.update_mode(mode, |mode| {
            mode.set_value(index, value);
            Ok(())
        })?;
        self.run_setting_watchers(index);
        Ok(())
    }

    /// Runs `f` whenever `setting` changes.
    pub fn watch_setting<T>(&mut self, setting: Setting<T>, f: impl Fn(&mut Editor) + 'static) {
        self.settings.watchers.push((setting.index, Rc::new(f)));
    }
}
