//! Cross-references: `find-definition`, `find-references`, and a jump stack to hop back.
//!
//! The commands don't know where answers come from: a `Query` goes down a backend chain
//! (see `chain`), language servers first, then tree-sitter tags, so tree-sitter covers for
//! a server that is missing, still indexing, or stumped.
//!
//! One definition is visited directly and several are offered in a picker; references fill
//! the `xref` location list, so `next-error` steps through them. Every jump pushes the
//! position it left onto the jump stack, which `xref-go-back` pops.

mod tags;

use std::ops::Range;
use std::path::PathBuf;

use crate::buffer::{Buffer, BufferId};
use crate::chain::{self, Request};
use crate::editor::Editor;
use crate::locations::{self, ListItem, Location, Severity};
use crate::text::{collapse_tilde, is_ident_char, trim_highlighted};
use crate::ui::PickerItem;

pub use tags::TagsBackend;

/// The mode of the references list, a buffer named `xref: <symbol>`.
pub const MODE: &str = "Xref";
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

impl Request for Query {
    type Answer = Vec<Item>;

    /// Answers to queries the user has moved on from are dropped.
    fn is_current(&self, ed: &Editor) -> bool {
        ed.active_buffer_id() == self.buffer && ed.active_view().cursor.pos == self.pos && !ed.has_modal()
    }

    fn is_empty(items: &Vec<Item>) -> bool {
        items.is_empty()
    }

    fn answer(self, ed: &mut Editor, items: Vec<Item>) {
        present(ed, &self, items);
    }

    fn unanswered(self, ed: &mut Editor, errors: Vec<String>) {
        let what = match self.kind {
            Kind::Definition => "definition",
            Kind::References => "references",
        };
        let reason = errors.first().map(|e| format!(" ({})", e)).unwrap_or_default();
        ed.set_status(format!("No {} found for '{}'{}", what, self.symbol, reason));
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

#[derive(Default)]
struct Jumps {
    back: Vec<Mark>,
    forward: Vec<Mark>,
}

/// Remembers the active position on the jump stack, as every xref jump does. Other jumps
/// (plugins, custom commands) can use it to make themselves reversible with `M-,`.
pub fn push_mark(ed: &mut Editor) {
    let mark = current_mark(ed);
    let jumps = ed.ext_mut::<Jumps>();
    jumps.forward.clear();
    jumps.back.push(mark);
    if jumps.back.len() > MAX_JUMPS {
        jumps.back.remove(0);
    }
}

/// The identifier at `pos`, or just before it (point right after a word), as a char range
/// and its text.
pub fn symbol_at(buf: &Buffer, pos: usize) -> Option<(std::ops::Range<usize>, String)> {
    let text = buf.text();
    let at = match buf.char_at(pos) {
        Some(c) if is_ident_char(c) => pos,
        _ if pos > 0 && buf.char_at(pos - 1).is_some_and(is_ident_char) => pos - 1,
        _ => return None,
    };
    let start = at - text.chars_at(at).reversed().take_while(|&c| is_ident_char(c)).count();
    let end = at + text.chars_at(at).take_while(|&c| is_ident_char(c)).count();
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
    ed.define_mode(locations::list_mode(MODE));
    chain::register(ed, 0, TagsBackend);
}

fn start(ed: &mut Editor, kind: Kind) {
    let buffer = ed.active_buffer_id();
    let pos = ed.active_view().cursor.pos;
    let Some((_, symbol)) = symbol_at(ed.active_buffer(), pos) else {
        ed.set_status("No symbol at point");
        return;
    };
    ed.set_status(match kind {
        Kind::Definition => format!("Finding definition of '{}'...", symbol),
        Kind::References => format!("Finding references to '{}'...", symbol),
    });
    chain::ask(ed, Query { kind, buffer, pos, symbol });
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
            let name = format!("xref: {}", query.symbol);
            let id = locations::fill_list(ed, &name, MODE, &header, &list);
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
        let jumps = ed.ext_mut::<Jumps>();
        let stack = if back { &mut jumps.back } else { &mut jumps.forward };
        let Some(mark) = stack.pop() else {
            ed.set_status(if back { "Jump stack is empty" } else { "No newer jump" });
            return;
        };
        let here = current_mark(ed);
        if !show_mark(ed, &mark) {
            continue;
        }
        let jumps = ed.ext_mut::<Jumps>();
        let other = if back { &mut jumps.forward } else { &mut jumps.back };
        other.push(here);
        return;
    }
}

fn show_mark(ed: &mut Editor, mark: &Mark) -> bool {
    if ed.buffers.contains(mark.buffer) {
        if ed.active_buffer_id() != mark.buffer {
            ed.show_in_active_view(mark.buffer);
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
    doc.jump_to(pos);
    true
}
