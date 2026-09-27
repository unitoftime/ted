//! The language server as an xref backend: `textDocument/definition` and
//! `textDocument/references`. It declines buffers without a ready server, and an empty or
//! failed answer falls through to tree-sitter.

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::json;
use ted_core::chain::{Backend, Reply};
use ted_core::locations::Location;
use ted_core::xref::{Item, Kind, Query};
use ted_core::Editor;

use crate::client;
use crate::protocol::{locations_from_json, text_document_position, Encoding, RawLocation};

pub struct LspBackend;

impl Backend<Query> for LspBackend {
    fn name(&self) -> &str {
        "lsp"
    }

    fn start(&self, ed: &mut Editor, query: &Query, reply: Reply<Query>) -> bool {
        let Some(doc) = client::prepare(ed, query.buffer, |_| true) else {
            return false;
        };
        let (server, encoding) = (doc.server, doc.encoding);
        let mut params = text_document_position(&doc.uri, &ed.buffers[query.buffer], query.pos, encoding);
        let method = match query.kind {
            Kind::Definition => "textDocument/definition",
            Kind::References => {
                params["context"] = json!({ "includeDeclaration": true });
                "textDocument/references"
            }
        };
        client::request(ed, server, method, params, move |ed, result| match result {
            Ok(answer) => resolve(ed, locations_from_json(&answer), encoding, reply),
            Err(e) => reply.send(ed, Err(e)),
        });
        true
    }
}

/// Turns the server's locations into items with their line's text: straight from open
/// buffers (which may hold unsaved edits), and from disk on a job thread for the rest.
fn resolve(ed: &mut Editor, found: Vec<RawLocation>, encoding: Encoding, reply: Reply<Query>) {
    let mut items = Vec::with_capacity(found.len());
    let mut on_disk = Vec::new();
    for raw in found {
        match ed.buffers.find_path(&raw.path) {
            Some(id) => {
                let line = ed.buffers[id].line_content(raw.pos.line).to_string();
                items.push(item(raw, line, encoding));
            }
            None => on_disk.push(raw),
        }
    }
    if on_disk.is_empty() {
        reply.send(ed, Ok(sorted(items)));
        return;
    }
    ed.spawn(move |ctx| {
        let mut files: HashMap<PathBuf, Vec<String>> = HashMap::new();
        for raw in on_disk {
            let lines = files.entry(raw.path.clone()).or_insert_with(|| {
                std::fs::read_to_string(&raw.path)
                    .map_or_else(|_| Vec::new(), |text| text.lines().map(str::to_string).collect())
            });
            let line = lines.get(raw.pos.line).cloned().unwrap_or_default();
            items.push(item(raw, line, encoding));
        }
        ctx.send(move |ed| reply.send(ed, Ok(sorted(items))));
    });
}

fn item(raw: RawLocation, text: String, encoding: Encoding) -> Item {
    let col = encoding.to_chars(text.chars(), raw.pos.col);
    Item { location: Location { path: raw.path, line: raw.pos.line, col }, text }
}

fn sorted(mut items: Vec<Item>) -> Vec<Item> {
    items.sort_by(|a, b| {
        let (a, b) = (&a.location, &b.location);
        (&a.path, a.line, a.col).cmp(&(&b.path, b.line, b.col))
    });
    items
}
