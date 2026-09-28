//! Reacting to changes other programs make to visited files. Clean buffers reload; a
//! buffer with edits of its own asks which version to keep once it is the active buffer.
//! The directory watcher says which files changed, so only those are looked at.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use crate::buffer::BufferId;
use crate::editor::Editor;
use crate::ui::Choice;
use crate::watch::{Changes, Watcher};

#[derive(Default)]
struct ExternalChanges {
    /// Set once registered.
    watcher: Option<Watcher>,
    /// The real location of each file buffer's file, whose directory is watched.
    files: HashMap<BufferId, PathBuf>,
    /// Buffers whose file may have changed, not yet reloaded or asked about.
    pending: HashSet<BufferId>,
}

pub(crate) fn register(ed: &mut Editor) {
    let watcher = Watcher::new(ed, on_change);
    ed.ext_mut::<ExternalChanges>().watcher = Some(watcher);
    ed.hooks.on_file_visited(track);
    ed.hooks.on_buffer_killed(|ed, id| watch_file(ed, id, None));
    // Buffers held back by their own edits are handled once they become active or clean.
    ed.hooks.on_post_command(|ed| {
        if !ed.ext_mut::<ExternalChanges>().pending.is_empty() {
            resolve(ed);
        }
    });
}

/// Watches the file buffer `id` visits.
fn track(ed: &mut Editor, id: BufferId) {
    let file = ed.buffers.get(id).and_then(|b| b.path()).filter(|p| !p.is_dir());
    // Through symlinks, since the change happens where the file really is.
    let file = file.map(|path| fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()));
    watch_file(ed, id, file);
}

fn watch_file(ed: &mut Editor, id: BufferId, file: Option<PathBuf>) {
    let state = ed.ext_mut::<ExternalChanges>();
    let old = match &file {
        Some(file) => state.files.insert(id, file.clone()),
        None => state.files.remove(&id),
    };
    let Some(watcher) = state.watcher.as_mut().filter(|_| old != file) else { return };
    // Watch before unwatching, so a directory both share stays watched.
    if let Some(dir) = file.as_deref().and_then(|f| f.parent()) {
        watcher.watch(dir);
    }
    if let Some(dir) = old.as_deref().and_then(|f| f.parent()) {
        watcher.unwatch(dir);
    }
}

fn on_change(ed: &mut Editor, changes: Changes) -> bool {
    match changes {
        Changes::Unknown => return check_all(ed),
        Changes::Paths(paths) => {
            let state = ed.ext_mut::<ExternalChanges>();
            let changed = state.files.iter().filter(|(_, file)| paths.binary_search(file).is_ok());
            let changed: Vec<BufferId> = changed.map(|(&id, _)| id).collect();
            state.pending.extend(changed);
        }
    }
    resolve(ed)
}

/// Checks every buffer against its file: when the window regains focus, or events may
/// have been missed. Returns whether anything changed.
pub fn check_all(ed: &mut Editor) -> bool {
    let ids = ed.buffers.ids();
    ed.ext_mut::<ExternalChanges>().pending.extend(ids);
    resolve(ed)
}

/// Reloads pending clean buffers whose file changed and asks about the active one if it
/// has edits; the rest wait. Returns whether anything changed.
fn resolve(ed: &mut Editor) -> bool {
    let active = ed.active_buffer_id();
    let can_ask = !ed.has_modal();
    let mut changed = false;
    for id in std::mem::take(&mut ed.ext_mut::<ExternalChanges>().pending) {
        let Some(buf) = ed.buffers.get_mut(id) else { continue };
        let dirty = buf.is_dirty();
        if dirty && (id != active || !can_ask) {
            ed.ext_mut::<ExternalChanges>().pending.insert(id);
            continue;
        }
        if !buf.is_modified_on_disk() {
            continue;
        }
        changed = true;
        if dirty {
            ask_which_version(ed, id);
            continue;
        }
        let name = buf.name().to_string();
        if buf.reload_from_disk().is_ok() {
            ed.clamp_views(id);
            ed.set_status(format!("Auto-reloaded {} from disk", name));
        }
    }
    changed
}

fn ask_which_version(ed: &mut Editor, id: BufferId) {
    let name = ed.buffers[id].name().to_string();
    let label = format!("{} modified on disk. (r)eload disk version or (k)eep buffer edits? (r/k): ", name);
    let keep_name = name.clone();
    let choice = Choice::new("resolve-conflict", label, "rkyn", move |ed, key| {
        if matches!(key, 'r' | 'y') {
            match ed.buffers[id].reload_from_disk() {
                Ok(()) => {
                    ed.clamp_views(id);
                    ed.set_status(format!("Reloaded {} from disk (discarded local edits)", name));
                }
                Err(_) => ed.set_status(format!("Error reloading {} from disk", name)),
            }
        } else {
            keep_local(ed, id, &name);
        }
    })
    .on_cancel(move |ed| keep_local(ed, id, &keep_name));
    ed.push_modal(choice);
}

fn keep_local(ed: &mut Editor, id: BufferId, name: &str) {
    ed.buffers[id].acknowledge_disk_version();
    ed.set_status(format!("Kept local edits for {}", name));
}
