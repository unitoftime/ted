//! Text editing: insertion, deletion, the kill ring, undo and comments.

use crate::commands::edit;
use crate::doc::Doc;
use crate::editor::{Editor, UndoChain};
use crate::kill_ring::KillMode;

/// Chain value of a kill, so the next kill appends to the same kill-ring entry.
struct Killed;

/// Chain value of a yank: the inserted range and its kill-ring entry, for `yank-pop`.
#[derive(Clone)]
struct Yanked {
    range: std::ops::Range<usize>,
    ring_index: usize,
}

pub fn register(ed: &mut Editor) {
    let c = &mut ed.commands;
    c.register_hidden("self-insert-command", "Insert the typed character", |ed, arg| {
        if let Some(ch) = arg.char() {
            edit(ed, |d| d.insert_char(ch));
        }
    });
    c.register("newline", "Insert a newline, keeping indentation", |ed, _| {
        edit(ed, |d| d.newline());
    });
    c.register("open-line", "Insert a newline after point without moving", |ed, _| {
        edit(ed, |d| d.open_line());
    });
    c.register("indent-for-tab-command", "Reindent the line or region to fit the code around it", |ed, _| {
        edit(ed, |d| d.indent_for_tab());
    });
    c.register("outdent", "Remove one level of indentation from the line or region", |ed, _| {
        edit(ed, |d| d.outdent());
    });
    c.register("delete-backward-char", "Delete the character before point", |ed, _| {
        edit(ed, |d| d.backspace());
    });
    c.register("delete-char", "Delete the character under point", |ed, _| {
        edit(ed, |d| d.delete_forward());
    });

    c.register("kill-line", "Cut to the end of the line into the kill ring", |ed, _| {
        kill(ed, KillMode::Append, |d| d.kill_line());
    });
    c.register("kill-word", "Cut the word after point", |ed, _| {
        kill(ed, KillMode::Append, |d| d.kill_word_forward());
    });
    c.register("backward-kill-word", "Cut the word before point", |ed, _| {
        kill(ed, KillMode::Prepend, |d| d.kill_word_backward());
    });
    c.register("kill-region", "Cut the selected region into the kill ring", |ed, _| {
        let killed = kill(ed, KillMode::Append, |d| d.kill_region());
        ed.set_status(if killed { "Killed region" } else { "The mark is not active now" });
    });
    c.register("copy-region", "Copy the selected region into the kill ring", |ed, _| {
        match ed.focused_doc().copy_region() {
            Some(text) => {
                ed.kill_ring.push(text, KillMode::New);
                ed.set_status("Copied region");
            }
            None => ed.set_status("The mark is not active now"),
        }
    });

    c.register("yank", "Paste the most recent kill", |ed, _| {
        ed.kill_ring.sync_from_system_clipboard();
        let Some(text) = ed.kill_ring.current().map(str::to_string) else {
            return;
        };
        if let Some(range) = edit(ed, |d| d.insert_text(&text)) {
            ed.set_chain(Yanked { range, ring_index: 0 });
        }
    });
    c.register("yank-pop", "Replace the just-yanked text with an older kill", |ed, _| {
        let Some(Yanked { range, ring_index }) = ed.last_chain::<Yanked>().cloned() else {
            ed.set_status("Previous command was not a yank");
            return;
        };
        let len = ed.kill_ring.len();
        let next = (ring_index + 1) % len.max(1);
        let Some(text) = ed.kill_ring.peek(next).map(str::to_string) else {
            return;
        };
        if let Some(range) = edit(ed, |d| d.replace_range(range, &text)) {
            ed.set_chain(Yanked { range, ring_index: next });
        }
    });

    c.register("undo", "Undo the last change (repeat to keep undoing)", |ed, _| {
        match edit(ed, |d| d.undo()) {
            Some(true) => report_revision(ed, "Undo"),
            Some(false) => ed.set_status("Already at oldest change"),
            None => {}
        }
        ed.set_chain(UndoChain);
    });
    c.register("redo", "Redo the last undone change", |ed, _| {
        match edit(ed, |d| d.redo()) {
            Some(true) => report_revision(ed, "Redo"),
            Some(false) => ed.set_status("Already at newest change"),
            None => {}
        }
        ed.set_chain(UndoChain);
    });

    c.register("comment-region", "Comment or uncomment the region's lines", |ed, _| {
        edit(ed, |d| d.comment_region());
    });
    c.register("keyboard-quit", "Cancel the current operation and deactivate the mark", |ed, _| {
        let mut doc = ed.doc();
        doc.clear_mark();
        doc.buf.end_edit_group();
        ed.set_status("Quit");
    });
}

/// Runs a killing edit and files the text in the kill ring, merging with an immediately
/// preceding kill. Returns whether anything was killed.
fn kill(ed: &mut Editor, merge: KillMode, f: impl FnOnce(&mut Doc) -> Option<String>) -> bool {
    let mode = if ed.last_chain::<Killed>().is_some() { merge } else { KillMode::New };
    let killed = edit(ed, f).flatten();
    let did_kill = killed.is_some();
    if let Some(text) = killed {
        ed.kill_ring.push(text, mode);
    }
    ed.set_chain(Killed);
    did_kill
}

pub(crate) fn report_revision(ed: &mut Editor, what: &str) {
    let doc = ed.focused_doc();
    let history = doc.buf.history();
    let msg = format!("{} (revision {}/{})", what, history.current_id, history.nodes.len().saturating_sub(1));
    ed.set_status(msg);
}
