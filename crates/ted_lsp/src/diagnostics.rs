//! Diagnostics: the server's errors and warnings, underlined in open buffers, echoed when
//! the cursor lands on one, and listed in `diagnostics` (`C-c !`), a location list that
//! `next-error` steps through. `set("lsp.diagnostics", false)` or
//! `lsp-toggle-diagnostics` turns the display off; they are still collected.

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::Value;
use ted_core::locations::{self, ListItem, Location, Severity};
use ted_core::{Buffer, BufferId, Color, Decoration, Editor, Face, FaceId};

use crate::client::{self, Lsp};
use crate::handles;
use crate::protocol::{range_from_json, uri_to_path, Encoding, Position};

const LAYER: &str = "lsp-diagnostics";
const LIST_BUFFER: &str = "diagnostics";
const LIST_MODE: &str = "Diagnostics";

pub struct Diagnostic {
    pub start: Position,
    pub end: Position,
    pub severity: Severity,
    pub message: String,
}

#[derive(Default)]
struct Store {
    /// The latest diagnostics per file, in the columns of the server that sent them.
    by_path: HashMap<PathBuf, (Encoding, Vec<Diagnostic>)>,
    /// The diagnostic last echoed, as (buffer, line, index), so it isn't repeated.
    echoed: Option<(BufferId, usize, usize)>,
}

/// Faces, registered at startup and overridable by name like any other.
struct DiagnosticFaces([FaceId; 3]);

impl DiagnosticFaces {
    fn get(&self, severity: Severity) -> FaceId {
        match severity {
            Severity::Error => self.0[0],
            Severity::Warning => self.0[1],
            Severity::Info | Severity::Match => self.0[2],
        }
    }
}

impl Default for DiagnosticFaces {
    fn default() -> Self {
        Self([FaceId::ERROR, FaceId::WARNING, FaceId::DEFAULT])
    }
}

pub fn register(ed: &mut Editor) {
    let faces = [
        ed.faces.register("diagnostic-error", Face::default().underline_in(Color::rgb(239, 41, 41))),
        ed.faces.register("diagnostic-warning", Face::default().underline_in(Color::rgb(252, 175, 62))),
        ed.faces.register("diagnostic-info", Face::default().underline_in(Color::rgb(114, 159, 207))),
    ];
    *ed.ext_mut::<DiagnosticFaces>() = DiagnosticFaces(faces);

    ed.commands.register("lsp-diagnostics", "List the language servers' errors and warnings", |ed, _| {
        let id = fill_list(ed);
        ed.show_buffer(id);
    });
    ed.commands.register("lsp-toggle-diagnostics", "Show or hide language server diagnostics", |ed, _| {
        let enabled = !ed.settings.get(handles(ed).diagnostics);
        if let Err(e) = ed.set_setting("lsp.diagnostics", &enabled.into()) {
            ed.set_status(e);
            return;
        }
        ed.set_status(if enabled { "Diagnostics shown" } else { "Diagnostics hidden" });
    });
    ed.define_mode(locations::list_mode(LIST_MODE).revert("lsp-diagnostics"));
}

/// Redraws the diagnostics of every attached buffer (after `lsp.diagnostics` changed).
pub fn refresh_all(ed: &mut Editor) {
    let attached: Vec<BufferId> = ed.ext::<Lsp>().map_or_else(Vec::new, |l| l.docs.keys().copied().collect());
    for id in attached {
        refresh_buffer(ed, id);
    }
}

/// Reads a `publishDiagnostics` notification (on the reader thread).
pub fn parse(params: &Value) -> Option<(PathBuf, Vec<Diagnostic>)> {
    let path = uri_to_path(params.get("uri")?.as_str()?)?;
    let found = params
        .get("diagnostics")?
        .as_array()?
        .iter()
        .filter_map(|d| {
            let (start, end) = range_from_json(d.get("range")?)?;
            let severity = match d.get("severity").and_then(Value::as_u64) {
                Some(2) => Severity::Warning,
                Some(3) | Some(4) => Severity::Info,
                _ => Severity::Error,
            };
            let message = d.get("message")?.as_str()?;
            let source = d.get("source").and_then(Value::as_str);
            let message = match source {
                Some(source) => format!("{} [{}]", message.lines().next().unwrap_or_default(), source),
                None => message.lines().next().unwrap_or_default().to_string(),
            };
            Some(Diagnostic { start, end, severity, message })
        })
        .collect();
    Some((path, found))
}

/// Stores the diagnostics a server published for `path` and redraws them if it is open.
pub fn publish(ed: &mut Editor, path: PathBuf, found: Vec<Diagnostic>) {
    let encoding = client::encoding_for(ed, &path).unwrap_or(Encoding::Utf16);
    let open = ed.buffers.find_path(&path);
    let store = ed.ext_mut::<Store>();
    if found.is_empty() {
        store.by_path.remove(&path);
    } else {
        store.by_path.insert(path, (encoding, found));
    }
    store.echoed = None;
    if let Some(id) = open {
        refresh_buffer(ed, id);
    }
}

/// Redraws buffer `id`'s diagnostics, or clears them when the display is off.
pub fn refresh_buffer(ed: &mut Editor, id: BufferId) {
    let Some(buf) = ed.buffers.get(id) else {
        return;
    };
    let shown = ed.settings.get_in(handles(ed).diagnostics, buf.mode());
    let stored = buf.path().and_then(|p| ed.ext::<Store>()?.by_path.get(p)).filter(|_| shown);
    let decorations: Vec<Decoration> = match (stored, ed.ext::<DiagnosticFaces>()) {
        (Some((encoding, found)), Some(faces)) => {
            found.iter().map(|d| Decoration::new(char_range(buf, d, *encoding), faces.get(d.severity))).collect()
        }
        _ => Vec::new(),
    };
    let buf = &mut ed.buffers[id];
    if decorations.is_empty() {
        buf.decorations_mut().clear(LAYER);
    } else {
        buf.decorations_mut().set(LAYER, decorations);
    }
}

/// Post-command hook: shows the diagnostic under the cursor (else the first on its line)
/// when the cursor arrives on it.
pub fn echo(ed: &mut Editor) {
    if !ed.settings.get_in(handles(ed).diagnostics, ed.active_buffer().mode()) || ed.has_modal() {
        return;
    }
    let id = ed.active_buffer_id();
    let buf = ed.active_buffer();
    let line = buf.char_to_line(ed.active_view().cursor.pos);
    let Some(store) = ed.ext::<Store>() else {
        return;
    };
    let Some((encoding, found)) = buf.path().and_then(|p| store.by_path.get(p)) else {
        return;
    };
    let here = encoding.position(buf, ed.active_view().cursor.pos);
    let on_line = || found.iter().enumerate().filter(|(_, d)| d.start.line <= line && line <= d.end.line);
    let hit = on_line().find(|(_, d)| d.start <= here && here <= d.end).or_else(|| on_line().next());
    let key = hit.map(|(i, _)| (id, line, i));
    let message = hit.map(|(_, d)| format!("{}: {}", d.severity.label(), d.message));
    let store = ed.ext_mut::<Store>();
    if key == store.echoed {
        return;
    }
    store.echoed = key;
    if let Some(message) = message {
        ed.set_status(message);
    }
}

/// Writes every file's diagnostics into the `diagnostics` list, by file and position.
fn fill_list(ed: &mut Editor) -> BufferId {
    let mut items = Vec::new();
    if let Some(store) = ed.ext::<Store>() {
        for (path, (encoding, found)) in &store.by_path {
            let open = ed.buffers.find_path(path).map(|id| &ed.buffers[id]);
            for d in found {
                let col =
                    open.map_or(d.start.col, |buf| encoding.to_chars(buf.line(d.start.line).chars(), d.start.col));
                let location = Location { path: path.clone(), line: d.start.line, col };
                items.push(ListItem {
                    location,
                    severity: d.severity,
                    text: d.message.clone(),
                    highlights: Vec::new(),
                });
            }
        }
    }
    items.sort_by(|a, b| {
        let (la, lb) = (&a.location, &b.location);
        (&la.path, la.line, la.col).cmp(&(&lb.path, lb.line, lb.col))
    });
    let count = |severity| items.iter().filter(|i| i.severity == severity).count();
    let header = format!("Diagnostics: {} errors, {} warnings", count(Severity::Error), count(Severity::Warning));
    locations::fill_list(ed, LIST_BUFFER, LIST_MODE, &header, &items)
}

/// The chars a diagnostic covers; an empty range widens to one char so it shows.
fn char_range(buf: &Buffer, d: &Diagnostic, encoding: Encoding) -> std::ops::Range<usize> {
    let len = buf.len_chars();
    let (start, end) = (encoding.char_of(buf, d.start), encoding.char_of(buf, d.end));
    if end > start {
        start..end
    } else if start < len {
        start..start + 1
    } else {
        start.saturating_sub(1)..len
    }
}
