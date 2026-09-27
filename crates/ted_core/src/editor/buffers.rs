//! Buffer lifecycle: visiting files, showing, saving and killing buffers.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::time::Duration;

use crate::buffer::{map_pos, Buffer, BufferId, Edit};
use crate::editor::Editor;
use crate::plugin::{Hooks, SaveToken};
use crate::text::{absolutize, collapse_tilde};

/// How long a save waits for `before_save` hooks (a formatter) before writing anyway.
const SAVE_HOOK_TIMEOUT: Duration = Duration::from_secs(3);

type SaveDone = Box<dyn FnOnce(&mut Editor, io::Result<()>)>;

struct PendingSave {
    buffer: BufferId,
    next_hook: usize,
    then: SaveDone,
}

/// Saves waiting on `before_save` hooks, by serial.
#[derive(Default)]
struct Saves {
    serial: u64,
    pending: HashMap<u64, PendingSave>,
}

impl Editor {
    pub fn add_buffer(&mut self, buffer: Buffer) -> BufferId {
        self.buffers.insert(buffer)
    }

    /// A new empty buffer called `name` in the mode named `mode` (plain text if unknown).
    pub fn new_buffer(&mut self, name: impl Into<String>, mode: &str) -> BufferId {
        let mut buf = Buffer::new(name, "");
        if let Some(mode) = self.modes.get(mode) {
            buf.set_mode(mode);
        }
        self.add_buffer(buf)
    }

    /// The generated (file-less) buffer called `name`, created empty in `mode` if needed:
    /// how output buffers like `*compilation*` or `*xref*` are found again.
    pub fn special_buffer(&mut self, name: &str, mode: &str) -> BufferId {
        match self.buffers.find(|b| b.name() == name && b.path().is_none()) {
            Some(id) => id,
            None => self.new_buffer(name, mode),
        }
    }

    /// Switches buffer `id` to the mode named `mode`. Returns false if there is none.
    pub fn set_buffer_mode(&mut self, id: BufferId, mode: &str) -> bool {
        let Some(mode) = self.modes.get(mode) else { return false };
        self.buffers[id].set_mode(mode);
        true
    }

    /// Visits `path` in the active view, reusing an open buffer when there is one.
    /// Directories open in dired.
    pub fn open_file(&mut self, path: impl AsRef<Path>) -> io::Result<BufferId> {
        let path = absolutize(path);
        if path.is_dir() {
            return crate::modes::dired::open(self, &path, None);
        }
        let id = self.visit_file(&path)?;
        self.recent_files.touch(&path);
        self.active_view_mut().set_buffer(id);
        let name = self.buffers[id].name().to_string();
        self.set_status(format!("Opened {}", name));
        Ok(id)
    }

    /// The buffer visiting the file at absolute `path`, loaded if none is open, without
    /// showing it. An open buffer the file changed under is reloaded unless it has edits.
    pub fn visit_file(&mut self, path: &Path) -> io::Result<BufferId> {
        if let Some(id) = self.buffers.find_path(path) {
            let buf = &mut self.buffers[id];
            if buf.is_modified_on_disk() && !buf.is_dirty() {
                buf.reload_from_disk()?;
            }
            return Ok(id);
        }
        let mut buffer = Buffer::from_file(path)?;
        buffer.set_mode(self.modes.for_path(Some(path)));
        let id = self.add_buffer(buffer);
        self.run_buffer_hooks(|h| &h.buffer_opened, id);
        Ok(id)
    }

    /// Shows `id`: focuses a view already displaying it, else loads it in the active view.
    pub fn show_buffer(&mut self, id: BufferId) {
        let visible = self.layout.views().iter().find(|v| v.buffer == id).map(|v| v.id());
        match visible {
            Some(view) => {
                self.layout.set_active(view);
            }
            None => self.active_view_mut().set_buffer(id),
        }
    }

    /// The `*scratch*` buffer, recreated if it was saved to a file or killed.
    pub fn ensure_scratch(&mut self) -> BufferId {
        match self.buffers.find(Buffer::is_scratch) {
            Some(id) => id,
            None => self.add_buffer(Buffer::scratch()),
        }
    }

    /// Writes `id` to its file once the `before_save` hooks are done with it, then runs
    /// `then` with the outcome. Without hooks, or with hooks that finish at once, this all
    /// happens before it returns.
    pub fn save_buffer(&mut self, id: BufferId, then: impl FnOnce(&mut Editor, io::Result<()>) + 'static) {
        let saves = self.ext_mut::<Saves>();
        saves.serial += 1;
        let serial = saves.serial;
        saves.pending.insert(serial, PendingSave { buffer: id, next_hook: 0, then: Box::new(then) });
        if !self.hooks.before_save.is_empty() {
            self.after(SAVE_HOOK_TIMEOUT, move |ed| ed.write_pending(serial));
        }
        self.continue_save(SaveToken(serial));
    }

    /// Runs the next `before_save` hook of the save `token` holds, or writes the file.
    pub(crate) fn continue_save(&mut self, token: SaveToken) {
        let Some(save) = self.ext_mut::<Saves>().pending.get_mut(&token.0) else {
            return;
        };
        let (buffer, index) = (save.buffer, save.next_hook);
        save.next_hook += 1;
        match self.hooks.before_save.get(index).cloned() {
            Some(hook) if self.buffers.contains(buffer) => hook(self, buffer, token),
            _ => self.write_pending(token.0),
        }
    }

    fn write_pending(&mut self, serial: u64) {
        let Some(save) = self.ext_mut::<Saves>().pending.remove(&serial) else {
            return;
        };
        let result = match self.buffers.get_mut(save.buffer) {
            Some(buf) => buf.save(),
            None => Err(io::Error::new(io::ErrorKind::NotFound, "the buffer was killed")),
        };
        if result.is_ok() {
            self.finish_save(save.buffer);
        }
        (save.then)(self, result);
    }

    /// Writes `id` to `path`, which it visits from then on (picking up that path's mode).
    /// Unlike `save_buffer`, this runs no `before_save` hooks.
    pub fn save_buffer_as(&mut self, id: BufferId, path: &Path) -> io::Result<()> {
        self.buffers[id].save_as(path)?;
        let mode = self.modes.for_path(Some(path));
        self.buffers[id].set_mode(mode);
        self.finish_save(id);
        Ok(())
    }

    fn finish_save(&mut self, id: BufferId) {
        if let Some(path) = self.buffers[id].path() {
            let msg = format!("Wrote {}", path.display());
            self.set_status(msg);
        }
        self.run_buffer_hooks(|h| &h.buffer_saved, id);
    }

    /// Applies `edits` to buffer `id` as one undo step (see `Buffer::apply_edits`), moving
    /// the cursor and mark of every window showing it along with the text.
    pub fn apply_edits(&mut self, id: BufferId, mut edits: Vec<Edit>) -> Result<(), String> {
        let active = self.layout.active();
        let cursor = if active.buffer == id { active.cursor.pos } else { 0 };
        self.buffers[id].apply_edits(&mut edits, cursor)?;
        for view in self.layout.views_showing(id) {
            let cursor = &mut view.cursor;
            cursor.pos = map_pos(&edits, cursor.pos);
            cursor.mark = cursor.mark.map(|mark| map_pos(&edits, mark));
            cursor.goal_col = None;
        }
        Ok(())
    }

    /// Kills `id` (without asking), showing another buffer wherever it was displayed.
    /// Killing the last remaining scratch buffer just empties it.
    pub fn kill_buffer(&mut self, id: BufferId) {
        let name = self.buffers[id].name().to_string();
        if self.buffers[id].is_scratch() && self.buffers.len() == 1 {
            self.buffers[id].set_text("");
            for view in self.layout.views_mut() {
                view.set_buffer(id);
            }
            self.set_status("Cleared scratch buffer");
            return;
        }

        self.run_buffer_hooks(|h| &h.buffer_killed, id);
        self.buffers.remove(id);
        let last = self.buffers.iter().next_back().map(|(other, _)| other);
        let replacement = match last {
            Some(other) => other,
            None => self.ensure_scratch(),
        };
        let buffers = &self.buffers;
        for view in self.layout.views_mut() {
            view.forget(id);
            if view.buffer == id {
                view.go_back(buffers, replacement);
            }
        }
        for saved in self.saved_layouts.values_mut() {
            for view in saved.views_mut() {
                view.forget(id);
                if view.buffer == id {
                    view.set_buffer(replacement);
                }
            }
        }
        self.ensure_scratch();
        self.set_status(format!("Killed buffer {}", name));
    }

    /// Keeps cursors of views showing `id` inside the buffer after its text was replaced.
    pub fn clamp_views(&mut self, id: BufferId) {
        let (len, lines) = (self.buffers[id].len_chars(), self.buffers[id].len_lines());
        for view in self.layout.views_showing(id) {
            view.clamp(len, lines);
            view.cursor.mark = None;
        }
    }

    /// Exits once every modified file is saved or knowingly left unsaved: how the window
    /// manager's close button and closing the last window end the session.
    pub fn request_exit(&mut self) {
        crate::commands::files::save_some_buffers(self, |ed| ed.running = false);
    }

    pub fn check_external_changes(&mut self) -> bool {
        crate::commands::files::check_external_changes(self)
    }

    fn run_buffer_hooks(&mut self, select: impl Fn(&Hooks) -> &Vec<crate::plugin::BufferHook>, id: BufferId) {
        for hook in select(&self.hooks).clone() {
            hook(self, id);
        }
    }

    /// A short description of a buffer for pickers: its path, or its kind.
    pub fn buffer_description(&self, id: BufferId) -> String {
        let buf = &self.buffers[id];
        match buf.path() {
            Some(path) => collapse_tilde(path),
            None if buf.is_scratch() => "scratch buffer".to_string(),
            None => format!("{} buffer", buf.mode().name.to_lowercase()),
        }
    }
}
