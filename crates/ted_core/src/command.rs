//! The command registry. Every user-invocable operation — built-in or plugin — is a named
//! command here; keymaps, M-x and help all refer to commands by `CommandId`.

use std::collections::HashMap;
use std::rc::Rc;

use crate::editor::Editor;
use crate::key::Key;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommandId(u32);

/// Argument carried by a binding (`C-c 1` -> `layout-restore 1`) or supplied by the
/// dispatcher (self-insert receives the typed char).
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Arg {
    #[default]
    None,
    Int(i64),
    Char(char),
    Str(Rc<str>),
    /// The key that triggered a keymap's `fallback` command.
    Key(Key),
    /// Where a mouse key happened, in frame pixels.
    Point(f32, f32),
}

impl Arg {
    /// Parses a config-file argument: integers become `Int`, anything else `Str`.
    pub fn parse(s: &str) -> Arg {
        match s.parse::<i64>() {
            Ok(n) => Arg::Int(n),
            Err(_) => Arg::Str(s.into()),
        }
    }

    pub fn int(&self) -> Option<i64> {
        match self {
            Arg::Int(n) => Some(*n),
            _ => None,
        }
    }

    pub fn char(&self) -> Option<char> {
        match self {
            Arg::Char(c) => Some(*c),
            Arg::Str(s) if s.chars().count() == 1 => s.chars().next(),
            _ => None,
        }
    }

    pub fn key(&self) -> Option<Key> {
        match self {
            Arg::Key(k) => Some(*k),
            _ => None,
        }
    }

    pub fn point(&self) -> Option<(f32, f32)> {
        match self {
            Arg::Point(x, y) => Some((*x, *y)),
            _ => None,
        }
    }

    pub fn str(&self) -> Option<&str> {
        match self {
            Arg::Str(s) => Some(s),
            _ => None,
        }
    }
}

pub type CommandFn = Rc<dyn Fn(&mut Editor, &Arg)>;

pub struct Command {
    pub name: String,
    pub doc: String,
    /// Hidden commands only make sense inside a modal (minibuffer editing, picker
    /// navigation) and are left out of M-x.
    pub hidden: bool,
    run: CommandFn,
}

#[derive(Default)]
pub struct Commands {
    list: Vec<Command>,
    by_name: HashMap<String, CommandId>,
}

impl Commands {
    /// Registers a command. Re-registering a name replaces its handler but keeps its id,
    /// so existing bindings follow the override.
    pub fn register(&mut self, name: &str, doc: &str, run: impl Fn(&mut Editor, &Arg) + 'static) -> CommandId {
        self.insert(name, doc, false, Rc::new(run))
    }

    pub fn register_hidden(&mut self, name: &str, doc: &str, run: impl Fn(&mut Editor, &Arg) + 'static) -> CommandId {
        self.insert(name, doc, true, Rc::new(run))
    }

    fn insert(&mut self, name: &str, doc: &str, hidden: bool, run: CommandFn) -> CommandId {
        let cmd = Command { name: name.to_string(), doc: doc.to_string(), hidden, run };
        if let Some(&id) = self.by_name.get(name) {
            self.list[id.0 as usize] = cmd;
            return id;
        }
        let id = CommandId(self.list.len() as u32);
        self.list.push(cmd);
        self.by_name.insert(name.to_string(), id);
        id
    }

    pub fn id(&self, name: &str) -> Option<CommandId> {
        self.by_name.get(name).copied()
    }

    pub fn get(&self, id: CommandId) -> &Command {
        &self.list[id.0 as usize]
    }

    pub fn iter(&self) -> impl Iterator<Item = (CommandId, &Command)> {
        self.list.iter().enumerate().map(|(i, c)| (CommandId(i as u32), c))
    }

    pub(crate) fn handler(&self, id: CommandId) -> CommandFn {
        self.list[id.0 as usize].run.clone()
    }
}
