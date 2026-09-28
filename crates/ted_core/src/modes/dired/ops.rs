//! Dired file operations: deleting, renaming/moving, copying and creating directories.
//! Each acts on the marked entries, or on the entry at point when none are marked, then
//! re-reads every dired buffer. Buffers visiting moved files follow them; clean buffers
//! visiting deleted files are killed.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::{entry_at_point, focus_entry, listed_dir, refresh_all, Dired};
use crate::commands::files::{anchor_directory, complete_path};
use crate::editor::Editor;
use crate::rows::{self, Rows};
use crate::text::{collapse_tilde, expand_tilde};
use crate::ui::Prompt;

pub(super) fn register(ed: &mut Editor) {
    let c = &mut ed.commands;
    c.register("dired-delete", "Delete the marked entries (or the one at point), after confirmation", |ed, _| {
        delete(ed)
    });
    c.register("dired-rename", "Rename or move the marked entries (or the one at point)", |ed, _| {
        transfer(ed, Transfer::Move)
    });
    c.register("dired-copy", "Copy the marked entries (or the one at point)", |ed, _| transfer(ed, Transfer::Copy));
    c.register("dired-create-directory", "Create a directory (and any missing parents)", |ed, _| create_directory(ed));
}

struct Source {
    path: PathBuf,
    /// A real directory, not a symlink to one: deleting or copying it recurses.
    is_tree: bool,
}

/// The listed directory and the marked entries, or the entry at point when none are.
fn sources(ed: &Editor) -> Option<(PathBuf, Vec<Source>)> {
    let buf = ed.active_buffer();
    let (state, rows) = (buf.local::<Dired>()?, buf.local::<Rows>()?);
    let dir = listed_dir(buf)?;
    let source = |e: &super::Entry| Source { path: dir.join(&e.name), is_tree: e.is_dir && !e.is_link };
    let marked: Vec<Source> = rows.marked().map(|i| source(&state.entries[i])).collect();
    if !marked.is_empty() {
        return Some((dir, marked));
    }
    let (_, entry) = entry_at_point(ed)?;
    let one = source(&entry);
    Some((dir, vec![one]))
}

fn clear_marks(ed: &mut Editor) {
    let id = ed.active_buffer_id();
    rows::clear_marks(ed, id);
}

fn delete(ed: &mut Editor) {
    let Some((_, sources)) = sources(ed) else { return };
    let recursive = if sources.iter().any(|s| s.is_tree) { " recursively" } else { "" };
    let label = format!("Delete {}{}? (y/n) ", describe(&sources), recursive);
    ed.confirm("dired-delete", label, move |ed, yes| {
        if !yes {
            ed.set_status("Canceled delete");
            return;
        }
        let results = sources.iter().map(|s| {
            let result = if s.is_tree { fs::remove_dir_all(&s.path) } else { fs::remove_file(&s.path) };
            (s, result)
        });
        let outcome = Outcome::collect(results);
        for s in sources.iter().filter(|s| !s.path.exists()) {
            kill_clean_buffers_under(ed, &s.path);
        }
        clear_marks(ed);
        refresh_all(ed);
        outcome.report(ed, "Deleted");
    });
}

/// Kills buffers without unsaved edits that visit `path` or something inside it.
fn kill_clean_buffers_under(ed: &mut Editor, path: &Path) {
    let doomed: Vec<_> = ed
        .buffers
        .iter()
        .filter(|(_, b)| !b.is_dirty() && b.path().is_some_and(|p| p.starts_with(path)))
        .map(|(id, _)| id)
        .collect();
    for id in doomed {
        ed.kill_buffer(id);
    }
}

#[derive(Clone, Copy)]
enum Transfer {
    Move,
    Copy,
}

impl Transfer {
    fn verb(self) -> &'static str {
        match self {
            Transfer::Move => "Rename",
            Transfer::Copy => "Copy",
        }
    }

    fn past(self) -> &'static str {
        match self {
            Transfer::Move => "Renamed",
            Transfer::Copy => "Copied",
        }
    }

    fn apply(self, source: &Source, dest: &Path) -> io::Result<()> {
        if source.is_tree && dest.starts_with(&source.path) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "cannot put a directory inside itself"));
        }
        match self {
            Transfer::Copy => copy_tree(&source.path, dest),
            Transfer::Move => match fs::rename(&source.path, dest) {
                Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
                    copy_tree(&source.path, dest)?;
                    remove_tree(&source.path)
                }
                result => result,
            },
        }
    }
}

/// Prompts for where to move or copy the sources: a new path for one entry, or an
/// existing directory to put them in. Asks before overwriting anything.
fn transfer(ed: &mut Editor, kind: Transfer) {
    let Some((dir, sources)) = sources(ed) else { return };
    let (label, initial) = match sources.as_slice() {
        [one] => (format!("{} {} to: ", kind.verb(), file_name(&one.path)), collapse_tilde(&one.path)),
        many => (format!("{} {} entries to: ", kind.verb(), many.len()), anchor_directory(&dir)),
    };
    let prompt = Prompt::new("dired-transfer", label, move |ed, input| {
        if input.is_empty() {
            return;
        }
        let dest = resolve(&dir, &input);
        let into_dir = dest.is_dir();
        if sources.len() > 1 && !into_dir {
            ed.set_status(format!("{} is not a directory", collapse_tilde(&dest)));
            return;
        }
        let plan: Vec<(Source, PathBuf)> = sources
            .into_iter()
            .map(|s| {
                let to = if into_dir { dest.join(file_name(&s.path)) } else { dest.clone() };
                (s, to)
            })
            .filter(|(s, to)| s.path != *to)
            .collect();
        let existing = plan.iter().filter(|(_, to)| to.symlink_metadata().is_ok()).count();
        if existing == 0 {
            run_transfer(ed, kind, &dir, plan);
            return;
        }
        let label = format!(
            "{} already exist{}. Overwrite? (y/n) ",
            plural(existing, "entry", "entries"),
            if existing == 1 { "s" } else { "" }
        );
        ed.confirm("dired-overwrite", label, move |ed, yes| {
            if yes {
                run_transfer(ed, kind, &dir, plan);
            } else {
                ed.set_status(format!("Canceled {}", kind.verb().to_lowercase()));
            }
        });
    })
    .initial(initial)
    .completer(complete_path);
    ed.push_modal(prompt);
}

fn run_transfer(ed: &mut Editor, kind: Transfer, dir: &Path, plan: Vec<(Source, PathBuf)>) {
    let mut moved = Vec::new();
    let outcome = Outcome::collect(plan.iter().map(|(s, to)| {
        let result = kind.apply(s, to);
        if result.is_ok() {
            moved.push((s.path.clone(), to.clone()));
        }
        (s, result)
    }));
    if let Transfer::Move = kind {
        for (from, to) in &moved {
            follow_move(ed, from, to);
        }
    }
    clear_marks(ed);
    refresh_all(ed);
    // A single entry renamed within this directory keeps point.
    if let [(_, to)] = plan.as_slice() {
        if to.parent() == Some(dir) {
            focus_entry(ed, to.file_name());
        }
    }
    outcome.report(ed, kind.past());
}

/// Points buffers visiting `from` (or anything inside it) at the moved location.
fn follow_move(ed: &mut Editor, from: &Path, to: &Path) {
    let mut moved_ids = Vec::new();
    for (id, buf) in ed.buffers.iter_mut() {
        let Some(rest) = buf.path().and_then(|p| p.strip_prefix(from).ok()) else { continue };
        let moved = if rest.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rest) };
        let listing = buf.local::<Dired>().is_some();
        buf.set_path(&moved);
        if listing {
            let name = format!("{}/", buf.name().trim_end_matches('/'));
            buf.set_name(name);
        }
        moved_ids.push(id);
    }
    for id in moved_ids {
        ed.file_visited(id);
    }
}

fn create_directory(ed: &mut Editor) {
    let Some(dir) = listed_dir(ed.active_buffer()) else { return };
    let initial = anchor_directory(&dir);
    let prompt = Prompt::new("dired-create-directory", "Create directory: ", move |ed, input| {
        if input.is_empty() {
            return;
        }
        let path = resolve(&dir, &input);
        if let Err(e) = fs::create_dir_all(&path) {
            ed.set_status(format!("Error creating {}: {}", collapse_tilde(&path), e));
            return;
        }
        refresh_all(ed);
        // Point goes to the new directory, or the first new parent listed here.
        let here = path.strip_prefix(&dir).ok().and_then(|p| p.components().next());
        focus_entry(ed, here.map(|c| c.as_os_str()));
        ed.set_status(format!("Created {}", anchor_directory(&path)));
    })
    .initial(initial)
    .completer(complete_path);
    ed.push_modal(prompt);
}

/// The successes and the first failure of an operation over several entries.
struct Outcome {
    done: usize,
    total: usize,
    first_error: Option<String>,
    /// The entry's name when there was just one.
    single: Option<String>,
}

impl Outcome {
    fn collect<'a>(results: impl Iterator<Item = (&'a Source, io::Result<()>)>) -> Self {
        let mut outcome = Outcome { done: 0, total: 0, first_error: None, single: None };
        for (source, result) in results {
            outcome.total += 1;
            outcome.single = (outcome.total == 1).then(|| file_name(&source.path));
            match result {
                Ok(()) => outcome.done += 1,
                Err(e) => {
                    outcome.first_error.get_or_insert_with(|| format!("{}: {}", file_name(&source.path), e));
                }
            }
        }
        outcome
    }

    fn report(self, ed: &mut Editor, past: &str) {
        let what = match self.single {
            Some(name) => name,
            None => plural(self.done, "entry", "entries"),
        };
        match self.first_error {
            None => ed.set_status(format!("{} {}", past, what)),
            Some(e) if self.total == 1 => ed.set_status(format!("Error: {}", e)),
            Some(e) => ed.set_status(format!("{} {} of {}; error: {}", past, self.done, self.total, e)),
        }
    }
}

/// `input` as a path, relative to `dir` unless absolute or `~`-rooted.
fn resolve(dir: &Path, input: &str) -> PathBuf {
    let path = expand_tilde(input);
    let path = if path.is_absolute() { path } else { dir.join(path) };
    path.canonicalize().unwrap_or(path)
}

/// Copies a file, symlink or whole directory tree to `dest`, merging into an existing
/// directory there.
fn copy_tree(src: &Path, dest: &Path) -> io::Result<()> {
    let kind = fs::symlink_metadata(src)?.file_type();
    #[cfg(unix)]
    if kind.is_symlink() {
        if dest.symlink_metadata().is_ok() {
            remove_tree(dest)?;
        }
        return std::os::unix::fs::symlink(fs::read_link(src)?, dest);
    }
    if kind.is_dir() {
        fs::create_dir_all(dest)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            copy_tree(&entry.path(), &dest.join(entry.file_name()))?;
        }
        return Ok(());
    }
    fs::copy(src, dest).map(drop)
}

/// Removes a file, symlink or whole directory tree.
fn remove_tree(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| collapse_tilde(path), |n| n.to_string_lossy().to_string())
}

/// "foo.txt" for one source, else "3 entries".
fn describe(sources: &[Source]) -> String {
    match sources {
        [one] => file_name(&one.path),
        many => plural(many.len(), "entry", "entries"),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{} {}", n, if n == 1 { one } else { many })
}
