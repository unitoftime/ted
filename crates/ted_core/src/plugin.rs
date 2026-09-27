//! The plugin API.
//!
//! A plugin is Rust code that extends the editor at startup through the same registries the
//! built-in features use, so everything it adds is rebindable, listed in M-x and described
//! by `describe-*`:
//!
//! ```ignore
//! pub struct Todo;
//!
//! impl Plugin for Todo {
//!     fn name(&self) -> &str { "todo" }
//!
//!     fn init(&mut self, ed: &mut Editor) {
//!         let file = ed.settings.define::<String>("todo.file", "~/todo.md", "The todo list");
//!         ed.commands.register("todo-open", "Open the todo list", move |ed, _| {
//!             let path = ed.settings.get(file).to_string();
//!             let _ = ed.open_file(ted_core::expand_tilde(path));
//!         });
//!         ed.commands.register("todo-list", "List open tasks", |ed, _| {
//!             let id = ed.special_buffer("*todo*", "Todo List");
//!             let mut text = StyledText::new();
//!             text.line(&[("Open tasks", Some(FaceId::HEADING))]);
//!             ed.buffers[id].set_styled("todo", text);
//!             ed.show_buffer(id);
//!         });
//!         // Special: read-only, with `h` (help), `n` / `p`, `g` (revert) and `q` (quit).
//!         ed.define_mode(Mode::new("Todo List").special().revert("todo-list"));
//!         // No global key: `C-c <key>` is the user's (`bind("C-c o", "todo-open")`).
//!     }
//! }
//! ```
//!
//! - `ed.commands.register(name, doc, |ed, arg| ..)` adds commands.
//! - `ed.define_mode(Mode::new(..))` adds a major mode and its keymap (named after the mode);
//!   `ed.bind_all(keymap, table)` binds defaults, which `init.rhai` can override. Global
//!   `C-c <key>` is left to the user (see `commands::bindings`).
//! - `ed.settings.define(name, default, doc)` declares settings; keep the handle and read
//!   with `ed.settings.get(handle)`; `ed.watch_setting` reacts to changes.
//! - `ed.faces.register(name, default)` declares faces themes can restyle.
//! - `ed.special_buffer` / `ed.new_buffer` make generated buffers; `StyledText` builds
//!   their content, `rows::RowText` when lines stand for items (point and marks then stay
//!   on their items across refreshes); `buffer.enable_keymap` layers a minor keymap on one
//!   buffer.
//! - `ed.hooks` subscribes to buffer and command events; `before_save` hooks can change a
//!   buffer before it is written (formatting) and hold the save until they are done.
//! - `ed.ext_mut::<T>()` / `ed.set_ext(..)` / `buffer.local_mut::<T>()` store plugin state
//!   per editor or buffer; `ed.set_chain` / `ed.last_chain` pass state to the next command.
//! - `ed.prompt / confirm / pick / push_modal` and `ui::Menu` / `ui::Tooltip` for UI;
//!   `ed.spawn` for background work, `ed.after` to run something later.
//! - `chain::register(ed, priority, backend)` answers definitions and references,
//!   completions or formatting for the buffers a plugin knows about (a language server).
//! - `ed.apply_edits` changes a buffer in many places as one undo step.

use std::rc::Rc;

use crate::buffer::BufferId;
use crate::editor::Editor;

pub trait Plugin: 'static {
    fn name(&self) -> &str;
    fn init(&mut self, editor: &mut Editor);
}

pub type BufferHook = Rc<dyn Fn(&mut Editor, BufferId)>;
pub type EditorHook = Rc<dyn Fn(&mut Editor)>;
/// Runs before a buffer is written, and may change it first. The save waits until the
/// hook calls `done` on the token, now or later (from a job's reply), but only so long:
/// a hook that takes too long is skipped.
pub type SaveHook = Rc<dyn Fn(&mut Editor, BufferId, SaveToken)>;

/// Lets a save held by a `before_save` hook go on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "the save waits until its token is done"]
pub struct SaveToken(pub(crate) u64);

impl SaveToken {
    /// The hook is finished with the buffer. Late or repeated calls are ignored.
    pub fn done(self, ed: &mut Editor) {
        ed.continue_save(self);
    }
}

#[derive(Default, Clone)]
pub struct Hooks {
    /// A buffer was created from a file or directory.
    pub buffer_opened: Vec<BufferHook>,
    /// In order, before a buffer is written by `Editor::save_buffer`.
    pub before_save: Vec<SaveHook>,
    pub buffer_saved: Vec<BufferHook>,
    /// Runs before the buffer is removed.
    pub buffer_killed: Vec<BufferHook>,
    /// After every command dispatched from a key or M-x.
    pub post_command: Vec<EditorHook>,
}
