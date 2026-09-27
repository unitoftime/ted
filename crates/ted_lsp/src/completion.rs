//! The language server as a completion backend (`textDocument/completion`), ahead of the
//! words of open buffers. The server's ranking (`sortText`) orders the candidates before
//! typing narrows them.

use serde_json::{json, Value};
use ted_core::chain::{Backend, Reply};
use ted_core::completion::{Answer, Item, Query, Trigger};
use ted_core::{Buffer, Editor};

use crate::client;
use crate::protocol::{edits_from_json, text_document_position, Encoding};

pub struct LspBackend;

impl Backend<Query> for LspBackend {
    fn name(&self) -> &str {
        "lsp"
    }

    fn start(&self, ed: &mut Editor, query: &Query, reply: Reply<Query>) -> bool {
        let Some(doc) = client::prepare(ed, query.buffer, |c| c.completion) else {
            return false;
        };
        let mut params = text_document_position(&doc.uri, &ed.buffers[query.buffer], query.pos, doc.encoding);
        params["context"] = match query.trigger {
            Trigger::Character(c) => json!({ "triggerKind": 2, "triggerCharacter": c.to_string() }),
            Trigger::Incomplete => json!({ "triggerKind": 3 }),
            Trigger::Manual | Trigger::Typing => json!({ "triggerKind": 1 }),
        };
        let (buffer, start, encoding) = (query.buffer, query.start, doc.encoding);
        client::request(ed, doc.server, "textDocument/completion", params, move |ed, result| {
            let answer = result.map(|v| ed.buffers.get(buffer).map(|buf| answer(&v, buf, start, encoding)));
            reply.send(ed, answer.map(Option::unwrap_or_default));
        });
        true
    }
}

/// Reads a `CompletionList` or `CompletionItem[]`; items without an edit of their own
/// replace the word from `start`.
fn answer(v: &Value, buf: &Buffer, start: usize, encoding: Encoding) -> Answer {
    let list = v.get("items").unwrap_or(v).as_array().map_or(&[][..], Vec::as_slice);
    let incomplete = v.get("isIncomplete").and_then(Value::as_bool).unwrap_or(false);
    let mut ranked: Vec<(&str, Item)> = list.iter().filter_map(|v| item(v, buf, start, encoding)).collect();
    ranked.sort_by(|a, b| a.0.cmp(b.0));
    Answer { items: ranked.into_iter().map(|(_, item)| item).collect(), incomplete }
}

/// A `CompletionItem` and its `sortText`.
fn item<'a>(v: &'a Value, buf: &Buffer, start: usize, encoding: Encoding) -> Option<(&'a str, Item)> {
    let text = |pointer: &str| v.pointer(pointer).and_then(Value::as_str);
    let label = text("/label")?;
    let (start, insert) = match v.get("textEdit") {
        // A `TextEdit`, or an `InsertReplaceEdit` (its `insert` range keeps the text after point).
        Some(edit) => {
            let range = edit.get("range").or_else(|| edit.get("insert"))?;
            (encoding.chars_of(buf, range)?.start, edit.get("newText")?.as_str()?)
        }
        None => (start, text("/insertText").unwrap_or(label)),
    };
    let snippet = v.get("insertTextFormat").and_then(Value::as_u64) == Some(2);
    let detail = text("/detail").or_else(|| text("/labelDetails/description")).unwrap_or_default();
    let item = Item {
        label: format!("{}{}", label, text("/labelDetails/detail").unwrap_or_default()),
        filter: text("/filterText").unwrap_or(label).to_string(),
        detail: detail.lines().next().unwrap_or_default().to_string(),
        kind: kind_name(v.get("kind").and_then(Value::as_u64).unwrap_or(0)),
        start,
        insert: if snippet { snippet_text(insert) } else { insert.to_string() },
        extra: v.get("additionalTextEdits").map_or_else(Vec::new, |e| edits_from_json(e, buf, encoding)),
    };
    Some((text("/sortText").unwrap_or(label), item))
}

/// A `CompletionItemKind`, briefly.
fn kind_name(kind: u64) -> &'static str {
    const NAMES: [&str; 25] = [
        "text",
        "method",
        "fn",
        "ctor",
        "field",
        "var",
        "class",
        "interface",
        "mod",
        "property",
        "unit",
        "value",
        "enum",
        "keyword",
        "snippet",
        "color",
        "file",
        "ref",
        "folder",
        "variant",
        "const",
        "struct",
        "event",
        "op",
        "type",
    ];
    kind.checked_sub(1).and_then(|i| NAMES.get(i as usize)).copied().unwrap_or("")
}

/// The plain text of a snippet: placeholders keep their default text (`${1:x}` is `x`),
/// tab stops (`$1`, `${1}`), choices and variables go.
fn snippet_text(snippet: &str) -> String {
    let mut out = String::with_capacity(snippet.len());
    let mut chars = snippet.chars().peekable();
    // Placeholders open around the current char, whose `}` closes them.
    let mut open = 0;
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.extend(chars.next()),
            '$' if chars.next_if_eq(&'{').is_some() => {
                while chars.next_if(char::is_ascii_digit).is_some() {}
                if chars.next_if_eq(&':').is_some() {
                    open += 1;
                } else {
                    chars.by_ref().find(|&c| c == '}');
                }
            }
            '$' if chars.peek().is_some_and(char::is_ascii_digit) => {
                while chars.next_if(char::is_ascii_digit).is_some() {}
            }
            '}' if open > 0 => open -= 1,
            c => out.push(c),
        }
    }
    out
}
