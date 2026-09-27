//! `lsp-rename`: renames the symbol at point everywhere the server knows it is used
//! (`textDocument/rename`). Each changed file's edits are one undo step in its buffer;
//! files that weren't open are opened (not shown) and left unsaved, like the rest.

use std::path::PathBuf;

use serde_json::{json, Value};
use ted_core::xref::symbol_at;
use ted_core::{Edit, Editor};

use crate::client;
use crate::protocol::{edits_from_json, text_document_position, uri_to_path, Encoding};

pub fn register(ed: &mut Editor) {
    ed.commands.register("lsp-rename", "Rename the symbol at point throughout the project", |ed, _| {
        rename(ed);
    });
}

fn rename(ed: &mut Editor) {
    let (buffer, pos) = (ed.active_buffer_id(), ed.active_view().cursor.pos);
    let Some((_, symbol)) = symbol_at(ed.active_buffer(), pos) else {
        ed.set_status("No symbol at point");
        return;
    };
    let label = format!("Rename '{}' to: ", symbol);
    ed.prompt("lsp-rename", label, symbol.clone(), move |ed, name| {
        if name.is_empty() || name == symbol {
            return;
        }
        let Some(doc) = client::prepare(ed, buffer, |c| c.rename) else {
            ed.set_status("No language server can rename in this buffer");
            return;
        };
        let mut params = text_document_position(&doc.uri, &ed.buffers[buffer], pos, doc.encoding);
        params["newName"] = json!(name);
        ed.set_status(format!("Renaming '{}'...", symbol));
        client::request(ed, doc.server, "textDocument/rename", params, move |ed, result| {
            let applied = result.and_then(|edit| apply_workspace_edit(ed, &edit, doc.encoding));
            ed.set_status(match applied {
                Err(e) => format!("Could not rename '{}': {}", symbol, e),
                Ok((0, _)) => format!("Nothing to rename for '{}'", symbol),
                Ok((edits, 1)) => format!("Renamed '{}' to '{}' ({} edits)", symbol, name, edits),
                Ok((edits, files)) => format!(
                    "Renamed '{}' to '{}': {} edits in {} files (M-x save-some-buffers saves them)",
                    symbol, name, edits, files
                ),
            });
        });
    });
}

/// Applies a `WorkspaceEdit` to the buffers of its files, opening those that aren't.
/// Returns how many edits it made in how many files. One that creates, renames or deletes
/// files is refused whole.
fn apply_workspace_edit(ed: &mut Editor, edit: &Value, encoding: Encoding) -> Result<(usize, usize), String> {
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
