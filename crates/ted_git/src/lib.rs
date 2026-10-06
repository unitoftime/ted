//! The built-in git interface, written purely against `ted_core`'s plugin API.
//!
//! `C-x g` opens the status buffer for the current repository:
//!
//! | Key | Action |
//! |---|---|
//! | `TAB` | expand/collapse the section, file or hunk at point |
//! | `n` / `p` | next/previous item |
//! | `RET` | visit the file at point (at the diff line), or show the commit |
//! | `s` / `u` | stage/unstage the section, file or hunk at point, or the selected lines of hunks |
//! | `S` / `U` | stage all tracked changes / unstage everything |
//! | `k` | discard the untracked file or directory, or unstaged change, at point, or the selected lines of hunks |
//! | `c` `P` `F` `f` `b` `d` `z` `x` | commit, push, pull, fetch, branch, diff, stash and reset menus |
//! | `l` | log; RET shows a commit |
//! | `h` / `?` | popup of the commands available here (runs them too) |
//! | `$` | the git process log |
//! | `g` / `q` | refresh / quit window |
//!
//! `h` / `?`, `n` / `p`, `g` and `q` come from the `special` keymap every git buffer
//! inherits. Diff buffers (`d`, and RET on a commit) show files and hunks the same way,
//! with `TAB`, `RET` and `s` / `u` / `k` working as in the status buffer.
//!
//! `M-x git-blame` annotates a file buffer in place with the commit behind each line.

mod blame;
mod changes;
mod commit;
mod diff;
mod git;
mod log;
mod menus;
mod model;
mod process;
mod renames;
mod status;

use std::path::{Path, PathBuf};

use ted_core::{BufferId, BufferScope, Editor, Face, FaceId, Faces, KeymapDef, Mode, Plugin};

use crate::changes::Action;
use crate::git::args;
use crate::model::{Commit, RefKind};

pub struct GitPlugin;

/// Faces the plugin registers; themes can override them by name.
#[derive(Clone, Copy, Default)]
pub(crate) struct GitFaces {
    pub section: FaceId,
    pub branch_local: FaceId,
    pub branch_remote: FaceId,
    pub hash: FaceId,
    pub file_heading: FaceId,
    pub file_note: FaceId,
    pub hunk_heading: FaceId,
    pub added: FaceId,
    pub removed: FaceId,
    pub count_added: FaceId,
    pub count_removed: FaceId,
    pub badge_current: FaceId,
    pub badge_local: FaceId,
    pub badge_remote: FaceId,
    pub badge_tag: FaceId,
}

impl GitFaces {
    fn register(faces: &mut Faces) -> Self {
        use ted_core::Color;
        let rgb = Color::rgb;
        Self {
            section: faces.register("git-section-heading", Face::fg(rgb(220, 180, 80)).bold()),
            branch_local: faces.register("git-branch-local", Face::fg(rgb(100, 180, 255))),
            branch_remote: faces.register("git-branch-remote", Face::fg(rgb(130, 200, 120))),
            hash: faces.register("git-hash", Face::fg(rgb(140, 140, 150))),
            file_heading: faces.register("git-diff-file-heading", Face::default().bold()),
            file_note: faces.register("git-diff-file-note", Face::fg(rgb(140, 140, 150))),
            hunk_heading: faces.register("git-diff-hunk-heading", Face::fg_bg(rgb(190, 190, 210), rgb(50, 55, 70))),
            added: faces.register("git-diff-added", Face::fg_bg(rgb(130, 210, 130), rgb(30, 55, 35))),
            removed: faces.register("git-diff-removed", Face::fg_bg(rgb(235, 120, 120), rgb(65, 32, 35))),
            count_added: faces.register("git-count-added", Face::fg(rgb(130, 210, 130))),
            count_removed: faces.register("git-count-removed", Face::fg(rgb(235, 120, 120))),
            badge_current: faces
                .register("git-badge-current", Face::fg_bg(rgb(150, 205, 255), rgb(35, 62, 100)).bold()),
            badge_local: faces.register("git-badge-local", Face::fg_bg(rgb(100, 180, 255), rgb(30, 45, 68))),
            badge_remote: faces.register("git-badge-remote", Face::fg_bg(rgb(130, 200, 120), rgb(30, 55, 35))),
            badge_tag: faces.register("git-badge-tag", Face::fg_bg(rgb(220, 180, 80), rgb(62, 50, 25))),
        }
    }

    /// A one-line commit summary: hash, a badge per branch or tag on it, then the subject.
    pub fn commit_line<'a>(&self, commit: &'a Commit) -> Vec<(&'a str, Option<FaceId>)> {
        let mut parts = vec![(commit.hash.as_str(), Some(self.hash)), (" ", None)];
        for r in &commit.refs {
            let face = Some(match r.kind {
                RefKind::Head | RefKind::Current => self.badge_current,
                RefKind::Local => self.badge_local,
                RefKind::Remote => self.badge_remote,
                RefKind::Tag => self.badge_tag,
            });
            parts.extend([(" ", face), (r.name.as_str(), face), (" ", face), (" ", None)]);
        }
        parts.push((&commit.subject, None));
        parts
    }

    /// The face for a line of unified diff body.
    pub fn diff_line(&self, line: &str) -> Option<FaceId> {
        match line.as_bytes().first() {
            _ if line.starts_with("@@") => Some(self.hunk_heading),
            Some(b'+') if !line.starts_with("+++") => Some(self.added),
            Some(b'-') if !line.starts_with("---") => Some(self.removed),
            _ => None,
        }
    }
}

/// The repository of the active buffer's project. Git buffers work in their repository's
/// root, so from them it is theirs.
pub(crate) fn repo(ed: &Editor) -> Option<PathBuf> {
    let project = ed.project();
    project.is_git().then_some(project.root)
}

/// The generated buffer in `mode` for repository `root`, created if needed, named
/// `<kind>: <repository>`.
pub(crate) fn generated_buffer(ed: &mut Editor, kind: &str, mode: &str, root: &Path) -> BufferId {
    ed.generated_buffer(&format!("{}: {}", kind, repo_name(root)), mode, BufferScope::Dir(root))
}

fn repo_name(root: &Path) -> String {
    root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

/// Runs the status buffer's version of a command at point, else the diff buffer's.
fn at_point(ed: &mut Editor, status: impl FnOnce(&mut Editor), diff: impl FnOnce(&mut Editor)) {
    if status::at_point(ed).is_some() {
        status(ed)
    } else {
        diff(ed)
    }
}

/// Registers a command that needs the current repository.
fn repo_command(ed: &mut Editor, name: &str, doc: &str, run: fn(&mut Editor, PathBuf)) {
    ed.commands.register(name, doc, move |ed, _| match repo(ed) {
        Some(root) => run(ed, root),
        None => ed.set_status("Not inside a git repository"),
    });
}

impl Plugin for GitPlugin {
    fn name(&self) -> &str {
        "git"
    }

    fn init(&mut self, ed: &mut Editor) {
        *ed.ext_mut::<GitFaces>() = GitFaces::register(&mut ed.faces);

        repo_command(ed, "git-status", "Show the git status of the current repository", status::open);
        repo_command(ed, "git-refresh", "Refresh the git status buffer", |ed, root| status::refresh(ed, &root));
        repo_command(ed, "git-commit", "Commit menu", menus::commit);
        repo_command(ed, "git-push", "Push menu", menus::push);
        repo_command(ed, "git-pull", "Pull menu", menus::pull);
        repo_command(ed, "git-fetch", "Fetch menu", menus::fetch);
        repo_command(ed, "git-branch", "Branch menu", menus::branch);
        repo_command(ed, "git-tag", "Tag menu", menus::tag);
        repo_command(ed, "git-merge", "Merge menu", menus::merge);
        repo_command(ed, "git-rebase", "Rebase menu", menus::rebase);
        repo_command(ed, "git-log", "Show the log of the current branch", log::open);
        repo_command(ed, "git-diff", "Diff menu", menus::diff);
        repo_command(ed, "git-stash", "Stash menu", menus::stash);
        repo_command(ed, "git-reset", "Reset menu", menus::reset);
        repo_command(ed, "git-stage-all", "Stage all changes to tracked files", status::stage_all);
        repo_command(ed, "git-unstage-all", "Unstage everything", |ed, root| {
            process::run(ed, root, args(&["reset", "-q"]), None, "Unstaged all", |_| {})
        });
        repo_command(ed, "git-process", "Show the git commands run in this repository", process::show);

        let c = &mut ed.commands;
        c.register("git-toggle", "Expand or collapse the item at point", |ed, _| {
            at_point(ed, status::toggle, diff::toggle)
        });
        c.register("git-visit", "Visit the file or commit at point", |ed, _| at_point(ed, status::visit, diff::visit));
        for (name, doc, action) in [
            ("git-stage", "Stage the change at point, or the selected lines", Action::Stage),
            ("git-unstage", "Unstage the change at point, or the selected lines", Action::Unstage),
            ("git-discard", "Discard the change at point, or the selected lines", Action::Discard),
        ] {
            c.register(name, doc, move |ed, _| at_point(ed, |ed| status::act(ed, action), |ed| diff::act(ed, action)));
        }
        c.register("git-log-visit", "Show the commit at point", |ed, _| log::visit(ed));
        c.register("git-diff-refresh", "Rerun the diff", |ed, _| diff::rerun(ed));
        c.register("git-commit-finish", "Commit with this message", |ed, _| commit::finish(ed));
        c.register("git-commit-cancel", "Abandon this commit message", |ed, _| commit::cancel(ed));
        blame::register(ed);

        ed.define_mode(
            Mode::new(status::MODE)
                .special()
                .revert("git-refresh")
                .restore("git-status")
                .help_group(
                    "Menus",
                    &[
                        "git-branch",
                        "git-commit",
                        "git-diff",
                        "git-fetch",
                        "git-pull",
                        "git-push",
                        "git-tag",
                        "git-merge",
                        "git-rebase",
                        "git-stash",
                        "git-reset",
                        "git-log",
                    ],
                )
                .help_group("Apply", &["git-stage", "git-stage-all", "git-unstage", "git-unstage-all", "git-discard"])
                .help_group(
                    "Essential",
                    &[
                        "git-toggle",
                        "row-next",
                        "row-previous",
                        "git-visit",
                        "revert-buffer",
                        "git-process",
                        "quit-window",
                    ],
                )
                .keys(&[
                    ("TAB", "git-toggle"),
                    ("RET", "git-visit"),
                    ("s", "git-stage"),
                    ("S", "git-stage-all"),
                    ("u", "git-unstage"),
                    ("U", "git-unstage-all"),
                    ("k", "git-discard"),
                    ("c", "git-commit"),
                    ("P", "git-push"),
                    ("F", "git-pull"),
                    ("f", "git-fetch"),
                    ("b", "git-branch"),
                    ("t", "git-tag"),
                    ("m", "git-merge"),
                    ("r", "git-rebase"),
                    ("l", "git-log"),
                    ("d", "git-diff"),
                    ("z", "git-stash"),
                    ("x", "git-reset"),
                    ("$", "git-process"),
                ]),
        );
        ed.define_mode(Mode::new(log::MODE).special().revert("git-log").keys(&[
            ("RET", "git-log-visit"),
            ("d", "git-diff"),
            ("t", "git-tag"),
            ("x", "git-reset"),
            ("$", "git-process"),
        ]));
        ed.define_mode(Mode::new(diff::MODE).special().revert("git-diff-refresh").keys(&[
            ("TAB", "git-toggle"),
            ("RET", "git-visit"),
            ("s", "git-stage"),
            ("u", "git-unstage"),
            ("k", "git-discard"),
            ("d", "git-diff"),
            ("$", "git-process"),
        ]));
        ed.define_mode(Mode::new(process::MODE).special());
        ed.define_mode(commit::mode());
        ed.define_keymap(KeymapDef::new("global").keys(&[("C-x g", "git-status")]));
    }
}
