//! `lsp-hover` (`C-c .`): the server's documentation of the symbol at point
//! (`textDocument/hover`), in a tooltip under it.

use serde_json::Value;
use ted_core::ui::Tooltip;
use ted_core::{Editor, FaceId};

use crate::client;
use crate::protocol::text_document_position;

pub fn register(ed: &mut Editor) {
    ed.commands.register("lsp-hover", "Show the language server's documentation of the symbol at point", |ed, _| {
        hover(ed);
    });
}

fn hover(ed: &mut Editor) {
    let (buffer, pos) = (ed.active_buffer_id(), ed.active_view().cursor.pos);
    let Some(doc) = client::prepare(ed, buffer, |c| c.hover) else {
        ed.set_status("No language server documentation for this buffer");
        return;
    };
    let params = text_document_position(&doc.uri, ed.active_buffer(), pos, doc.encoding);
    client::request(ed, doc.server, "textDocument/hover", params, move |ed, result| {
        if ed.active_buffer_id() != buffer || ed.active_view().cursor.pos != pos || ed.has_modal() {
            return;
        }
        let lines = match result {
            Ok(answer) => answer.get("contents").map(contents).unwrap_or_default(),
            Err(e) => {
                ed.set_status(format!("No documentation ({})", e));
                return;
            }
        };
        if lines.is_empty() {
            ed.set_status("No documentation at point");
        } else {
            ed.push_modal(Tooltip::new("lsp-hover", lines));
        }
    });
}

/// The lines of `MarkupContent`, a `MarkedString` or several: markdown turned into plain
/// lines, code in the function face.
fn contents(v: &Value) -> Vec<(String, Option<FaceId>)> {
    let mut lines = Vec::new();
    let mut add = |v: &Value| match v {
        Value::String(text) => markdown(text, &mut lines),
        // `MarkupContent` has a kind, a `MarkedString` a language (its value is code).
        _ => match (v.get("value").and_then(Value::as_str), v.get("language")) {
            (Some(code), Some(_)) => lines.extend(code.lines().map(|l| (l.to_string(), Some(FaceId::FUNCTION)))),
            (Some(text), None) => markdown(text, &mut lines),
            (None, _) => {}
        },
    };
    match v {
        Value::Array(items) => items.iter().for_each(&mut add),
        v => add(v),
    }
    while lines.last().is_some_and(|(l, _)| l.is_empty()) {
        lines.pop();
    }
    lines
}

/// Appends markdown as display lines: fenced code gets the function face, headings the
/// heading face; rules become blank lines, and backticks and escapes go.
fn markdown(text: &str, lines: &mut Vec<(String, Option<FaceId>)>) {
    if !lines.is_empty() {
        lines.push((String::new(), None));
    }
    let mut code = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            code = !code;
            continue;
        }
        let blank = trimmed.is_empty() || trimmed.chars().all(|c| c == '-' || c == '*' || c == '_');
        let entry = match () {
            _ if code => (line.to_string(), Some(FaceId::FUNCTION)),
            _ if blank => (String::new(), None),
            _ if trimmed.starts_with('#') => (plain(trimmed.trim_start_matches('#').trim()), Some(FaceId::HEADING)),
            _ => (plain(line), None),
        };
        // One blank line at most between paragraphs, none at the start.
        let repeat_blank = entry.0.is_empty() && lines.last().is_none_or(|(l, _)| l.is_empty());
        if !repeat_blank {
            lines.push(entry);
        }
    }
}

/// Inline markdown as plain text: no backticks, no backslash escapes.
fn plain(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '`' => {}
            '\\' => out.extend(chars.next()),
            c => out.push(c),
        }
    }
    out
}
