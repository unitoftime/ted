//! Files and buffers: visiting, saving, switching, killing and reverting.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::external_changes;
use crate::buffer::BufferId;
use crate::command::Arg;
use crate::editor::Editor;
use crate::project::Project;
use crate::text::{absolutize, collapse_tilde, expand_tilde};
use crate::ui::{Choice, Completion, Picker, PickerItem, Prompt};

pub fn register(ed: &mut Editor) {
    let c = &mut ed.commands;
    c.register("find-file", "Open a file from disk with path completion", |ed, _| {
        let anchor = anchor_directory(&ed.active_buffer().directory());
        find_file_prompt(ed, anchor);
    });
    c.register("save-buffer", "Save the active buffer to its file", |ed, _| save_active(ed));
    c.register("save-some-buffers", "Offer to save each modified file", |ed, _| {
        if modified_files(ed).is_empty() {
            ed.set_status("No files need saving");
        }
        save_some_buffers(ed, |_| {});
    });
    c.register("save-buffer-as", "Save the active buffer to a new path (write-file)", |ed, _| {
        save_as_prompt(ed, "Write file: ");
    });
    c.register("switch-to-buffer", "Fuzzy switch to an open buffer or a file of the current project", |ed, _| {
        buffer_switcher(ed)
    });
    c.register("switch-to-scratch", "Switch to the *scratch* buffer", |ed, _| {
        let id = ed.ensure_scratch();
        ed.show_buffer(id);
        ed.set_status("Switched to *scratch* buffer");
    });
    c.register("recentf-open-files", "Fuzzy open a recently visited file", |ed, _| recent_files(ed));
    c.register("kill-buffer", "Close the active buffer", |ed, _| {
        let id = ed.active_buffer_id();
        let buf = ed.active_buffer();
        if !buf.is_dirty() {
            ed.kill_buffer(id);
            return;
        }
        let label = format!("Buffer {} has unsaved changes. Kill anyway? (y/n) ", buf.name());
        ed.confirm("kill-buffer", label, move |ed, yes| {
            if yes {
                ed.kill_buffer(id);
            } else {
                ed.set_status("Canceled kill buffer");
            }
        });
    });
    c.register("revert-buffer", "Refresh the buffer from its source (file, directory, command)", |ed, _| {
        revert(ed);
    });
    c.register("exit-ted", "Exit ted after confirmation, offering to save modified files", |ed, _| {
        if modified_files(ed).is_empty() {
            ed.confirm("exit", "Are you sure you want to exit ted? (y/n) ", |ed, yes| {
                if yes {
                    ed.quit();
                } else {
                    ed.set_status("Canceled exit");
                }
            });
        } else {
            // Answering the save questions is confirmation enough; `q` cancels.
            ed.request_exit();
        }
    });
    c.register("reload-ted", "Restart ted from its binary (a new build), reopening its files and windows", |ed, _| {
        crate::session::request_restart(ed)
    });
}

fn modified_files(ed: &Editor) -> Vec<BufferId> {
    modified(ed, ed.buffers.ids())
}

/// The file buffers among `ids` with unsaved changes.
fn modified(ed: &Editor, mut ids: Vec<BufferId>) -> Vec<BufferId> {
    ids.retain(|&id| ed.buffers.get(id).is_some_and(|b| b.path().is_some() && b.is_dirty()));
    ids
}

type Continuation = Box<dyn FnOnce(&mut Editor)>;

/// Asks about each modified file buffer in turn: (y)es save it, (n)o leave it, (!) save it
/// and all the rest, (q)uit. Runs `then` once every buffer is answered, unless the user
/// quit or a save failed.
pub fn save_some_buffers(ed: &mut Editor, then: impl FnOnce(&mut Editor) + 'static) {
    offer_to_save(ed, ed.buffers.ids(), then);
}

/// As `save_some_buffers`, asking only about the modified files among `ids`.
pub fn offer_to_save(ed: &mut Editor, ids: Vec<BufferId>, then: impl FnOnce(&mut Editor) + 'static) {
    let pending = modified(ed, ids);
    ask_to_save(ed, pending, Box::new(then));
}

fn ask_to_save(ed: &mut Editor, mut pending: Vec<BufferId>, then: Continuation) {
    pending.retain(|&id| ed.buffers.get(id).is_some_and(|b| b.is_dirty()));
    let Some(&id) = pending.first() else {
        then(ed);
        return;
    };
    let buf = &ed.buffers[id];
    let path = buf.path().map(collapse_tilde).unwrap_or_default();
    let on_disk = if buf.is_modified_on_disk() { " (changed on disk!)" } else { "" };
    let label = format!("Save {}{}? (y)es (n)o (!) all (q)uit: ", path, on_disk);
    let choice = Choice::new("save-some-buffers", label, "yn!q", move |ed, key| {
        let rest = pending[1..].to_vec();
        match key {
            'y' => write(ed, id, move |ed| ask_to_save(ed, rest, then)),
            'n' => ask_to_save(ed, rest, then),
            '!' => write_all(ed, pending, then),
            _ => ed.set_status("Canceled"),
        }
    });
    ed.push_modal(choice);
}

/// Saves `ids` one after another, then runs `then`; stops at a failure.
fn write_all(ed: &mut Editor, mut ids: Vec<BufferId>, then: Continuation) {
    if ids.is_empty() {
        then(ed);
        return;
    }
    let id = ids.remove(0);
    write(ed, id, move |ed| write_all(ed, ids, then));
}

/// `dir` as the initial text of a file prompt.
pub fn anchor_directory(dir: &Path) -> String {
    let mut s = collapse_tilde(dir);
    if !s.ends_with('/') {
        s.push('/');
    }
    s
}

/// Completes the last path component of `input` against the file system.
pub fn complete_path(input: &str) -> Completion {
    let (dir, prefix) = match input.rfind('/') {
        Some(i) => (&input[..=i], &input[i + 1..]),
        None => ("", input),
    };
    let scan_dir = if dir.is_empty() { PathBuf::from(".") } else { expand_tilde(dir) };

    let mut candidates: Vec<String> = std::fs::read_dir(&scan_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let hidden = name.starts_with('.') && !prefix.starts_with('.');
            if hidden || !name.starts_with(prefix) {
                return None;
            }
            Some(if entry.path().is_dir() { format!("{}/", name) } else { name })
        })
        .collect();
    candidates.sort();

    let common = if candidates.is_empty() { prefix } else { common_prefix(&candidates) };
    let base = if dir.is_empty() { String::new() } else { collapse_tilde(dir) };
    Completion { text: format!("{}{}", base, common), base: dir.to_string(), candidates, continue_suffix: Some('/') }
}

fn common_prefix(items: &[String]) -> &str {
    let Some((first, rest)) = items.split_first() else {
        return "";
    };
    let len = rest.iter().fold(first.len(), |len, s| {
        first[..len].char_indices().zip(s.chars()).find(|((_, a), b)| a != b).map_or(len.min(s.len()), |((i, _), _)| i)
    });
    &first[..len]
}

pub fn find_file_prompt(ed: &mut Editor, initial: String) {
    let prompt = Prompt::new("find-file", "Find file: ", visit_input).initial(initial).completer(complete_path);
    ed.push_modal(prompt);
}

/// Visits the entered path: a file, or a directory in dired.
fn visit_input(ed: &mut Editor, input: String) {
    if input.is_empty() {
        return;
    }
    if let Err(e) = ed.open_file(&input) {
        ed.set_status(format!("Error opening {}: {}", input, e));
    }
}

fn save_as_prompt(ed: &mut Editor, label: &str) {
    ed.prompt("save-as", label, "", |ed, input| {
        if input.is_empty() {
            return;
        }
        let id = ed.active_buffer_id();
        if let Err(e) = ed.save_buffer_as(id, &absolutize(&input)) {
            ed.set_status(format!("Error saving {}: {}", input, e));
        }
        ed.ensure_scratch();
    });
}

fn save_active(ed: &mut Editor) {
    let id = ed.active_buffer_id();
    let buf = ed.active_buffer();
    if buf.path().is_none() {
        save_as_prompt(ed, "File to save in: ");
        return;
    }
    if buf.is_modified_on_disk() {
        let label = format!("{} was modified on disk! Overwrite disk file? (y/n): ", buf.name());
        ed.confirm("overwrite-disk", label, move |ed, yes| {
            if yes {
                ed.buffers[id].acknowledge_disk_version();
                write(ed, id, |_| {});
            } else {
                ed.set_status("Save cancelled (file modified on disk)");
            }
        });
        return;
    }
    write(ed, id, |_| {});
}

/// Saves `id`, then runs `then` if it saved; a failure is reported in the status line.
fn write(ed: &mut Editor, id: BufferId, then: impl FnOnce(&mut Editor) + 'static) {
    ed.save_buffer(id, move |ed, result| match result {
        Ok(()) => then(ed),
        Err(e) => ed.set_status(format!("Error saving file: {}", e)),
    });
}

/// What a switcher entry leads to.
enum Target {
    Buffer(BufferId),
    File(PathBuf),
}

/// The active workspace's buffers, most recently shown first, then (in a git project) the
/// project's other files, nearest to the active buffer's directory first. Paths inside the
/// project show relative to its root.
fn buffer_switcher(ed: &mut Editor) {
    let scratch = ed.ensure_scratch();
    let project = ed.project();
    let active = ed.active_buffer_id();
    let mut ids = ed.workspaces.active().buffers().to_vec();
    if !ids.contains(&scratch) {
        ids.push(scratch);
    }
    // Other buffers first so RET immediately switches away.
    ids.sort_by_key(|&id| id == active);
    let buffers = ids.iter().map(|&id| {
        let buf = &ed.buffers[id];
        let relative = buf.path().and_then(|p| p.strip_prefix(&project.root).ok());
        let subtitle = relative.map_or_else(|| ed.buffer_description(id), |p| p.display().to_string());
        (PickerItem::new(buf.name(), subtitle), Target::Buffer(id))
    });
    let title =
        if project.is_git() { format!("Switch Buffer · {}", project.name()) } else { "Switch Buffer".to_string() };
    let picker = Picker::with_values("switch-to-buffer", title, buffers, |ed, target| match target {
        Target::Buffer(id) if ed.buffers.contains(id) => {
            let name = ed.buffers[id].name().to_string();
            ed.show_buffer(id);
            ed.set_status(format!("Switched to buffer {}", name));
            external_changes::check_all(ed);
        }
        Target::Buffer(_) => {}
        Target::File(path) => {
            if let Err(e) = ed.open_file(&path) {
                ed.set_status(format!("Error opening {}: {}", path.display(), e));
            }
        }
    });
    if !project.is_git() {
        ed.push_modal(picker);
        return;
    }
    let open: HashSet<PathBuf> = ids
        .iter()
        .filter_map(|&id| Some(ed.buffers[id].path()?.strip_prefix(&project.root).ok()?.to_path_buf()))
        .collect();
    let here = ed.active_buffer().directory();
    let lists = ed.file_lists();
    let cached = lists.cached(&project);
    let picker = picker.load_in_background(ed, move |feed| {
        let cached = cached.as_deref().unwrap_or_default();
        if !cached.is_empty() && !feed.send(project_files_near(&project, &here, cached.iter(), &open)) {
            return;
        }
        // The files that appeared since the last listing.
        let fresh = lists.list(&project);
        let known: HashSet<&PathBuf> = cached.iter().collect();
        feed.send(project_files_near(&project, &here, fresh.iter().filter(|f| !known.contains(f)), &open));
    });
    ed.push_modal(picker);
}

/// Job thread: `files` of the project (relative to the root) not in `open`, by directory
/// distance from `here`.
fn project_files_near<'a>(
    project: &Project,
    here: &Path,
    files: impl Iterator<Item = &'a PathBuf>,
    open: &HashSet<PathBuf>,
) -> Vec<(PickerItem, Target)> {
    let here: Vec<_> = here.strip_prefix(&project.root).map(|p| p.components().collect()).unwrap_or_default();
    let mut files: Vec<(usize, &PathBuf)> = files
        .filter(|rel| !open.contains(*rel))
        .map(|rel| {
            let dir = rel.parent().map_or(0, |d| d.components().count());
            let shared = rel.components().zip(&here).take_while(|(a, b)| a == *b).count().min(dir);
            (dir + here.len() - 2 * shared, rel)
        })
        .collect();
    files.sort_unstable();
    files
        .into_iter()
        .map(|(_, rel)| {
            let name = rel.file_name().unwrap_or_default().to_string_lossy().to_string();
            let item = PickerItem::new(name, rel.display().to_string());
            (item, Target::File(project.root.join(rel)))
        })
        .collect()
}

fn recent_files(ed: &mut Editor) {
    let paths: Vec<PathBuf> = ed.recent_files.entries().to_vec();
    let items = paths
        .iter()
        .map(|p| PickerItem::new(p.file_name().unwrap_or_default().to_string_lossy(), collapse_tilde(p)))
        .collect();
    ed.pick("recentf", "Recent Files", items, move |ed, index| {
        let path = &paths[index];
        if let Err(e) = ed.open_file(path) {
            ed.set_status(format!("Error opening {}: {}", path.display(), e));
        }
    });
}

fn revert(ed: &mut Editor) {
    if let Some(name) = ed.active_buffer().mode().revert.clone() {
        match ed.commands.id(&name) {
            Some(command) => ed.call(command, &Arg::None),
            None => ed.set_status(format!("Unknown revert command '{}'", name)),
        }
        return;
    }
    let id = ed.active_buffer_id();
    let Some(path) = ed.active_buffer().path().map(Path::to_path_buf) else {
        ed.set_status("Buffer is not visiting a file");
        return;
    };
    match ed.buffers[id].reload_from_disk() {
        Ok(()) => {
            ed.clamp_views(id);
            ed.set_status(format!("Reverted buffer from {}", path.display()));
        }
        Err(e) => ed.set_status(format!("Error reverting buffer: {}", e)),
    }
}
