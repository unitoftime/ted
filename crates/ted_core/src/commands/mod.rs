//! Built-in commands, grouped by area. Each module registers its commands through the same
//! `Commands` registry plugins use; `bindings` then installs the default keys.

pub mod bindings;
pub mod edit;
pub mod files;
pub mod help;
pub mod jump;
pub mod modal;
pub mod motion;
pub mod search;
pub mod undo_tree;
pub mod windows;

use crate::doc::Doc;
use crate::editor::Editor;

pub(crate) fn register_builtin(ed: &mut Editor) {
    motion::register(ed);
    edit::register(ed);
    files::register(ed);
    windows::register(ed);
    search::register(ed);
    help::register(ed);
    jump::register(ed);
    modal::register(ed);
    undo_tree::register(ed);
    crate::rows::register(ed);
    crate::locations::register(ed);
    crate::xref::register(ed);
}

/// Runs a cursor motion with shift-selection semantics: a shift-translated key extends the
/// selection, an unshifted one ends a shift selection.
pub fn motion(ed: &mut Editor, f: impl FnOnce(&mut Doc)) {
    let shift = ed.shift_translated();
    let mut doc = ed.focused_doc();
    doc.begin_motion(shift);
    f(&mut doc);
}

/// Runs an edit on the focused doc, refusing (with a message) in read-only buffers.
pub fn edit<R>(ed: &mut Editor, f: impl FnOnce(&mut Doc) -> R) -> Option<R> {
    let mut doc = ed.focused_doc();
    if !doc.buf.is_read_only() {
        return Some(f(&mut doc));
    }
    ed.set_status("Buffer is read-only");
    None
}
