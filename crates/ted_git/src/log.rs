//! The log buffer (`l`); RET shows the commit on the current line.

use std::path::PathBuf;

use ted_core::rows::{self, RowSpec, RowText};
use ted_core::{Editor, FaceId};

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
        ed.show_buffer(id);
        ed.buffers[id].local_mut::<LogBuffer>().hashes = commits.into_iter().map(|c| c.hash).collect();
        text.install(ed, id, "git");
    });
}

/// RET in the log: shows the commit on the current line.
pub fn visit(ed: &mut Editor) {
    let buf = ed.active_buffer();
    let hash = buf.local::<LogBuffer>().zip(rows::at_point(ed)).and_then(|(log, row)| log.hashes.get(row).cloned());
    let root = buf.directory();
    if let Some(hash) = hash {
        diff::show(ed, root, diff::Source::Commit(hash));
    }
}
