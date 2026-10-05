//! Diff buffers: the `d` menu's diffs and commit views, written like the status buffer's
//! sections (`changes`): a heading per file with its hunks under it, under a commit's
//! details or a title. `TAB` folds a file or hunk, `n` / `p` step between them, `RET`
//! visits the line in the working tree, `s` / `u` / `k` stage, unstage or discard in
//! diffs of unstaged or staged changes, and `g` reruns the diff.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ted_core::jobs::JobHandle;
use ted_core::rows::{self, RowText};
use ted_core::{BufferId, BufferScope, Editor, FaceId};

use crate::changes::{self, Action, Fold, Line};
use crate::git::git;
use crate::model::{self, diff_args, patch_args, CommitDetails, FileDiff, Section};
use crate::renames;
use crate::status::{self, Item};
use crate::{generated_buffer, GitFaces};

pub const MODE: &str = "Git Diff";

/// What a diff buffer shows, and how to show it again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Unstaged,
    Staged,
    /// The working tree against HEAD.
    Worktree,
    /// One file's unstaged or staged changes: its path, after the old one if it is renamed.
    File(Section, Vec<String>),
    Range(String),
    Commit(String),
    Stash(String),
}

impl Source {
    /// The status section the changes are in, where staging and unstaging apply.
    fn section(&self) -> Option<Section> {
        match self {
            Source::Unstaged => Some(Section::Unstaged),
            Source::Staged => Some(Section::Staged),
            Source::File(section, _) => Some(*section),
            _ => None,
        }
    }

    fn args(&self) -> Vec<String> {
        let file = |spec: &[&str], paths: &[String]| {
            diff_args(&spec.iter().copied().chain(paths.iter().map(String::as_str)).collect::<Vec<_>>())
        };
        match self {
            Source::Unstaged => diff_args(&[]),
            Source::Staged => diff_args(&["--cached"]),
            Source::Worktree => diff_args(&["HEAD"]),
            Source::File(Section::Staged, paths) => file(&["--cached", "--"], paths),
            Source::File(_, paths) => file(&["--"], paths),
            Source::Range(range) => diff_args(&[range]),
            Source::Commit(rev) => patch_args(&["show", "--format=", "--diff-merges=first-parent"], &[rev]),
            Source::Stash(name) => patch_args(&["stash", "show", "-p"], &[name]),
        }
    }

    /// The files the source's diff changes. Runs git; call from a job.
    fn files(&self, root: &Path) -> Result<Vec<FileDiff>, String> {
        let args = self.args();
        match self {
            // Against the working tree, where git needs help to see a rename.
            Source::Unstaged | Source::Worktree | Source::File(Section::Unstaged, _) => {
                renames::worktree_diff(root, &args, None)
            }
            _ => model::load_diff(root, &args),
        }
    }

    /// The commit whose details head the diff.
    fn commit(&self) -> Option<&str> {
        match self {
            Source::Commit(rev) | Source::Stash(rev) => Some(rev),
            _ => None,
        }
    }

    fn title(&self) -> String {
        match self {
            Source::Unstaged => "Unstaged changes".into(),
            Source::Staged => "Staged changes".into(),
            Source::Worktree => "Changes since HEAD".into(),
            Source::File(Section::Staged, paths) => format!("Staged changes to {}", paths.join(" → ")),
            Source::File(_, paths) => format!("Unstaged changes to {}", paths.join(" → ")),
            Source::Range(range) => format!("Diff {}", range),
            Source::Commit(rev) => format!("Commit {}", rev),
            Source::Stash(name) => format!("Stash {}", name),
        }
    }
}

/// Buffer-local state of a diff buffer.
#[derive(Default)]
struct DiffBuffer {
    source: Option<Source>,
    commit: Option<CommitDetails>,
    files: Vec<FileDiff>,
    /// The diff line of each row.
    lines: Vec<Line>,
    /// Files and hunks `TAB` closed.
    folded: HashSet<Fold>,
    job: Option<JobHandle>,
}

impl DiffBuffer {
    fn is_open(&self, line: Line) -> bool {
        Fold::of(&self.files, line).is_none_or(|fold| !self.folded.contains(&fold))
    }
}

/// Shows `source` in the repository's diff buffer.
pub fn show(ed: &mut Editor, root: PathBuf, source: Source) {
    let id = generated_buffer(ed, "git diff", MODE, &root);
    load(ed, id, root, source, true);
}

/// Reruns the diff of `root`'s diff buffer, if it has one (after staging changed it).
pub fn refresh(ed: &mut Editor, root: PathBuf) {
    let found = ed.find_generated(MODE, BufferScope::Dir(&root));
    let Some((id, source)) = found.and_then(|id| Some((id, ed.buffers[id].local::<DiffBuffer>()?.source.clone()?)))
    else {
        return;
    };
    load(ed, id, root, source, false);
}

/// `g`: reruns the active diff buffer's diff.
pub fn rerun(ed: &mut Editor) {
    if ed.active_buffer().local::<DiffBuffer>().is_some() {
        refresh(ed, ed.active_buffer().directory());
    }
}

/// Runs `source`'s git commands in the background and writes the result into buffer `id`,
/// showing it from the top if `show`, else keeping point on what it was on.
fn load(ed: &mut Editor, id: BufferId, root: PathBuf, source: Source, show: bool) {
    let job = ed.spawn(move |ctx| {
        let commit = source.commit().map(|rev| git(&root, &model::details_args(rev), None)).transpose();
        let files = source.files(&root);
        ctx.send(move |ed| {
            let (commit, files) = match (commit, files) {
                (Ok(commit), Ok(files)) => (commit.and_then(|out| model::parse_details(&out)), files),
                (Err(e), _) | (_, Err(e)) => return ed.set_status(format!("git failed: {}", e)),
            };
            let Some(buf) = ed.buffers.get_mut(id) else { return };
            let state = buf.local_mut::<DiffBuffer>();
            let fresh = state.source.as_ref() != Some(&source);
            if fresh {
                state.folded.clear();
            }
            let title = source.title();
            (state.source, state.commit, state.files) = (Some(source), commit, files);
            if show {
                ed.show_buffer(id);
                ed.set_status(title);
            }
            render(ed, id, fresh || show);
        });
    });
    if let Some(previous) = ed.buffers[id].local_mut::<DiffBuffer>().job.replace(job) {
        previous.cancel();
    }
}

/// Writes the diff buffer's text: from the top if `fresh`, else keeping point on its item.
fn render(ed: &mut Editor, id: BufferId, fresh: bool) {
    let faces = *ed.ext_mut::<GitFaces>();
    let Some(state) = ed.buffers.get(id).and_then(|b| b.local::<DiffBuffer>()) else { return };
    let Some(source) = &state.source else { return };

    let mut text = RowText::new();
    match &state.commit {
        Some(details) => write_commit(&mut text, details, &faces),
        None => {
            let files = match state.files.len() {
                1 => " (1 file)".to_string(),
                n => format!(" ({} files)", n),
            };
            text.line(&[(&source.title(), Some(faces.section)), (&files, None)]);
        }
    }
    text.line(&[]);
    if state.files.is_empty() {
        text.line(&[("No changes", Some(FaceId::SHADOW))]);
    }
    let mut lines = Vec::new();
    changes::write(
        &state.files,
        &faces,
        |line| state.is_open(line),
        |line, parts| {
            text.row(changes::row_spec(&state.files, line, ()), parts);
            lines.push(line);
        },
    );

    ed.buffers[id].local_mut::<DiffBuffer>().lines = lines;
    if fresh {
        text.install_fresh(ed, id, "git");
    } else {
        text.install(ed, id, "git");
    }
}

/// A commit's summary line, author and date, then the rest of its message.
fn write_commit(text: &mut RowText, details: &CommitDetails, faces: &GitFaces) {
    text.line(&faces.commit_line(&details.commit));
    let author = format!("{} <{}>, {} ({})", details.commit.author, details.email, details.date, details.commit.date);
    text.line(&[(&author, Some(FaceId::SHADOW))]);
    if !details.body.is_empty() {
        text.line(&[]);
        for line in details.body.lines() {
            text.line(&[(line, None)]);
        }
    }
}

/// The active diff buffer's state and the diff line at point.
fn at_point(ed: &Editor) -> Option<(&DiffBuffer, Line)> {
    let state = ed.active_buffer().local::<DiffBuffer>()?;
    Some((state, *state.lines.get(rows::at_point(ed)?)?))
}

/// TAB: folds or unfolds the file or hunk at point.
pub fn toggle(ed: &mut Editor) {
    let Some(fold) = at_point(ed).and_then(|(state, line)| Fold::of(&state.files, line)) else { return };
    let id = ed.active_buffer_id();
    let folded = &mut ed.buffers[id].local_mut::<DiffBuffer>().folded;
    if !folded.remove(&fold) {
        folded.insert(fold);
    }
    render(ed, id, false);
}

/// RET: visits the working-tree line of the diff line at point.
pub fn visit(ed: &mut Editor) {
    let Some((path, line)) = at_point(ed).and_then(|(state, line)| changes::target(&state.files, line)) else {
        return;
    };
    let root = ed.active_buffer().directory();
    changes::visit(ed, &root, &path, line);
}

/// `s` / `u` / `k`: stages, unstages or discards the file or hunk at point, in a diff of
/// unstaged or staged changes.
pub fn act(ed: &mut Editor, action: Action) {
    let Some((state, line)) = at_point(ed) else {
        ed.set_status(action.nothing_here());
        return;
    };
    let section = state.source.as_ref().and_then(Source::section);
    match changes::command(action, section, &state.files, line) {
        Ok(command) => {
            let root = ed.active_buffer().directory();
            changes::run(ed, root, action, command);
        }
        Err(why) => ed.set_status(why),
    }
}

/// `d d`: the diff of whatever is at point in the status buffer.
pub fn dwim(ed: &mut Editor, root: PathBuf) {
    let status = status::at_point(ed).and_then(|(_, item)| {
        let status = ed.active_buffer().local::<status::StatusBuffer>()?.status.as_ref()?;
        Some((item, status))
    });
    let source = match status {
        Some((Item::Section(Section::Staged), _)) => Source::Staged,
        Some((Item::Change(section, line), status)) => match status.files(section).get(line.file()) {
            Some(file) => Source::File(section, file.paths().map(str::to_string).collect()),
            None => return,
        },
        Some((Item::Stash(i), status)) => Source::Stash(status.stashes[i].name.clone()),
        Some((Item::Commit(c), status)) => Source::Commit(status.recent[c].hash.clone()),
        _ => Source::Unstaged,
    };
    show(ed, root, source);
}
