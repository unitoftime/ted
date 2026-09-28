//! `lsp-rename`: renames the symbol at point everywhere the server knows it is used
//! (`textDocument/rename`). Each changed file's edits are one undo step in its buffer;
//! files that weren't open are opened (not shown) and left unsaved, like the rest.

use serde_json::json;
use ted_core::xref::symbol_at;
use ted_core::Editor;

use crate::protocol::text_document_position;
use crate::{client, workspace_edit};

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
            let applied = result.and_then(|edit| workspace_edit::apply(ed, &edit, doc.encoding));
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
