//! The status buffer: renders `Status` as sections with inline diffs (`changes`), each
//! line of which is a row (`ted_core::rows`) for the item it shows, and implements the
//! actions that operate "at point".

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ted_core::jobs::JobHandle;
use ted_core::rows::{self, RowSpec, RowText};
use ted_core::{BufferId, BufferScope, Editor, FaceId};

use crate::changes::{self, Action, Command, Fold, Line};
use crate::diff::{self, Source};
use crate::git::args;
use crate::model::{self, FileDiff, Section, Status};
use crate::{generated_buffer, GitFaces};

pub const MODE: &str = "Git Status";

/// What a status-buffer line shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Item {
    /// A line that stands for nothing: the head, blanks between sections.
    Blank,
    Section(Section),
    Untracked(usize),
    /// A line of the unstaged or staged diff.
    Change(Section, Line),
    Stash(usize),
    Commit(usize),
}

/// Identifies an item across refreshes, so point and folds stay on "the same thing".
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ItemKey {
    Section(Section),
    Untracked(String),
    Change(Section, Fold),
    Stash(String),
    Commit(String),
}

/// Buffer-local state of a status buffer.
#[derive(Default)]
pub struct StatusBuffer {
    pub status: Option<Status>,
    /// The item of each row.
    items: Vec<Item>,
    /// Sections and folds `TAB` turned from how they start: sections and hunks open,
    /// files closed.
    toggled: HashSet<ItemKey>,
    refresh_job: Option<JobHandle>,
}

impl StatusBuffer {
    fn key(&self, item: Item) -> Option<ItemKey> {
        let status = self.status.as_ref()?;
        Some(match item {
            Item::Section(s) => ItemKey::Section(s),
            Item::Untracked(i) => ItemKey::Untracked(status.untracked.get(i)?.clone()),
            Item::Change(s, line) => ItemKey::Change(s, Fold::of(status.files(s), line)?),
            Item::Stash(i) => ItemKey::Stash(status.stashes.get(i)?.name.clone()),
            Item::Commit(c) => ItemKey::Commit(status.recent.get(c)?.hash.clone()),
            Item::Blank => return None,
        })
    }

    /// Whether what is under `item` shows.
    fn is_open(&self, item: Item) -> bool {
        let starts_open = !matches!(item, Item::Change(_, Line::File(_)));
        self.key(item).is_none_or(|key| starts_open != self.toggled.contains(&key))
    }

    /// The row showing `item`, keyed by what it shows so point stays on it across
    /// refreshes; `n` / `p` stop on headings only.
    fn row(&self, item: Item) -> RowSpec {
        match item {
            Item::Change(section, line) => {
                let files = self.status.as_ref().map_or(&[][..], |s| s.files(section));
                changes::row_spec(files, line, section)
            }
            _ => RowSpec::new(self.key(item).ok_or(item)),
        }
    }
}

pub fn find_buffer(ed: &Editor, root: &Path) -> Option<BufferId> {
    ed.find_generated(MODE, BufferScope::Dir(root))
}

/// Opens (or reuses) the status buffer for `root` in the active window and refreshes it.
pub fn open(ed: &mut Editor, root: PathBuf) {
    let id = generated_buffer(ed, "git status", MODE, &root);
    let buf = &mut ed.buffers[id];
    if buf.local::<StatusBuffer>().is_none() {
        buf.set_text("Loading...\n");
        buf.local_mut::<StatusBuffer>();
    }
    ed.show_buffer(id);
    refresh(ed, &root);
}

/// Reloads the status of `root` in the background and re-renders its buffer.
pub fn refresh(ed: &mut Editor, root: &Path) {
    let Some(id) = find_buffer(ed, root) else { return };
    let root = root.to_path_buf();
    let job = ed.spawn(move |ctx| {
        let result = model::load(&root);
        ctx.send(move |ed| match result {
            Ok(status) => {
                if let Some(state) = ed.buffers.get_mut(id).map(|b| b.local_mut::<StatusBuffer>()) {
                    state.status = Some(status);
                    render(ed, id);
                }
            }
            Err(e) => ed.set_status(format!("git status failed: {}", e)),
        });
    });
    let state = ed.buffers[id].local_mut::<StatusBuffer>();
    if let Some(previous) = state.refresh_job.replace(job) {
        previous.cancel();
    }
}

/// Builds the status text, recording the item of each row.
struct Renderer<'a> {
    state: &'a StatusBuffer,
    text: RowText,
    items: Vec<Item>,
}

impl Renderer<'_> {
    /// Appends a line that stands for no item (the head, blanks).
    fn line(&mut self, parts: &[(&str, Option<FaceId>)]) {
        self.text.line(parts);
    }

    fn item(&mut self, item: Item, parts: &[(&str, Option<FaceId>)]) {
        self.text.row(self.state.row(item), parts);
        self.items.push(item);
    }

    /// A section's heading, then (unless it is collapsed) what `rows` writes under it.
    fn section(&mut self, section: Section, title: &str, faces: &GitFaces, rows: impl FnOnce(&mut Self)) {
        self.line(&[]);
        self.item(Item::Section(section), &[(title, Some(faces.section))]);
        if self.state.is_open(Item::Section(section)) {
            rows(self);
        }
    }
}

fn render(ed: &mut Editor, id: BufferId) {
    let faces = *ed.ext_mut::<GitFaces>();
    let Some(state) = ed.buffers.get(id).and_then(|b| b.local::<StatusBuffer>()) else { return };
    let Some(status) = state.status.as_ref() else { return };

    let mut r = Renderer { state, text: RowText::new(), items: Vec::new() };
    let head = &status.head;
    let branch = head.branch.as_deref().unwrap_or("(detached)");
    r.line(&[("Head:     ", None), (branch, Some(faces.branch_local)), ("  ", None), (&head.subject, None)]);
    if let Some(upstream) = &head.upstream {
        let counts = match (head.ahead, head.behind) {
            (0, 0) => String::new(),
            (a, b) => format!("  (ahead {}, behind {})", a, b),
        };
        r.line(&[("Upstream: ", None), (upstream, Some(faces.branch_remote)), (&counts, None)]);
    }
    if let Some(rebase) = &head.rebase {
        let branch = (rebase.branch.as_deref().unwrap_or("(detached)"), Some(faces.branch_local));
        let progress = format!("  ({}/{})", rebase.step, rebase.steps);
        let onto = (rebase.onto.as_str(), Some(faces.branch_local));
        r.line(&[("Rebasing: ", None), branch, (" onto ", None), onto, (&progress, None)]);
    }
    if let Some(merging) = &head.merging {
        r.line(&[("Merging:  ", None), (merging, Some(faces.branch_local))]);
    }

    if !status.untracked.is_empty() {
        let title = format!("Untracked files ({})", status.untracked.len());
        r.section(Section::Untracked, &title, &faces, |r| {
            for (i, path) in status.untracked.iter().enumerate() {
                r.item(Item::Untracked(i), &[("  ", None), (path, None)]);
            }
        });
    }
    for (section, title) in [(Section::Unstaged, "Unstaged changes"), (Section::Staged, "Staged changes")] {
        let files = status.files(section);
        if files.is_empty() {
            continue;
        }
        let title = format!("{} ({})", title, files.len());
        r.section(section, &title, &faces, |r| {
            let open = |line| state.is_open(Item::Change(section, line));
            changes::write(files, &faces, open, |line, parts| r.item(Item::Change(section, line), parts));
        });
    }
    if !status.stashes.is_empty() {
        let title = format!("Stashes ({})", status.stashes.len());
        r.section(Section::Stashes, &title, &faces, |r| {
            for (i, stash) in status.stashes.iter().enumerate() {
                r.item(Item::Stash(i), &[(&stash.name, Some(faces.hash)), (" ", None), (&stash.subject, None)]);
            }
        });
    }
    if !status.recent.is_empty() {
        r.section(Section::Recent, "Recent commits", &faces, |r| {
            for (i, commit) in status.recent.iter().enumerate() {
                r.item(Item::Commit(i), &faces.commit_line(commit));
            }
        });
    }

    let Renderer { text, items, .. } = r;
    ed.buffers[id].local_mut::<StatusBuffer>().items = items;
    text.install(ed, id, "git");
}

/// The status buffer's root and the item at point, if the active buffer is a status buffer.
pub fn at_point(ed: &Editor) -> Option<(PathBuf, Item)> {
    let buf = ed.active_buffer();
    let state = buf.local::<StatusBuffer>()?;
    let item = rows::at_point(ed).and_then(|row| state.items.get(row).copied());
    Some((buf.directory(), item.unwrap_or(Item::Blank)))
}

fn status_of(ed: &Editor) -> Option<&Status> {
    ed.active_buffer().local::<StatusBuffer>()?.status.as_ref()
}

/// TAB: expands or collapses the section, file or hunk at point.
pub fn toggle(ed: &mut Editor) {
    let Some((_, item)) = at_point(ed) else { return };
    if !matches!(item, Item::Section(_) | Item::Change(..)) {
        return;
    }
    let id = ed.active_buffer_id();
    let state = ed.buffers[id].local_mut::<StatusBuffer>();
    let Some(key) = state.key(item) else { return };
    if !state.toggled.remove(&key) {
        state.toggled.insert(key);
    }
    render(ed, id);
}

/// RET: visits the file (at the diff line's position) or shows the stash or commit at
/// point.
pub fn visit(ed: &mut Editor) {
    let Some((root, item)) = at_point(ed) else { return };
    let Some(status) = status_of(ed) else { return };
    let target = match item {
        Item::Untracked(i) => status.untracked.get(i).map(|path| (path.clone(), 0)),
        Item::Change(section, line) => changes::target(status.files(section), line),
        Item::Stash(i) => return diff::show(ed, root, Source::Stash(status.stashes[i].name.clone())),
        Item::Commit(c) => return diff::show(ed, root, Source::Commit(status.recent[c].hash.clone())),
        _ => None,
    };
    if let Some((path, line)) = target {
        changes::visit(ed, &root, &path, line);
    }
}

/// The lines of hunks among `items` (the rows a selection covers), with the section they
/// are in: that of the first, when the selection runs on into another.
fn selected_lines(items: &[Item]) -> Option<(Section, Vec<Line>)> {
    let mut lines = items
        .iter()
        .filter_map(|item| match *item {
            Item::Change(section, line @ Line::Body(..)) => Some((section, line)),
            _ => None,
        })
        .peekable();
    let section = lines.peek()?.0;
    Some((section, lines.take_while(|(s, _)| *s == section).map(|(_, line)| line).collect()))
}

/// `s` / `u` / `k`: stages, unstages or discards the section, file or hunk at point, or
/// the changed lines of hunks the selection covers. Discarding a stash drops it.
pub fn act(ed: &mut Editor, action: Action) {
    let selected = rows::in_region(ed).unwrap_or_default();
    let Some((root, item)) = at_point(ed) else { return };
    let Some(state) = ed.active_buffer().local::<StatusBuffer>() else { return };
    let Some(status) = state.status.as_ref() else { return };
    if let Some((section, lines)) = selected_lines(state.items.get(selected).unwrap_or_default()) {
        let command = changes::command(action, Some(section), status.files(section), None, &lines);
        return changes::run(ed, root, action, command);
    }
    let command = match (action, item) {
        (Action::Stage, Item::Section(Section::Untracked)) => {
            let mut a = args(&["add", "--"]);
            a.extend(status.untracked.iter().cloned());
            Ok(Command::new(a, "all untracked files"))
        }
        (Action::Stage, Item::Untracked(i)) => Ok(Command::new(args(&["add", "--", &status.untracked[i]]), "")),
        (Action::Stage, Item::Section(Section::Unstaged)) => Ok(Command::new(stage_all_args(&status.unstaged), "")),
        (Action::Unstage, Item::Section(Section::Staged)) => Ok(Command::new(args(&["reset", "-q"]), "")),
        (Action::Discard, Item::Section(Section::Unstaged)) => {
            Ok(Command::new(args(&["checkout", "--", "."]), "all unstaged changes"))
        }
        (Action::Discard, Item::Section(Section::Untracked)) => {
            let paths = status.untracked.clone();
            return delete_untracked(ed, root, "all untracked files".into(), paths);
        }
        (Action::Discard, Item::Untracked(i)) => {
            let path = status.untracked[i].clone();
            let what = if path.ends_with('/') { format!("{} and everything in it", path) } else { path.clone() };
            return delete_untracked(ed, root, what, vec![path]);
        }
        (Action::Discard, Item::Stash(i)) => {
            return crate::menus::drop_stash(ed, root, status.stashes[i].name.clone());
        }
        (_, Item::Change(section, line)) => {
            changes::command(action, Some(section), status.files(section), Some(line), &[])
        }
        _ => Err(action.nothing_here()),
    };
    changes::run(ed, root, action, command);
}

/// `git add` arguments staging every change in `unstaged`: `git add -u`, unless there are
/// renames, whose new paths it would leave untracked. Then the paths are named.
fn stage_all_args(unstaged: &[FileDiff]) -> Vec<String> {
    if unstaged.iter().all(|file| file.from.is_none()) {
        return args(&["add", "-u"]);
    }
    changes::path_args(&["add"], unstaged.iter().flat_map(FileDiff::paths))
}

/// `S`: stages all changes to tracked files of `root`, as its status buffer (if it has
/// one) shows them.
pub fn stage_all(ed: &mut Editor, root: PathBuf) {
    let status = find_buffer(ed, &root).and_then(|id| ed.buffers[id].local::<StatusBuffer>()?.status.as_ref());
    let args = stage_all_args(status.map_or(&[], |status| &status.unstaged));
    crate::process::run(ed, root, args, None, "Staged all", |_| {});
}

/// Deletes untracked `paths` (a directory listed as `dir/` with everything in it) after
/// confirmation.
fn delete_untracked(ed: &mut Editor, root: PathBuf, what: String, paths: Vec<String>) {
    ed.confirm("git-discard", format!("Discard {}? (y/n) ", what), move |ed, yes| {
        if !yes {
            ed.set_status("Discard cancelled");
            return;
        }
        let failed: Vec<_> = paths.iter().filter(|p| delete(&root.join(p)).is_err()).collect();
        ed.set_status(if failed.is_empty() { "Deleted".to_string() } else { format!("Could not delete {:?}", failed) });
        refresh(ed, &root);
    });
}

fn delete(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// The stash on the current line, if the active buffer is a status buffer showing one.
pub fn stash_at_point(ed: &mut Editor) -> Option<String> {
    let (_, Item::Stash(i)) = at_point(ed)? else { return None };
    Some(status_of(ed)?.stashes.get(i)?.name.clone())
}

/// The commit on the current line, if the active buffer is a status buffer showing one.
pub fn commit_at_point(ed: &Editor) -> Option<String> {
    let (_, Item::Commit(c)) = at_point(ed)? else { return None };
    Some(status_of(ed)?.recent.get(c)?.hash.clone())
}

/// Whether the status at point has anything staged.
pub fn has_staged(ed: &Editor) -> bool {
    status_of(ed).is_some_and(|s| !s.staged.is_empty())
}
