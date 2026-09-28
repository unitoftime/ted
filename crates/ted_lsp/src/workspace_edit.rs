//! Applying a server's `WorkspaceEdit` (from a rename or a code action) to the buffers of
//! the files it changes.

use std::path::PathBuf;

use serde_json::Value;
use ted_core::{Edit, Editor};

use crate::protocol::{edits_from_json, uri_to_path, Encoding};

/// Applies a `WorkspaceEdit` to the buffers of its files, opening those that aren't.
/// Returns how many edits it made in how many files. One that creates, renames or deletes
/// files is refused whole.
pub fn apply(ed: &mut Editor, edit: &Value, encoding: Encoding) -> Result<(usize, usize), String> {
    let path = |uri: Option<&str>| uri.and_then(uri_to_path).ok_or_else(|| "the server named a bad file".to_string());
    let mut files: Vec<(PathBuf, &Value)> = Vec::new();
    if let Some(changes) = edit.get("documentChanges").and_then(Value::as_array) {
        for change in changes {
            if let Some(kind) = change.get("kind").and_then(Value::as_str) {
                return Err(format!("it needs a file {}, which isn't supported", kind));
            }
            let uri = change.pointer("/textDocument/uri").and_then(Value::as_str);
            files.push((path(uri)?, change.get("edits").unwrap_or(&Value::Null)));
        }
    } else if let Some(changes) = edit.get("changes").and_then(Value::as_object) {
        for (uri, edits) in changes {
            files.push((path(Some(uri))?, edits));
        }
    }
    let mut count = 0;
    for (path, edits) in &files {
        let id = ed.visit_file(path).map_err(|e| format!("{}: {}", path.display(), e))?;
        let edits: Vec<Edit> = edits_from_json(edits, &ed.buffers[id], encoding);
        count += edits.len();
        ed.apply_edits(id, edits).map_err(|e| format!("{}: {}", path.display(), e))?;
    }
    Ok((count, files.len()))
}
