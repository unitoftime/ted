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
//!             let id = ed.generated_buffer("todo list", "Todo List", BufferScope::Editor);
//!             let mut text = StyledText::new();
//!             text.line(&[("Open tasks", Some(FaceId::HEADING))]);
//!             ed.buffers[id].set_styled("todo", text);
//!             ed.show_buffer(id);
//!         });
//!         // Special: read-only, with `h` (help), `n` / `p`, `g` (revert) and `q` (quit).
//!         ed.define_mode(Mode::new("Todo List").special().revert("todo-list").keys(&[("RET", "todo-open")]));
//!         // No global key: `C-c <key>` is the user's (`bind("C-c o", "todo-open")`).
//!     }
//! }
//! ```
//!
//! - `ed.commands.register(name, doc, |ed, arg| ..)` adds commands.
//! - `ed.define_mode(Mode::new(..).keys(..))` adds a major mode and its keymap (named
//!   after the mode). `ed.define_keymap(KeymapDef::new(..))` defines any other keymap, or
//!   adds to one (`"global"`). Either way the bindings are defaults `init.rhai` can
//!   override; global `C-c <key>` is left to the user (see `commands::bindings`).
//! - `ed.settings.define(name, default, doc)` declares settings; keep the handle and read
//!   with `ed.settings.get(handle)`, or `get_in(handle, buf.mode())` for a buffer, since
//!   any setting can have its own value in a mode (`Mode::set`, `ed.set_mode_setting`).
//!   `ed.watch_setting` reacts to changes.
//! - `ed.faces.register(name, default)` declares faces themes can restyle.
//! - `ed.generated_buffer(name, mode, scope)` makes (or finds again) an output buffer, one
//!   per editor or per directory; `StyledText` builds its content, `rows::RowText` when
//!   lines stand for items (point and marks then stay on their items across refreshes);
//!   `buffer.enable_keymap` layers a minor keymap on one buffer.
//! - `ed.hooks.on_*` subscribes to buffer and command events; `before_save` hooks can
//!   change a buffer before it is written (formatting) and hold the save until they are
//!   done.
//! - `ed.ext_mut::<T>()` / `ed.set_ext(..)` / `buffer.local_mut::<T>()` store plugin state
//!   per editor or buffer; `ed.set_chain` / `ed.last_chain` pass state to the next command.
//! - `ed.prompt / confirm / pick / push_modal` and `ui::Menu` / `ui::Tooltip` for UI;
//!   `ed.spawn` for background work, `ed.after` to run something later.
//! - `process::Program` starts outside programs (git, servers, builds), so where programs
//!   run is decided in one place.
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

/// Code plugins run on editor events, registered with the `on_*` methods.
#[derive(Default, Clone)]
pub struct Hooks {
    pub(crate) file_visited: Vec<BufferHook>,
    pub(crate) before_save: Vec<SaveHook>,
    pub(crate) buffer_saved: Vec<BufferHook>,
    pub(crate) buffer_killed: Vec<BufferHook>,
    pub(crate) post_command: Vec<EditorHook>,
}

impl Hooks {
    /// A buffer started visiting a file or directory: it was opened, saved under a new
    /// name, or its file was renamed.
    pub fn on_file_visited(&mut self, hook: impl Fn(&mut Editor, BufferId) + 'static) {
        self.file_visited.push(Rc::new(hook));
    }

    /// Runs in order before a buffer is written by `Editor::save_buffer`; see `SaveHook`.
    pub fn on_before_save(&mut self, hook: impl Fn(&mut Editor, BufferId, SaveToken) + 'static) {
        self.before_save.push(Rc::new(hook));
    }

    pub fn on_buffer_saved(&mut self, hook: impl Fn(&mut Editor, BufferId) + 'static) {
        self.buffer_saved.push(Rc::new(hook));
    }

    /// Runs before the buffer is removed.
    pub fn on_buffer_killed(&mut self, hook: impl Fn(&mut Editor, BufferId) + 'static) {
        self.buffer_killed.push(Rc::new(hook));
    }

    /// Runs after every command dispatched from a key or M-x.
    pub fn on_post_command(&mut self, hook: impl Fn(&mut Editor) + 'static) {
        self.post_command.push(Rc::new(hook));
    }
}
