//! The log buffer (`l`); RET shows the commit on the current line.

use std::path::{Path, PathBuf};

use ted_core::rows::{self, RowSpec, RowText};
use ted_core::{BufferScope, Editor, FaceId};

use crate::{diff, generated_buffer, model, process, GitFaces};

pub const MODE: &str = "Git Log";
const LOG_LIMIT: usize = 256;

/// Buffer-local: the commit of each row.
#[derive(Default)]
struct LogBuffer {
    hashes: Vec<String>,
}

/// Shows the log in the repository's log buffer, keeping point on its commit when the log
/// is refreshed.
pub fn open(ed: &mut Editor, root: PathBuf) {
    load(ed, root, true);
}

/// Reloads the repository's log buffer where it is, if there is one.
pub fn refresh(ed: &mut Editor, root: &Path) {
    if ed.find_generated(MODE, BufferScope::Dir(root)).is_some() {
        load(ed, root.to_path_buf(), false);
    }
}

/// Loads the log into the repository's log buffer, and with `show` brings it up.
fn load(ed: &mut Editor, root: PathBuf, show: bool) {
    process::query(ed, root.clone(), model::log_args(LOG_LIMIT), move |ed, out| {
        let faces = *ed.ext_mut::<GitFaces>();
        let commits = model::parse_log(&out);
        let mut text = RowText::new();
        for commit in &commits {
            let meta = format!("  ({}, {})", commit.author, commit.date);
            let mut parts = faces.commit_line(commit);
            parts.push((&meta, Some(FaceId::SHADOW)));
            text.row(RowSpec::new(&commit.hash), &parts);
        }
        let id = generated_buffer(ed, "git log", MODE, &root);
        if show {
            ed.show_buffer(id);
        }
        ed.buffers[id].local_mut::<LogBuffer>().hashes = commits.into_iter().map(|c| c.hash).collect();
        text.install(ed, id, "git");
    });
}

/// The commit on the current line, if the active buffer is a log buffer.
pub fn commit_at_point(ed: &Editor) -> Option<String> {
    let log = ed.active_buffer().local::<LogBuffer>()?;
    log.hashes.get(rows::at_point(ed)?).cloned()
}

/// RET in the log: shows the commit on the current line.
pub fn visit(ed: &mut Editor) {
    if let Some(hash) = commit_at_point(ed) {
        let root = ed.active_buffer().directory();
        diff::show(ed, root, diff::Source::Commit(hash));
    }
}
