//! The language server as a formatter (`textDocument/formatting`, or `rangeFormatting`
//! for a region), for `format-buffer` and `format_on_save`.

use serde_json::json;
use ted_core::chain::{Backend, Reply};
use ted_core::format::Query;
use ted_core::{Editor, IndentStyle};

use crate::client;
use crate::protocol::edits_from_json;

pub struct LspBackend;

impl Backend<Query> for LspBackend {
    fn name(&self) -> &str {
        "lsp"
    }

    fn start(&self, ed: &mut Editor, query: &Query, reply: Reply<Query>) -> bool {
        let region = query.range.is_some();
        let Some(doc) = client::prepare(ed, query.buffer, |c| if region { c.range_formatting } else { c.formatting })
        else {
            return false;
        };
        let buf = &ed.buffers[query.buffer];
        let options = json!({ "tabSize": buf.tab_width(), "insertSpaces": buf.indent_style() == IndentStyle::Spaces });
        let mut params = json!({ "textDocument": { "uri": doc.uri }, "options": options });
        let method = match &query.range {
            Some(range) => {
                let (start, end) = (doc.encoding.position(buf, range.start), doc.encoding.position(buf, range.end));
                params["range"] = json!({ "start": start.to_json(), "end": end.to_json() });
                "textDocument/rangeFormatting"
            }
            None => "textDocument/formatting",
        };
        let (buffer, encoding) = (query.buffer, doc.encoding);
        client::request(ed, doc.server, method, params, move |ed, result| {
            let edits =
                result.map(|v| ed.buffers.get(buffer).map_or_else(Vec::new, |b| edits_from_json(&v, b, encoding)));
            reply.send(ed, edits);
        });
        true
    }
}
