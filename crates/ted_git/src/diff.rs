//! Diff buffers: `d` menu diffs and commit views. `RET` on a diff line visits that line in
//! the working tree; `g` reruns the same git command.

use std::path::PathBuf;

use ted_core::{Editor, StyledText};

use crate::git::args;
use crate::model::Section;
use crate::status::{self, Item};
use crate::{generated_buffer, process, repo_name, GitFaces};

pub const MODE: &str = "Git Diff";

/// Buffer-local: the git command that produced this diff.
#[derive(Default)]
struct DiffBuffer {
    args: Vec<String>,
}

/// Runs `git <git_args>` and shows the output as a colored diff in the repo's diff buffer.
pub fn show(ed: &mut Editor, root: PathBuf, what: String, git_args: Vec<String>) {
    let query_args = git_args.clone();
    process::query(ed, root.clone(), query_args, move |ed, out| {
        let faces = *ed.ext_mut::<GitFaces>();
        let text = if out.is_empty() { "No changes.\n".to_string() } else { out };
        let id = generated_buffer(ed, &format!("git-diff: {}", repo_name(&root)), MODE, &root);
        let buf = &mut ed.buffers[id];
        buf.set_styled("git", styled(&text, &faces));
        buf.local_mut::<DiffBuffer>().args = git_args;
        ed.show_buffer(id);
        ed.doc().set_cursor(0);
        ed.set_status(what);
    });
}

fn styled(text: &str, faces: &GitFaces) -> StyledText {
    let mut styled = StyledText::new();
    for line in text.split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        let face = if content.starts_with("commit ") {
            Some(faces.hash)
        } else if content.starts_with("diff --git") || content.starts_with("+++ ") || content.starts_with("--- ") {
            Some(faces.file_heading)
        } else {
            faces.diff_line(content)
        };
        styled.push(content, face);
        styled.push(&line[content.len()..], None);
    }
    styled
}

/// `g` in a diff buffer: reruns its git command.
pub fn refresh(ed: &mut Editor) {
    let buf = ed.active_buffer();
    let Some(diff) = buf.local::<DiffBuffer>() else { return };
    let (git_args, root) = (diff.args.clone(), buf.directory());
    show(ed, root, "Refreshed".into(), git_args);
}

/// `RET`: visits the working-tree line of the diff line at point.
pub fn visit(ed: &mut Editor) {
    let root = ed.active_buffer().directory();
    let doc = ed.doc();
    let current = doc.buf.char_to_line(doc.pos());
    let line_text = |l: usize| doc.buf.line_content(l).to_string();

    // Walk up to the enclosing hunk header and file header.
    let (mut new_lines, mut hunk_start, mut path) = (0, None, None);
    for l in (0..=current).rev() {
        let text = line_text(l);
        if hunk_start.is_none() {
            if text.starts_with("@@") {
                hunk_start = Some(new_start(&text));
            } else if l != current && !text.starts_with('-') {
                new_lines += 1;
            }
        }
        if let Some(p) = text.strip_prefix("+++ b/") {
            path = Some(p.to_string());
            break;
        }
        if text.starts_with("diff --git") {
            path = text.rsplit_once(" b/").map(|(_, p)| p.to_string());
            break;
        }
    }
    let Some(path) = path else { return };
    let line = hunk_start.map_or(0, |start| start.saturating_sub(1) + new_lines);
    if let Err(e) = ed.open_file(root.join(&path)) {
        ed.set_status(format!("Cannot open {}: {}", path, e));
        return;
    }
    let mut doc = ed.doc();
    let pos = doc.buf.line_to_char(line);
    doc.set_cursor(pos);
}

fn new_start(header: &str) -> usize {
    crate::model::Hunk { header: header.to_string(), lines: Vec::new() }.new_start()
}

fn diff_args(extra: &[&str]) -> Vec<String> {
    let mut a = args(&["diff", "--no-ext-diff"]);
    a.extend(extra.iter().map(|s| s.to_string()));
    a
}

pub fn unstaged(ed: &mut Editor, root: PathBuf) {
    show(ed, root, "Unstaged changes".into(), diff_args(&[]));
}

pub fn staged(ed: &mut Editor, root: PathBuf) {
    show(ed, root, "Staged changes".into(), diff_args(&["--cached"]));
}

pub fn worktree(ed: &mut Editor, root: PathBuf) {
    show(ed, root, "Changes since HEAD".into(), diff_args(&["HEAD"]));
}

pub fn commit(ed: &mut Editor, root: PathBuf, rev: String) {
    let what = format!("Commit {}", rev);
    show(ed, root, what, args(&["show", "--stat", "-p", "--no-ext-diff", &rev]));
}

pub fn stash(ed: &mut Editor, root: PathBuf, name: String) {
    let what = format!("Stash {}", name);
    show(ed, root, what, args(&["stash", "show", "--stat", "-p", "--no-ext-diff", &name]));
}

pub fn range(ed: &mut Editor, root: PathBuf, range: String) {
    let what = format!("Diff {}", range);
    show(ed, root, what, diff_args(&[&range]));
}

/// `d d`: the diff of whatever is at point in the status buffer.
pub fn dwim(ed: &mut Editor, root: PathBuf) {
    let Some((_, item)) = status::at_point(ed) else {
        unstaged(ed, root);
        return;
    };
    let status = ed.active_buffer().local::<status::StatusBuffer>().and_then(|s| s.status.clone());
    let Some(status) = status else { return };
    match item {
        Item::Section(Section::Staged) => staged(ed, root),
        Item::File(section @ (Section::Unstaged | Section::Staged), f)
        | Item::Hunk(section, f, _)
        | Item::HunkLine(section, f, _, _) => {
            let Some(path) = status.files(section).get(f).map(|f| f.path.clone()) else { return };
            let cached: &[&str] = if section == Section::Staged { &["--cached", "--"] } else { &["--"] };
            let mut extra = cached.to_vec();
            extra.push(&path);
            show(ed, root, format!("Changes to {}", path), diff_args(&extra));
        }
        Item::Stash(i) => {
            let name = status.stashes[i].name.clone();
            stash(ed, root, name);
        }
        Item::Commit(c) => {
            let hash = status.recent[c].hash.clone();
            commit(ed, root, hash);
        }
        _ => unstaged(ed, root),
    }
}
