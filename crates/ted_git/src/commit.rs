//! Writing commit messages in a buffer: `C-c C-c` commits, `C-c C-k` cancels.
//!
//! The message is highlighted as Markdown, with the `#` lines git ignores drawn as
//! comments.

use std::path::PathBuf;

use ted_core::{Editor, FaceId, Mode};

use crate::git::args;
use crate::{process, status};

pub const MODE: &str = "Git Commit";
pub const BUFFER: &str = "COMMIT_EDITMSG";
const HELP: &str =
    "# Write the commit message above. Lines starting with '#' are ignored.\n# C-c C-c to commit, C-c C-k to cancel.\n";

pub fn mode() -> Mode {
    Mode::new(MODE)
        .comment("# ")
        .grammar(ted_core::syntax::markdown)
        .line_face(|_, text| text.starts_with('#').then_some(FaceId::COMMENT))
        .keys(&[("C-c C-c", "git-commit-finish"), ("C-c C-k", "git-commit-cancel")])
}

/// Buffer-local marker: this buffer's message amends the last commit.
#[derive(Default)]
struct Amend(bool);

/// Opens a message buffer for a new commit, or for amending HEAD (prefilled).
pub fn start(ed: &mut Editor, root: PathBuf, amend: bool) {
    if !amend {
        if !status::has_staged(ed) {
            ed.set_status("Nothing staged (s stages the change at point)");
            return;
        }
        open(ed, root, false, "");
        return;
    }
    process::query(ed, root.clone(), args(&["log", "-1", "--format=%B"]), move |ed, message| {
        open(ed, root, true, message.trim_end());
    });
}

fn open(ed: &mut Editor, root: PathBuf, amend: bool, message: &str) {
    if let Some(old) = ed.buffers.find(|b| b.local::<Amend>().is_some()) {
        ed.kill_buffer(old);
    }
    let id = ed.new_buffer(BUFFER, MODE);
    let buf = &mut ed.buffers[id];
    buf.set_text(&format!("{}\n\n{}", message, HELP));
    buf.local_mut::<Amend>().0 = amend;
    buf.set_directory(root);
    ed.show_buffer(id);
    ed.set_status(if amend {
        "Amending: C-c C-c to commit, C-c C-k to cancel"
    } else {
        "C-c C-c to commit, C-c C-k to cancel"
    });
}

/// `C-c C-c`: commits with the buffer's message.
pub fn finish(ed: &mut Editor) {
    let id = ed.active_buffer_id();
    let buf = ed.active_buffer();
    let Some(amend) = buf.local::<Amend>() else { return };
    let (amend, root) = (amend.0, buf.directory());
    let text = buf.to_string();
    let message = text.lines().filter(|l| !l.starts_with('#')).collect::<Vec<_>>().join("\n").trim().to_string();
    if message.is_empty() {
        ed.set_status("Aborting commit due to empty message");
        return;
    }
    let mut git_args = args(&["commit", "-F", "-"]);
    if amend {
        git_args.push("--amend".into());
    }
    let status_root = root.clone();
    process::run(ed, root, git_args, Some(message), "Committed", move |ed| close(ed, id, &status_root));
}

/// `C-c C-k`: abandons the message.
pub fn cancel(ed: &mut Editor) {
    let id = ed.active_buffer_id();
    let root = ed.active_buffer().directory();
    close(ed, id, &root);
    ed.set_status("Commit cancelled");
}

fn close(ed: &mut Editor, id: ted_core::BufferId, root: &std::path::Path) {
    if ed.buffers.contains(id) {
        ed.kill_buffer(id);
    }
    if let Some(status) = status::find_buffer(ed, root) {
        ed.show_buffer(status);
    }
}
