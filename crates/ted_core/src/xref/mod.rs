//! Cross-references: `find-definition`, `find-references`, and a jump stack to hop back.
//!
//! The commands don't know where answers come from. A `Backend` (a language server,
//! tree-sitter tags) answers a `Query` asynchronously through the `Reply` it is handed.
//! Backends are tried in priority order: one that declines the query, fails, or finds
//! nothing hands it to the next, so tree-sitter covers for a server that is missing, still
//! indexing, or stumped.
//!
//! One definition is visited directly and several are offered in a picker; references fill
//! the `*xref*` location list, so `next-error` steps through them. Every jump pushes the
//! position it left onto the jump stack, which `xref-go-back` pops.

mod tags;

use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

use crate::buffer::{Buffer, BufferId};
use crate::editor::Editor;
use crate::locations::{self, ListItem, Location, Severity};
use crate::text::{collapse_tilde, trim_highlighted};
use crate::ui::PickerItem;

pub use tags::TagsBackend;

pub const LIST_BUFFER: &str = "*xref*";
const MAX_JUMPS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Definition,
    References,
}

/// What the user asked about: the symbol at `pos` in `buffer`.
#[derive(Debug, Clone)]
pub struct Query {
    pub kind: Kind,
    pub buffer: BufferId,
    pub pos: usize,
    pub symbol: String,
}

/// A found location, with the text of its line for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub location: Location,
    pub text: String,
}

pub trait Backend {
    fn name(&self) -> &str;

    /// Starts answering `query`, eventually calling `reply.send`. Returns false (without
    /// using `reply`) to decline, e.g. when no server handles the buffer.
    fn find(&self, ed: &mut Editor, query: &Query, reply: Reply) -> bool;
}

/// The way back to the query a backend is answering. It is plain data, so a backend can
/// carry it through a job thread and send it back with the answer.
#[derive(Debug)]
#[must_use = "a query stays pending until its reply is sent"]
pub struct Reply {
    serial: u64,
    /// Index of the backend to try next if this one comes up empty.
    next: usize,
}

impl Reply {
    /// Delivers a backend's answer. An error or an empty answer passes the query on to the
    /// next backend. Answers to queries the user has moved on from are dropped.
    pub fn send(self, ed: &mut Editor, result: Result<Vec<Item>, String>) {
        let pending = ed.ext_mut::<Xref>().pending.as_ref().filter(|p| p.serial == self.serial);
        let Some(query) = pending.map(|p| p.query.clone()) else {
            return;
        };
        let moved = ed.active_buffer_id() != query.buffer || ed.active_view().cursor.pos != query.pos;
        if moved || ed.has_modal() {
            ed.ext_mut::<Xref>().pending = None;
            return;
        }
        match result {
            Ok(items) if !items.is_empty() => {
                ed.ext_mut::<Xref>().pending = None;
                present(ed, &query, items);
            }
            Ok(_) => try_backends(ed, self.serial, self.next),
            Err(e) => {
                if let Some(pending) = &mut ed.ext_mut::<Xref>().pending {
                    pending.errors.push(e);
                }
                try_backends(ed, self.serial, self.next);
            }
        }
    }
}

/// A position to return to.
#[derive(Debug, Clone)]
struct Mark {
    buffer: BufferId,
    /// Reopens the file if the buffer was killed in the meantime.
    path: Option<PathBuf>,
    pos: usize,
}

struct Pending {
    serial: u64,
    query: Query,
    errors: Vec<String>,
}

#[derive(Default)]
struct Xref {
    /// Highest priority first.
    backends: Vec<(i32, Rc<dyn Backend>)>,
    pending: Option<Pending>,
    serial: u64,
    back: Vec<Mark>,
    forward: Vec<Mark>,
}

/// Adds a backend. Higher `priority` is asked first; tree-sitter tags sit at 0.
pub fn register_backend(ed: &mut Editor, priority: i32, backend: impl Backend + 'static) {
    let backends = &mut ed.ext_mut::<Xref>().backends;
    let at = backends.partition_point(|(p, _)| *p >= priority);
    backends.insert(at, (priority, Rc::new(backend)));
}

/// Remembers the active position on the jump stack, as every xref jump does. Other jumps
/// (plugins, custom commands) can use it to make themselves reversible with `M-,`.
pub fn push_mark(ed: &mut Editor) {
    let mark = current_mark(ed);
    let xref = ed.ext_mut::<Xref>();
    xref.forward.clear();
    xref.back.push(mark);
    if xref.back.len() > MAX_JUMPS {
        xref.back.remove(0);
    }
}

/// The identifier at `pos`, or just before it (point right after a word), as a char range
/// and its text.
pub fn symbol_at(buf: &Buffer, pos: usize) -> Option<(std::ops::Range<usize>, String)> {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let text = buf.text();
    let at = match buf.char_at(pos) {
        Some(c) if is_ident(c) => pos,
        _ if pos > 0 && buf.char_at(pos - 1).is_some_and(is_ident) => pos - 1,
        _ => return None,
    };
    let start = at - text.chars_at(at).reversed().take_while(|&c| is_ident(c)).count();
    let end = at + text.chars_at(at).take_while(|&c| is_ident(c)).count();
    Some((start..end, buf.slice_to_string(start..end)))
}

pub(crate) fn register(ed: &mut Editor) {
    let c = &mut ed.commands;
    c.register("find-definition", "Jump to the definition of the symbol at point", |ed, _| {
        start(ed, Kind::Definition);
    });
    c.register("find-references", "List the references to the symbol at point", |ed, _| {
        start(ed, Kind::References);
    });
    c.register("xref-go-back", "Return to where the last definition or reference jump started", |ed, _| {
        hop(ed, true);
    });
    c.register("xref-go-forward", "Undo an xref-go-back", |ed, _| {
        hop(ed, false);
    });
    register_backend(ed, 0, TagsBackend);
}

fn start(ed: &mut Editor, kind: Kind) {
    let buffer = ed.active_buffer_id();
    let pos = ed.active_view().cursor.pos;
    let Some((_, symbol)) = symbol_at(ed.active_buffer(), pos) else {
        ed.set_status("No symbol at point");
        return;
    };
    let xref = ed.ext_mut::<Xref>();
    xref.serial += 1;
    let serial = xref.serial;
    xref.pending =
        Some(Pending { serial, query: Query { kind, buffer, pos, symbol: symbol.clone() }, errors: Vec::new() });
    ed.set_status(match kind {
        Kind::Definition => format!("Finding definition of '{}'...", symbol),
        Kind::References => format!("Finding references to '{}'...", symbol),
    });
    try_backends(ed, serial, 0);
}

/// Hands pending query `serial` to the first backend from index `from` that accepts it;
/// reports failure when none is left.
fn try_backends(ed: &mut Editor, serial: u64, from: usize) {
    let xref = ed.ext_mut::<Xref>();
    let Some(query) = xref.pending.as_ref().filter(|p| p.serial == serial).map(|p| p.query.clone()) else {
        return;
    };
    let backends: Vec<_> = xref.backends.iter().skip(from).map(|(_, b)| b.clone()).collect();
    for (i, backend) in backends.into_iter().enumerate() {
        if backend.find(ed, &query, Reply { serial, next: from + i + 1 }) {
            return;
        }
    }
    let errors = ed.ext_mut::<Xref>().pending.take().map(|p| p.errors).unwrap_or_default();
    let what = match query.kind {
        Kind::Definition => "definition",
        Kind::References => "references",
    };
    let reason = errors.first().map(|e| format!(" ({})", e)).unwrap_or_default();
    ed.set_status(format!("No {} found for '{}'{}", what, query.symbol, reason));
}

fn present(ed: &mut Editor, query: &Query, mut items: Vec<Item>) {
    items.dedup_by(|a, b| a.location == b.location);
    let root = ed.buffers[query.buffer].project().root;
    match query.kind {
        Kind::Definition if items.len() == 1 => jump(ed, &items[0].location),
        Kind::Definition => {
            let entries = items
                .iter()
                .map(|item| {
                    let (line, highlights) = trim_highlighted(&item.text, &symbol_in(item, &query.symbol));
                    PickerItem::at(item.location.clone(), line, highlights)
                })
                .collect();
            let title = format!("Definitions of '{}'", query.symbol);
            ed.set_status(format!("{} definitions of '{}'", items.len(), query.symbol));
            ed.pick("xref", title, entries, move |ed, index| jump(ed, &items[index].location));
        }
        Kind::References => {
            let list: Vec<ListItem> = items
                .into_iter()
                .map(|item| {
                    let highlights = symbol_in(&item, &query.symbol);
                    ListItem { location: item.location, severity: Severity::Match, text: item.text, highlights }
                })
                .collect();
            let header = format!("References to '{}' in {} ({})", query.symbol, collapse_tilde(&root), list.len());
            push_mark(ed);
            let id = locations::fill_list(ed, LIST_BUFFER, locations::MODE, &header, &list);
            ed.show_buffer(id);
            ed.set_status(format!("{} references to '{}' (M-g n steps through them)", list.len(), query.symbol));
        }
    }
}

fn jump(ed: &mut Editor, location: &Location) {
    push_mark(ed);
    locations::visit(ed, location);
}

/// Where `symbol` is in `item`'s line, as a char range: at the item's column, else its
/// first occurrence (a backend's column can be off), else nowhere.
fn symbol_in(item: &Item, symbol: &str) -> Vec<Range<usize>> {
    let len = symbol.chars().count();
    let col = item.location.col;
    let start = match item.text.chars().skip(col).take(len).eq(symbol.chars()) {
        true => Some(col),
        false => item.text.find(symbol).map(|b| item.text[..b].chars().count()),
    };
    start.map(|start| start..start + len).into_iter().collect()
}

fn current_mark(ed: &Editor) -> Mark {
    let buf = ed.active_buffer();
    Mark { buffer: ed.active_buffer_id(), path: buf.path().map(PathBuf::from), pos: ed.active_view().cursor.pos }
}

/// Pops a mark off the back (or forward) stack and returns to it, recording the current
/// position on the other stack. Marks whose buffer and file are both gone are skipped.
fn hop(ed: &mut Editor, back: bool) {
    loop {
        let xref = ed.ext_mut::<Xref>();
        let stack = if back { &mut xref.back } else { &mut xref.forward };
        let Some(mark) = stack.pop() else {
            ed.set_status(if back { "Jump stack is empty" } else { "No newer jump" });
            return;
        };
        let here = current_mark(ed);
        if !show_mark(ed, &mark) {
            continue;
        }
        let xref = ed.ext_mut::<Xref>();
        let other = if back { &mut xref.forward } else { &mut xref.back };
        other.push(here);
        return;
    }
}

fn show_mark(ed: &mut Editor, mark: &Mark) -> bool {
    if ed.buffers.contains(mark.buffer) {
        if ed.active_buffer_id() != mark.buffer {
            ed.active_view_mut().set_buffer(mark.buffer);
        }
    } else if let Some(path) = &mark.path {
        if ed.open_file(path).is_err() {
            return false;
        }
    } else {
        return false;
    }
    let mut doc = ed.doc();
    let pos = mark.pos.min(doc.buf.len_chars());
    doc.clear_mark();
    doc.set_cursor(pos);
    doc.reveal();
    true
}
