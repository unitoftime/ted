//! The editable text of every modal that takes typed input: a one-line `Buffer` with its
//! own cursor and mark, so the regular motion, editing, kill/yank and undo commands work
//! in it exactly as in a window (see `Editor::focused_doc`).

use std::ops::Range;

use crate::buffer::Buffer;
use crate::doc::Doc;
use crate::view::View;

pub struct LineInput {
    buf: Buffer,
    view: View,
    /// The buffer text as of the last `set` or `sync`, for cheap `&str` access.
    text: String,
    synced_version: u64,
}

impl Default for LineInput {
    fn default() -> Self {
        Self::new("")
    }
}

impl LineInput {
    /// An input holding `text`, with the cursor at its end.
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let buf = Buffer::new("*minibuffer*", &text);
        let mut view = View::detached();
        view.cursor.pos = buf.len_chars();
        Self { synced_version: buf.version(), buf, view, text }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.view.cursor.pos
    }

    pub fn region(&self) -> Option<Range<usize>> {
        self.view.cursor.region()
    }

    /// Replaces the text as one undo step and moves the cursor to its end. Programmatic, so
    /// the modal is not told (`sync` does not report it).
    pub fn set(&mut self, text: impl Into<String>) {
        let text = text.into();
        let unchanged = text == self.text;
        let mut doc = self.doc();
        doc.clear_mark();
        if unchanged {
            let end = doc.buf.len_chars();
            doc.set_cursor(end);
            return;
        }
        let all = 0..doc.buf.len_chars();
        doc.replace_range(all, &text);
        self.text = text;
        self.synced_version = self.buf.version();
    }

    /// The input as a `Doc`, for the shared motion and editing operations.
    pub fn doc(&mut self) -> Doc<'_> {
        Doc::new(&mut self.view, &mut self.buf)
    }

    /// Picks up edits made through `doc` since the last sync. Returns whether the text changed.
    pub(crate) fn sync(&mut self) -> bool {
        if self.buf.version() == self.synced_version {
            return false;
        }
        self.synced_version = self.buf.version();
        let text = self.buf.text().to_string();
        let changed = text != self.text;
        self.text = text;
        changed
    }

    pub(crate) fn break_undo_chain(&mut self) {
        self.buf.break_undo_chain();
    }
}
