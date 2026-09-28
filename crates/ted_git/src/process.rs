//! Running git commands from the UI: mutations are logged to the repository's
//! `git process` buffer and followed by a status refresh; queries hand their output to a callback. Both run as jobs.

use std::path::{Path, PathBuf};

use ted_core::{Editor, FaceId, StyledText};

use crate::git::{git, git_output, GitOutput};
use crate::{diff, status};
use crate::{generated_buffer, GitFaces};

pub const MODE: &str = "Git Process";

/// Runs a git command that changes the repository, then refreshes its status and diff
/// buffers.
/// `on_success` runs on the UI thread after a successful run.
pub fn run(
    ed: &mut Editor,
    root: PathBuf,
    args: Vec<String>,
    stdin: Option<String>,
    done: &str,
    on_success: impl FnOnce(&mut Editor) + Send + 'static,
) {
    let label = format!("git {}", args.join(" "));
    let done = done.to_string();
    ed.set_status(format!("Running {}...", label));
    ed.spawn(move |ctx| {
        let output = git_output(&root, &args, stdin.as_deref());
        ctx.send(move |ed| {
            log(ed, &root, &label, &output);
            if output.ok {
                ed.set_status(done);
                on_success(ed);
            } else {
                let reason =
                    output.stderr.lines().chain(output.stdout.lines()).find(|l| !l.trim().is_empty()).unwrap_or("");
                ed.set_status(format!("{} failed: {} ($ for details)", label, reason.trim()));
            }
            status::refresh(ed, &root);
            diff::refresh(ed, root);
        });
    });
}

/// Runs a read-only git command and passes its stdout to `then` on the UI thread.
pub fn query(
    ed: &mut Editor,
    root: PathBuf,
    args: Vec<String>,
    then: impl FnOnce(&mut Editor, String) + Send + 'static,
) {
    ed.spawn(move |ctx| {
        let result = git(&root, &args, None);
        ctx.send(move |ed| match result {
            Ok(out) => then(ed, out),
            Err(e) => ed.set_status(format!("git {} failed: {}", args.join(" "), e)),
        });
    });
}

fn log(ed: &mut Editor, root: &Path, label: &str, output: &GitOutput) {
    let faces = *ed.ext_mut::<GitFaces>();
    let mut text = StyledText::new();
    text.line(&[(&format!("$ {}", label), Some(if output.ok { faces.section } else { FaceId::ERROR }))]);
    text.line(&[(&output.stdout, None), (&output.stderr, None)]);
    let id = generated_buffer(ed, "git process", MODE, root);
    ed.buffers[id].append_styled("git-process", text);
}

/// `$`: shows the process log of `root`.
pub fn show(ed: &mut Editor, root: PathBuf) {
    let id = generated_buffer(ed, "git process", MODE, &root);
    ed.show_buffer(id);
    let end = ed.buffers[id].len_chars();
    ed.doc().set_cursor(end);
}
