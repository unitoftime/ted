//! Source actions: fixes the server makes to a whole file (`textDocument/codeAction` with
//! a `source.*` kind), such as organizing imports. `organize-imports` runs that one; the
//! kinds in a mode's `lsp.save_actions` run in order before its buffers are saved.

use serde_json::{json, Value};
use ted_core::{BufferId, Editor, SaveToken};

use crate::{client, handles, workspace_edit};

const ORGANIZE_IMPORTS: &str = "source.organizeImports";

pub fn register(ed: &mut Editor) {
    ed.commands.register("organize-imports", "Sort, add and remove the buffer's imports", |ed, _| {
        let buffer = ed.active_buffer_id();
        run(ed, buffer, ORGANIZE_IMPORTS, |ed, result| {
            ed.set_status(match result {
                Err(e) => format!("Could not organize imports: {}", e),
                Ok(0) => "Imports already organized".to_string(),
                Ok(_) => "Organized imports".to_string(),
            });
        });
    });
    ed.hooks.on_before_save(|ed, buffer, save| {
        let mode = ed.buffers[buffer].mode();
        let kinds = ed.settings.get_in(handles(ed).save_actions, mode);
        let kinds: Vec<String> = kinds.iter().filter_map(|kind| kind.as_str()).map(str::to_string).collect();
        run_on_save(ed, buffer, kinds.into_iter(), save);
    });
}

/// Runs each of `kinds` on `buffer` in turn, then lets the save go on. One that fails is
/// skipped, as a formatter that fails is.
fn run_on_save(ed: &mut Editor, buffer: BufferId, mut kinds: std::vec::IntoIter<String>, save: SaveToken) {
    match kinds.next() {
        Some(kind) => run(ed, buffer, &kind, move |ed, _| run_on_save(ed, buffer, kinds, save)),
        None => save.done(ed),
    }
}

/// Asks `buffer`'s server for its source action of `kind` and applies it, then runs
/// `then` with how many edits it made.
fn run(ed: &mut Editor, buffer: BufferId, kind: &str, then: impl FnOnce(&mut Editor, Result<usize, String>) + 'static) {
    let Some(doc) = client::prepare(ed, buffer, |c| c.code_actions) else {
        return then(ed, Err("no language server for this buffer offers it".to_string()));
    };
    let buf = &ed.buffers[buffer];
    let (start, end) = (doc.encoding.position(buf, 0), doc.encoding.position(buf, buf.len_chars()));
    let params = json!({
        "textDocument": { "uri": doc.uri },
        "range": { "start": start.to_json(), "end": end.to_json() },
        // Automatic: the server leaves out actions that need choosing.
        "context": { "diagnostics": [], "only": [kind], "triggerKind": 2 },
    });
    let (version, kind) = (buf.version(), kind.to_string());
    client::request(ed, doc.server, "textDocument/codeAction", params, move |ed, result| {
        let applied = result.and_then(|actions| {
            if ed.buffers.get(buffer).map(|b| b.version()) != Some(version) {
                return Err("the buffer changed meanwhile".to_string());
            }
            match action_edit(&actions, &kind) {
                Some(edit) => workspace_edit::apply(ed, edit, doc.encoding).map(|(edits, _)| edits),
                None => Ok(0),
            }
        });
        then(ed, applied);
    });
}

/// The edit of the first action of `kind` (or a kind under it) in `actions`, if any.
fn action_edit<'a>(actions: &'a Value, kind: &str) -> Option<&'a Value> {
    let of_kind = |action: &&Value| {
        action
            .get("kind")
            .and_then(Value::as_str)
            .is_some_and(|k| k.strip_prefix(kind).is_some_and(|rest| rest.is_empty() || rest.starts_with('.')))
    };
    actions.as_array()?.iter().filter(of_kind).find_map(|action| action.get("edit"))
}
