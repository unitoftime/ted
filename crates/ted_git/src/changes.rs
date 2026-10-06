//! Diffs as rows, shared by the status buffer's unstaged and staged sections and by diff
//! buffers: a heading per file (its kind, path and line counts, or a binary file's sizes; a
//! renamed file's path as `old → new`) with its hunks under it, and what the keys at point
//! do to them: visit the line, stage, unstage or discard. A selection stages, unstages or
//! discards just the changed lines it covers.

use std::hash::Hash;
use std::path::{Path, PathBuf};

use ted_core::rows::RowSpec;
use ted_core::text::human_size;
use ted_core::{Editor, FaceId};

use crate::git::args;
use crate::model::{FileDiff, Section};
use crate::{process, GitFaces};

/// A row of a diff, by index: a file's heading, a hunk's heading or a line of a hunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Line {
    File(usize),
    Hunk(usize, usize),
    /// A line of a hunk's body: (file, hunk, line within the hunk).
    Body(usize, usize, usize),
}

impl Line {
    pub fn file(self) -> usize {
        match self {
            Line::File(f) | Line::Hunk(f, _) | Line::Body(f, ..) => f,
        }
    }

    pub fn hunk(self) -> Option<usize> {
        match self {
            Line::File(_) => None,
            Line::Hunk(_, h) | Line::Body(_, h, _) => Some(h),
        }
    }

    /// The file, hunk and line of a line of a hunk's body.
    pub fn body(self) -> Option<(usize, usize, usize)> {
        match self {
            Line::Body(f, h, l) => Some((f, h, l)),
            _ => None,
        }
    }
}

/// What `TAB` folds on a line, named so it stays folded across refreshes: a file by path,
/// or a hunk by path and header.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Fold {
    File(String),
    Hunk(String, String),
}

impl Fold {
    /// The fold `line` of `files` is in: a file heading's file, else its hunk.
    pub fn of(files: &[FileDiff], line: Line) -> Option<Fold> {
        let file = files.get(line.file())?;
        Some(match line.hunk() {
            None => Fold::File(file.path.clone()),
            Some(h) => Fold::Hunk(file.path.clone(), file.hunks.get(h)?.header.clone()),
        })
    }
}

/// Writes `files` through `row`: a heading per file, and under each open file its hunks,
/// each a heading over its lines when open. `open` says whether a file or hunk heading
/// shows what is under it.
pub fn write(
    files: &[FileDiff],
    faces: &GitFaces,
    open: impl Fn(Line) -> bool,
    mut row: impl FnMut(Line, &[(&str, Option<FaceId>)]),
) {
    for (f, file) in files.iter().enumerate() {
        let kind = format!("{:<11}", file.kind);
        let note = note(file);
        let (added, removed) = (format!("  +{}", file.added), format!(" -{}", file.removed));
        let heading = Some(faces.file_heading);
        let mut parts = vec![(kind.as_str(), None)];
        if let Some(from) = &file.from {
            parts.extend([(from.as_str(), heading), (" → ", None)]);
        }
        parts.push((file.path.as_str(), heading));
        if !note.is_empty() {
            parts.push((note.as_str(), Some(faces.file_note)));
        }
        // Only hunks have lines to count: a binary file, a rename or a change of
        // permissions alone has none.
        if !file.hunks.is_empty() {
            parts.extend([(added.as_str(), Some(faces.count_added)), (removed.as_str(), Some(faces.count_removed))]);
        }
        row(Line::File(f), &parts);
        if !open(Line::File(f)) {
            continue;
        }
        for (h, hunk) in file.hunks.iter().enumerate() {
            row(Line::Hunk(f, h), &[(&hunk.header, Some(faces.hunk_heading))]);
            if !open(Line::Hunk(f, h)) {
                continue;
            }
            for (l, line) in hunk.lines.iter().enumerate() {
                row(Line::Body(f, h, l), &[(line, faces.diff_line(line))]);
            }
        }
    }
}

/// What a file's heading says of its change besides line counts: new permissions, and
/// for a binary file its size before and after (the one size of a new or deleted file).
fn note(file: &FileDiff) -> String {
    let mut note = String::new();
    if let Some((old, new)) = file.mode {
        note.push_str(&format!("  mode {:o} → {:o}", old, new));
    }
    if let Some(binary) = file.binary {
        note.push_str("  binary");
        match (binary.old.map(human_size), binary.new.map(human_size)) {
            (Some(old), Some(new)) => note.push_str(&format!(" {} → {}", old, new)),
            (Some(size), None) | (None, Some(size)) => note.push_str(&format!(" {}", size)),
            (None, None) => {}
        }
    }
    note
}

/// The row for `line` of `files` (listed under `group`, e.g. a status section), keyed by
/// what it shows so point stays on it across refreshes; `n` / `p` stop on headings.
pub fn row_spec(files: &[FileDiff], line: Line, group: impl Hash) -> RowSpec {
    let hunk_line = match line {
        Line::Body(.., l) => Some(l),
        _ => None,
    };
    let spec = RowSpec::new((group, Fold::of(files, line), hunk_line));
    if hunk_line.is_some() {
        spec.passive()
    } else {
        spec
    }
}

/// The file `line` is in and the line of it (from 0) that `line` shows in its new version:
/// a hunk's first line, or the line itself counting only lines the new version has.
pub fn target(files: &[FileDiff], line: Line) -> Option<(String, usize)> {
    let file = files.get(line.file())?;
    let Some(hunk) = line.hunk().and_then(|h| file.hunks.get(h)) else {
        return Some((file.path.clone(), 0));
    };
    let offset = match line {
        Line::Body(.., l) => hunk.lines[..l].iter().filter(|x| !x.starts_with('-')).count(),
        _ => 0,
    };
    Some((file.path.clone(), hunk.new_start().saturating_sub(1) + offset))
}

/// Visits `path` (relative to `root`) at `line`.
pub fn visit(ed: &mut Editor, root: &Path, path: &str, line: usize) {
    if let Err(e) = ed.open_file(root.join(path)) {
        ed.set_status(format!("Cannot open {}: {}", path, e));
        return;
    }
    let mut doc = ed.doc();
    let pos = doc.buf.line_to_char(line.min(doc.buf.len_lines() - 1));
    doc.jump_to(pos);
}

/// What `s`, `u` and `k` do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Stage,
    Unstage,
    Discard,
}

impl Action {
    /// Why nothing happens when there is nothing at point to act on.
    pub fn nothing_here(self) -> &'static str {
        match self {
            Action::Stage => "Nothing to stage here",
            Action::Unstage => "Nothing to unstage here",
            Action::Discard => "Nothing to discard here",
        }
    }
}

/// A git command an action runs.
pub struct Command {
    args: Vec<String>,
    stdin: Option<String>,
    /// What a discard's confirmation calls the change.
    what: String,
}

impl Command {
    pub fn new(args: Vec<String>, what: impl Into<String>) -> Self {
        Self { args, stdin: None, what: what.into() }
    }

    fn patch(args: Vec<String>, patch: String, what: &str) -> Self {
        Self { args, stdin: Some(patch), what: what.to_string() }
    }
}

/// `git <command> -- <paths>`.
pub fn path_args<'a>(command: &[&'a str], paths: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    command.iter().copied().chain(["--"]).chain(paths).map(str::to_string).collect()
}

/// How `git apply` takes a patch of changes listed under `section` for `action`: its
/// arguments, and whether they apply the patch in reverse. `--recount`, since a hunk cut
/// down to some of its lines keeps its header.
fn apply(action: Action, section: Option<Section>) -> Result<(Vec<String>, bool), &'static str> {
    Ok(match (action, section) {
        (Action::Stage, Some(Section::Unstaged)) => (args(&["apply", "--cached", "--recount", "-"]), false),
        (Action::Unstage, Some(Section::Staged)) => (args(&["apply", "--cached", "--reverse", "--recount", "-"]), true),
        (Action::Discard, Some(Section::Unstaged)) => (args(&["apply", "--reverse", "--recount", "-"]), true),
        (Action::Discard, Some(Section::Staged)) => return Err("Unstage it first (u), then discard"),
        _ => return Err(action.nothing_here()),
    })
}

/// What `action` runs on changes of `files`, listed under `section` (`None` for diffs the
/// index plays no part in, like a commit's), or why it doesn't apply: on the changed lines
/// among the lines of hunks in `selected` (the rows a selection covers, in the order they
/// show) and on those alone, else on the file or hunk `at` is in.
///
/// A renamed file is staged, unstaged and discarded as one change to both its paths,
/// while taking some of its lines back leaves the rename as it is.
pub fn command(
    action: Action,
    section: Option<Section>,
    files: &[FileDiff],
    at: Option<Line>,
    selected: &[Line],
) -> Result<Command, &'static str> {
    let (apply, reverse) = apply(action, section)?;
    let picks: Vec<_> = selected.iter().filter_map(|line| line.body()).collect();
    if !picks.is_empty() {
        let mut patch = String::new();
        for picks in picks.chunk_by(|a, b| a.0 == b.0) {
            let f = picks[0].0;
            let Some(file) = files.get(f) else { continue };
            patch.push_str(&file.patch_picked(reverse, |h, l| picks.binary_search(&(f, h, l)).is_ok()));
        }
        if patch.is_empty() {
            return Err("No changed lines in the selection");
        }
        return Ok(Command::patch(apply, patch, "the selected lines"));
    }

    let line = at.ok_or(action.nothing_here())?;
    let file = files.get(line.file()).ok_or(action.nothing_here())?;
    let path = file.path.as_str();
    let on_paths = |command: &[&str]| path_args(command, file.paths());
    Ok(match (action, line.hunk()) {
        (_, Some(h)) => Command::patch(apply, file.patch_picked(reverse, |hunk, _| hunk == h), "this hunk"),
        (Action::Stage, None) => Command::new(on_paths(&["add"]), path),
        (Action::Unstage, None) => Command::new(on_paths(&["reset", "-q"]), path),
        (Action::Discard, None) => match &file.from {
            // The index has nothing to check out at the new path: unapplying the change
            // moves the file back.
            Some(from) => {
                let what = format!("the rename of {} to {} and changes to it", from, path);
                Command::patch(apply, file.patch(), &what)
            }
            None => Command::new(args(&["checkout", "--", path]), format!("changes to {}", path)),
        },
    })
}

/// Runs `command` for `action` in `root`, after confirmation when it discards changes, or
/// says why there is none to run. Ends the selection it may have been for.
pub fn run(ed: &mut Editor, root: PathBuf, action: Action, command: Result<Command, &'static str>) {
    let Command { args, stdin, what } = match command {
        Ok(command) => command,
        Err(why) => return ed.set_status(why),
    };
    let run = move |ed: &mut Editor, done: &str| {
        ed.doc().clear_mark();
        process::run(ed, root, args, stdin, done, |_| {});
    };
    match action {
        Action::Stage => run(ed, "Staged"),
        Action::Unstage => run(ed, "Unstaged"),
        Action::Discard => ed.confirm("git-discard", format!("Discard {}? (y/n) ", what), move |ed, yes| {
            if yes {
                run(ed, "Discarded");
            } else {
                ed.set_status("Discard cancelled");
            }
        }),
    }
}
