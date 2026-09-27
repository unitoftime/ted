//! The status buffer: renders `Status` as sections with inline diffs, each line of which is
//! a row (`ted_core::rows`) for the item it shows, and implements the actions that operate
//! "at point".

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ted_core::jobs::JobHandle;
use ted_core::rows::{self, RowSpec, RowText};
use ted_core::{BufferId, Editor, FaceId};

use crate::git::args;
use crate::model::{self, Section, Status};
use crate::process;
use crate::{repo_name, GitFaces};

pub const MODE: &str = "Git Status";

/// What a status-buffer line shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Item {
    /// A line that stands for nothing: the head, blanks between sections.
    Blank,
    Section(Section),
    File(Section, usize),
    Hunk(Section, usize, usize),
    /// A line inside a hunk: (section, file, hunk, line within the hunk).
    HunkLine(Section, usize, usize, usize),
    Stash(usize),
    Commit(usize),
}

impl Item {
    /// Whether `n` / `p` stop here.
    fn is_heading(self) -> bool {
        matches!(self, Item::Section(_) | Item::File(..) | Item::Hunk(..) | Item::Stash(_) | Item::Commit(_))
    }
}

/// Identifies an item across refreshes, so point stays on "the same thing".
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ItemKey {
    Section(Section),
    File(Section, String),
    Hunk(Section, String, String),
    Stash(String),
    Commit(String),
}

/// Buffer-local state of a status buffer.
#[derive(Default)]
pub struct StatusBuffer {
    pub status: Option<Status>,
    /// The item of each row.
    items: Vec<Item>,
    expanded: HashSet<ItemKey>,
    collapsed: HashSet<ItemKey>,
    refresh_job: Option<JobHandle>,
}

impl StatusBuffer {
    fn key(&self, item: Item) -> Option<ItemKey> {
        let status = self.status.as_ref()?;
        let path = |section: Section, file: usize| match section {
            Section::Untracked => status.untracked.get(file).cloned(),
            _ => status.files(section).get(file).map(|f| f.path.clone()),
        };
        Some(match item {
            Item::Section(s) => ItemKey::Section(s),
            Item::File(s, f) => ItemKey::File(s, path(s, f)?),
            Item::Hunk(s, f, h) | Item::HunkLine(s, f, h, _) => {
                ItemKey::Hunk(s, path(s, f)?, status.files(s).get(f)?.hunks.get(h)?.header.clone())
            }
            Item::Stash(i) => ItemKey::Stash(status.stashes.get(i)?.name.clone()),
            Item::Commit(c) => ItemKey::Commit(status.recent.get(c)?.hash.clone()),
            Item::Blank => return None,
        })
    }

    /// The row showing `item`, keyed by what it shows so point stays on it across
    /// refreshes; `n` / `p` stop on headings only.
    fn row(&self, item: Item) -> RowSpec {
        let hunk_line = match item {
            Item::HunkLine(.., l) => Some(l),
            _ => None,
        };
        let spec = match self.key(item) {
            Some(key) => RowSpec::new((key, hunk_line)),
            None => RowSpec::new(item),
        };
        if item.is_heading() {
            spec
        } else {
            spec.passive()
        }
    }
}

pub fn find_buffer(ed: &Editor, root: &Path) -> Option<BufferId> {
    ed.buffers.find(|b| b.local::<StatusBuffer>().is_some() && b.directory() == root)
}

/// Opens (or reuses) the status buffer for `root` in the active window and refreshes it.
pub fn open(ed: &mut Editor, root: PathBuf) {
    let id = match find_buffer(ed, &root) {
        Some(id) => id,
        None => {
            let id = ed.new_buffer(format!("git: {}", repo_name(&root)), MODE);
            let buf = &mut ed.buffers[id];
            buf.set_text("Loading...\n");
            buf.local_mut::<StatusBuffer>();
            buf.set_directory(&root);
            id
        }
    };
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

    let is_collapsed = |key: ItemKey| state.collapsed.contains(&key);
    if !status.untracked.is_empty() {
        r.line(&[]);
        let title = format!("Untracked files ({})", status.untracked.len());
        r.item(Item::Section(Section::Untracked), &[(&title, Some(faces.section))]);
        if !is_collapsed(ItemKey::Section(Section::Untracked)) {
            for (i, path) in status.untracked.iter().enumerate() {
                r.item(Item::File(Section::Untracked, i), &[("  ", None), (path, None)]);
            }
        }
    }
    for (section, title) in [(Section::Unstaged, "Unstaged changes"), (Section::Staged, "Staged changes")] {
        let files = status.files(section);
        if files.is_empty() {
            continue;
        }
        r.line(&[]);
        let title = format!("{} ({})", title, files.len());
        r.item(Item::Section(section), &[(&title, Some(faces.section))]);
        if is_collapsed(ItemKey::Section(section)) {
            continue;
        }
        for (f, file) in files.iter().enumerate() {
            let kind = format!("{:<11}", file.kind);
            r.item(Item::File(section, f), &[(&kind, None), (&file.path, Some(faces.file_heading))]);
            if !state.expanded.contains(&ItemKey::File(section, file.path.clone())) {
                continue;
            }
            for (h, hunk) in file.hunks.iter().enumerate() {
                r.item(Item::Hunk(section, f, h), &[(&hunk.header, Some(faces.hunk_heading))]);
                if is_collapsed(ItemKey::Hunk(section, file.path.clone(), hunk.header.clone())) {
                    continue;
                }
                for (l, line) in hunk.lines.iter().enumerate() {
                    r.item(Item::HunkLine(section, f, h, l), &[(line, faces.diff_line(line))]);
                }
            }
        }
    }
    if !status.stashes.is_empty() {
        r.line(&[]);
        let title = format!("Stashes ({})", status.stashes.len());
        r.item(Item::Section(Section::Stashes), &[(&title, Some(faces.section))]);
        if !is_collapsed(ItemKey::Section(Section::Stashes)) {
            for (i, stash) in status.stashes.iter().enumerate() {
                r.item(Item::Stash(i), &[(&stash.name, Some(faces.hash)), (" ", None), (&stash.subject, None)]);
            }
        }
    }
    if !status.recent.is_empty() {
        r.line(&[]);
        r.item(Item::Section(Section::Recent), &[("Recent commits", Some(faces.section))]);
        if !is_collapsed(ItemKey::Section(Section::Recent)) {
            for (i, commit) in status.recent.iter().enumerate() {
                r.item(Item::Commit(i), &faces.commit_line(commit));
            }
        }
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
    let id = ed.active_buffer_id();
    let state = ed.buffers[id].local_mut::<StatusBuffer>();
    let Some(key) = state.key(item) else { return };
    let set = match key {
        ItemKey::File(..) => &mut state.expanded,
        _ => &mut state.collapsed,
    };
    if !set.remove(&key) {
        set.insert(key);
    }
    render(ed, id);
}

/// RET: visits the file (at the diff line's position) or shows the commit at point.
pub fn visit(ed: &mut Editor) {
    let Some((root, item)) = at_point(ed) else { return };
    let Some(status) = status_of(ed) else { return };
    let (path, line) = match item {
        Item::File(Section::Untracked, i) => (status.untracked.get(i).cloned(), 0),
        Item::File(s, f) => (status.files(s).get(f).map(|f| f.path.clone()), 0),
        Item::Hunk(s, f, h) | Item::HunkLine(s, f, h, _) => {
            let Some(file) = status.files(s).get(f) else { return };
            let hunk = &file.hunks[h];
            let offset = match item {
                // Removed lines don't exist in the new file; count only lines that do.
                Item::HunkLine(_, _, _, l) => hunk.lines[..l].iter().filter(|x| !x.starts_with('-')).count(),
                _ => 0,
            };
            (Some(file.path.clone()), hunk.new_start().saturating_sub(1) + offset)
        }
        Item::Stash(i) => {
            let name = status.stashes[i].name.clone();
            crate::diff::stash(ed, root, name);
            return;
        }
        Item::Commit(c) => {
            let hash = status.recent[c].hash.clone();
            crate::diff::commit(ed, root, hash);
            return;
        }
        _ => return,
    };
    let Some(path) = path else { return };
    if let Err(e) = ed.open_file(root.join(&path)) {
        ed.set_status(format!("Cannot open {}: {}", path, e));
        return;
    }
    let mut doc = ed.doc();
    let pos = doc.buf.line_to_char(line);
    doc.set_cursor(pos);
}

/// `s`: stages the section, file or hunk at point.
pub fn stage(ed: &mut Editor) {
    let Some((root, item)) = at_point(ed) else { return };
    let Some(status) = status_of(ed) else { return };
    let (git_args, stdin) = match item {
        Item::Section(Section::Untracked) => {
            let mut a = args(&["add", "--"]);
            a.extend(status.untracked.iter().cloned());
            (a, None)
        }
        Item::Section(Section::Unstaged) => (args(&["add", "-u"]), None),
        Item::File(Section::Untracked, i) => (args(&["add", "--", &status.untracked[i]]), None),
        Item::File(Section::Unstaged, f) => (args(&["add", "--", &status.unstaged[f].path]), None),
        Item::Hunk(Section::Unstaged, f, h) | Item::HunkLine(Section::Unstaged, f, h, _) => {
            (args(&["apply", "--cached", "-"]), Some(status.unstaged[f].patch(Some(h))))
        }
        _ => {
            ed.set_status("Nothing to stage here");
            return;
        }
    };
    process::run(ed, root, git_args, stdin, "Staged", |_| {});
}

/// `u`: unstages the section, file or hunk at point.
pub fn unstage(ed: &mut Editor) {
    let Some((root, item)) = at_point(ed) else { return };
    let Some(status) = status_of(ed) else { return };
    let (git_args, stdin) = match item {
        Item::Section(Section::Staged) => (args(&["reset", "-q"]), None),
        Item::File(Section::Staged, f) => (args(&["reset", "-q", "--", &status.staged[f].path]), None),
        Item::Hunk(Section::Staged, f, h) | Item::HunkLine(Section::Staged, f, h, _) => {
            (args(&["apply", "--cached", "--reverse", "-"]), Some(status.staged[f].patch(Some(h))))
        }
        _ => {
            ed.set_status("Nothing to unstage here");
            return;
        }
    };
    process::run(ed, root, git_args, stdin, "Unstaged", |_| {});
}

/// `k`: discards the untracked file or unstaged change at point, after confirmation.
pub fn discard(ed: &mut Editor) {
    let Some((root, item)) = at_point(ed) else { return };
    let Some(status) = status_of(ed) else { return };
    let (what, git_args, stdin, delete): (String, Vec<String>, Option<String>, Vec<String>) = match item {
        Item::Section(Section::Untracked) => ("all untracked files".into(), Vec::new(), None, status.untracked.clone()),
        Item::File(Section::Untracked, i) => {
            let path = status.untracked[i].clone();
            let what = if path.ends_with('/') { format!("{} and everything in it", path) } else { path.clone() };
            (what, Vec::new(), None, vec![path])
        }
        Item::Section(Section::Unstaged) => {
            ("all unstaged changes".into(), args(&["checkout", "--", "."]), None, Vec::new())
        }
        Item::File(Section::Unstaged, f) => {
            let path = status.unstaged[f].path.clone();
            (format!("changes to {}", path), args(&["checkout", "--", &path]), None, Vec::new())
        }
        Item::Hunk(Section::Unstaged, f, h) | Item::HunkLine(Section::Unstaged, f, h, _) => (
            "this hunk".into(),
            args(&["apply", "--reverse", "-"]),
            Some(status.unstaged[f].patch(Some(h))),
            Vec::new(),
        ),
        Item::File(Section::Staged, _) | Item::Hunk(Section::Staged, ..) | Item::HunkLine(Section::Staged, ..) => {
            ed.set_status("Unstage it first (u), then discard");
            return;
        }
        Item::Stash(i) => {
            crate::menus::drop_stash(ed, root, status.stashes[i].name.clone());
            return;
        }
        _ => {
            ed.set_status("Nothing to discard here");
            return;
        }
    };
    ed.confirm("git-discard", format!("Discard {}? (y/n) ", what), move |ed, yes| {
        if !yes {
            ed.set_status("Discard cancelled");
            return;
        }
        if !delete.is_empty() {
            let failed: Vec<_> = delete.iter().filter(|p| delete_untracked(&root.join(p)).is_err()).collect();
            ed.set_status(if failed.is_empty() {
                "Deleted".to_string()
            } else {
                format!("Could not delete {:?}", failed)
            });
            refresh(ed, &root);
        } else {
            process::run(ed, root, git_args, stdin, "Discarded", |_| {});
        }
    });
}

/// Deletes an untracked file, or a whole untracked directory (listed as `dir/`).
fn delete_untracked(path: &Path) -> std::io::Result<()> {
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

/// Whether the status at point has anything staged.
pub fn has_staged(ed: &Editor) -> bool {
    status_of(ed).is_some_and(|s| !s.staged.is_empty())
}
