//! Keymaps: named tries from key sequences to command bindings.
//!
//! Lookup walks a stack of layers (top modal, else the buffer's minor maps, its mode's map
//! and global). Each layer may have a parent chain; an opaque layer (every modal) stops the
//! search so unbound keys are swallowed instead of leaking into the buffer underneath.
//!
//! An unbound shifted key is retried without Shift, as in Emacs (`S-<right>` reaches
//! `<right>`). Keys are otherwise exact: `C-c c` and `C-c C-c` are separate bindings.

use std::collections::HashMap;

use crate::command::{Arg, CommandId};
use crate::key::Key;

#[derive(Debug, Clone, PartialEq)]
pub struct Binding {
    pub command: CommandId,
    pub arg: Arg,
}

#[derive(Debug, Clone)]
enum Node {
    Bind(Binding),
    Prefix(HashMap<Key, Node>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeymapId(u16);

/// Keymaps every editor has, in id order: (name, parent, opaque).
const BUILTIN: &[(&str, Option<KeymapId>, bool)] = &[
    ("global", None, false),
    ("input", None, true),
    ("minibuffer", Some(KeymapId::INPUT), false),
    ("search", Some(KeymapId::INPUT), false),
    ("picker", Some(KeymapId::INPUT), false),
    ("choice", None, true),
    ("menu", None, true),
    ("undo-tree", None, true),
    ("jump", None, true),
    ("special", None, false),
    ("completion", None, true),
    ("tooltip", None, true),
];

impl KeymapId {
    pub const GLOBAL: KeymapId = KeymapId(0);
    /// Line editing shared by every modal with text input.
    pub const INPUT: KeymapId = KeymapId(1);
    pub const MINIBUFFER: KeymapId = KeymapId(2);
    pub const SEARCH: KeymapId = KeymapId(3);
    pub const PICKER: KeymapId = KeymapId(4);
    pub const CHOICE: KeymapId = KeymapId(5);
    pub const MENU: KeymapId = KeymapId(6);
    pub const UNDO_TREE: KeymapId = KeymapId(7);
    pub const JUMP: KeymapId = KeymapId(8);
    /// Keys shared by generated read-only buffers (`Mode::special`): help, quit, refresh
    /// and row motion.
    pub const SPECIAL: KeymapId = KeymapId(9);
    /// The completion popup: selects and accepts candidates while typing goes on in the
    /// buffer.
    pub const COMPLETION: KeymapId = KeymapId(10);
    pub const TOOLTIP: KeymapId = KeymapId(11);
}

#[derive(Clone)]
pub struct Keymap {
    pub name: String,
    root: HashMap<Key, Node>,
    pub parent: Option<KeymapId>,
    /// Invoked with `Arg::Char` for printable keys that nothing in this map binds.
    pub self_insert: Option<CommandId>,
    /// Stop the layer search here instead of falling through to lower layers.
    pub opaque: bool,
    /// Receives (as `Arg::Key`) every single key this map leaves unbound, printable or not
    /// (printable keys go to a `self_insert` in the chain first). Used for raw passthrough,
    /// e.g. a terminal sending keys to its program, and for search ending on keys it doesn't
    /// use. Multi-key sequences that turn out unbound are replayed to it key by key.
    pub fallback: Option<CommandId>,
    /// Keys the fallback doesn't take, so lower layers handle them (a terminal lets `C-x`
    /// and `M-x` reach the editor's global bindings).
    pub fallback_exempt: Vec<Key>,
}

enum Lookup<'a> {
    Bound(&'a Binding),
    Prefix,
    Missing,
}

impl Keymap {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            root: HashMap::new(),
            parent: None,
            self_insert: None,
            opaque: false,
            fallback: None,
            fallback_exempt: Vec::new(),
        }
    }

    /// Binds `seq`, replacing any binding or prefix previously at that position.
    pub fn bind(&mut self, seq: &[Key], binding: Binding) {
        let Some((last, prefix)) = seq.split_last() else {
            return;
        };
        let mut level = &mut self.root;
        for key in prefix {
            let node = level.entry(*key).or_insert_with(|| Node::Prefix(HashMap::new()));
            if let Node::Bind(_) = node {
                *node = Node::Prefix(HashMap::new());
            }
            let Node::Prefix(next) = node else { unreachable!() };
            level = next;
        }
        level.insert(*last, Node::Bind(binding));
    }

    pub fn unbind(&mut self, seq: &[Key]) {
        fn remove(level: &mut HashMap<Key, Node>, seq: &[Key]) {
            match seq {
                [] => {}
                [key] => {
                    level.remove(key);
                }
                [key, rest @ ..] => {
                    if let Some(Node::Prefix(next)) = level.get_mut(key) {
                        remove(next, rest);
                        if next.is_empty() {
                            level.remove(key);
                        }
                    }
                }
            }
        }
        remove(&mut self.root, seq);
    }

    fn lookup(&self, seq: &[Key]) -> Lookup<'_> {
        let mut level = &self.root;
        for (i, key) in seq.iter().enumerate() {
            match level.get(key) {
                None => return Lookup::Missing,
                Some(Node::Bind(b)) if i + 1 == seq.len() => return Lookup::Bound(b),
                Some(Node::Bind(_)) => return Lookup::Missing,
                Some(Node::Prefix(next)) => level = next,
            }
        }
        Lookup::Prefix
    }

    /// All bindings in this map (not its parents), flattened to full key sequences.
    pub fn bindings(&self) -> Vec<(Vec<Key>, &Binding)> {
        fn walk<'a>(level: &'a HashMap<Key, Node>, path: &mut Vec<Key>, out: &mut Vec<(Vec<Key>, &'a Binding)>) {
            for (key, node) in level {
                path.push(*key);
                match node {
                    Node::Bind(b) => out.push((path.clone(), b)),
                    Node::Prefix(next) => walk(next, path, out),
                }
                path.pop();
            }
        }
        let mut out = Vec::new();
        walk(&self.root, &mut Vec::new(), &mut out);
        out
    }
}

/// Outcome of resolving a key sequence against the active layers.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolved {
    /// `keymap` is the map that bound it (or supplied its self-insert / fallback command).
    Command {
        binding: Binding,
        keymap: KeymapId,
        shift_translated: bool,
    },
    /// The sequence continues; `key` is the last key as it matched (after shift
    /// translation), which is what the pending sequence should record.
    Prefix {
        key: Key,
    },
    Unbound,
}

#[derive(Clone)]
pub struct Keymaps {
    maps: Vec<Keymap>,
    by_name: HashMap<String, KeymapId>,
}

impl Default for Keymaps {
    fn default() -> Self {
        let mut keymaps = Self { maps: Vec::new(), by_name: HashMap::new() };
        for (i, &(name, parent, opaque)) in BUILTIN.iter().enumerate() {
            let id = keymaps.ensure(name);
            debug_assert_eq!(id, KeymapId(i as u16));
            let map = keymaps.get_mut(id);
            map.parent = parent;
            map.opaque = opaque;
        }
        keymaps
    }
}

impl Keymaps {
    /// Returns the keymap named `name`, creating it if needed.
    pub fn ensure(&mut self, name: &str) -> KeymapId {
        if let Some(&id) = self.by_name.get(name) {
            return id;
        }
        let id = KeymapId(self.maps.len() as u16);
        self.maps.push(Keymap::new(name));
        self.by_name.insert(name.to_string(), id);
        id
    }

    pub fn id(&self, name: &str) -> Option<KeymapId> {
        self.by_name.get(name).copied()
    }

    pub fn get(&self, id: KeymapId) -> &Keymap {
        &self.maps[id.0 as usize]
    }

    pub fn get_mut(&mut self, id: KeymapId) -> &mut Keymap {
        &mut self.maps[id.0 as usize]
    }

    pub fn iter(&self) -> impl Iterator<Item = (KeymapId, &Keymap)> {
        self.maps.iter().enumerate().map(|(i, m)| (KeymapId(i as u16), m))
    }

    /// Restores the bindings `snapshot` had, keeping maps created since.
    pub fn restore(&mut self, snapshot: &Keymaps) {
        self.maps[..snapshot.maps.len()].clone_from_slice(&snapshot.maps);
    }

    /// A map followed by its parent chain.
    pub fn chain(&self, id: KeymapId) -> impl Iterator<Item = &Keymap> {
        std::iter::successors(Some(self.get(id)), |m| m.parent.map(|p| self.get(p)))
    }

    /// The fallback command of `layer` or its parents.
    pub fn fallback(&self, layer: KeymapId) -> Option<CommandId> {
        self.chain(layer).find_map(|m| m.fallback)
    }

    fn is_fallback_exempt(&self, layer: KeymapId, key: &Key) -> bool {
        self.chain(layer).any(|m| m.fallback_exempt.contains(key))
    }

    /// The explicit binding of `seq` in `layer` or its parents (no self-insert or fallback).
    pub fn bound(&self, layer: KeymapId, seq: &[Key]) -> Option<Binding> {
        self.chain(layer).find_map(|m| match m.lookup(seq) {
            Lookup::Bound(b) => Some(b.clone()),
            _ => None,
        })
    }

    /// Resolves `key` typed after `prefix` (keys already matched as a prefix).
    pub fn resolve(&self, layers: &[KeymapId], prefix: &[Key], key: Key) -> Resolved {
        let mut seq = Vec::with_capacity(prefix.len() + 1);
        seq.extend_from_slice(prefix);
        seq.push(key);
        let exact = self.resolve_exact(layers, &seq);
        if exact != Resolved::Unbound {
            return exact;
        }
        let Some(unshifted) = key.unshifted() else {
            return Resolved::Unbound;
        };
        *seq.last_mut().expect("seq holds key") = unshifted;
        match self.resolve_exact(layers, &seq) {
            Resolved::Command { binding, keymap, .. } => Resolved::Command { binding, keymap, shift_translated: true },
            other => other,
        }
    }

    fn resolve_exact(&self, layers: &[KeymapId], seq: &[Key]) -> Resolved {
        let command = |binding, keymap| Resolved::Command { binding, keymap, shift_translated: false };
        let last = *seq.last().expect("non-empty sequence");
        for &layer in layers {
            for (id, map) in self.chain_ids(layer) {
                match map.lookup(seq) {
                    Lookup::Bound(b) => return command(b.clone(), id),
                    Lookup::Prefix => return Resolved::Prefix { key: last },
                    Lookup::Missing => {}
                }
            }
            if let [key] = seq {
                if let Some(ch) = key.printable() {
                    if let Some((id, cmd)) = self.chain_ids(layer).find_map(|(id, m)| Some((id, m.self_insert?))) {
                        return command(Binding { command: cmd, arg: Arg::Char(ch) }, id);
                    }
                }
                if !self.is_fallback_exempt(layer, key) {
                    if let Some((id, cmd)) = self.chain_ids(layer).find_map(|(id, m)| Some((id, m.fallback?))) {
                        return command(Binding { command: cmd, arg: Arg::Key(*key) }, id);
                    }
                }
            }
            if self.chain(layer).any(|m| m.opaque) {
                return Resolved::Unbound;
            }
        }
        Resolved::Unbound
    }

    fn chain_ids(&self, id: KeymapId) -> impl Iterator<Item = (KeymapId, &Keymap)> {
        std::iter::successors(Some(id), |&id| self.get(id).parent).map(|id| (id, self.get(id)))
    }
}
