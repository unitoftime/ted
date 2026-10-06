//! Location lists: buffers whose lines point into files, such as compiler errors.
//!
//! A producer `push`es an `Entry` for each line of its buffer that refers to a file, and
//! makes the buffer the editor's current list with `set_current`. From then on
//! `next-error` / `previous-error` step through it from any buffer, and inside the list
//! buffer RET visits the entry on the cursor line while `n` / `p` move between entries.
//!
//! Entries are the buffer's rows (`rows`): the buffer-local `LocationList` holds what row
//! `i` points at, and `Rows` which line it is on. Notes are rows `n` / `p` pass over.
//!
//! Producers with a ready list of locations (references, diagnostics) use `fill_list`,
//! which writes them into a buffer in the `Locations` mode grouped by file: a short file
//! name (grown by directories only where two files share it), then a line per item with
//! its line number and text. Modes for such buffers derive from it with `list_mode` to add their own keys
//! and `revert` command, sharing the list keys.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::buffer::{Buffer, BufferId};
use crate::editor::{BufferScope, Editor};
use crate::face::FaceId;
use crate::mode::Mode;
use crate::rows::{self, RowSpec, RowText, Rows};
use crate::settings::TAB_WIDTH;
use crate::text::{highlight_runs, short_paths, trim_highlighted};

/// The mode (and keymap) of generated location lists.
pub const MODE: &str = "Locations";
pub const KEYMAP: &str = "locations";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Context (notes, help); visitable with RET but skipped when stepping.
    Info,
    /// A search hit or reference: stepped through, but not a problem.
    Match,
    Warning,
    Error,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Info => "Note",
            Severity::Match => "Match",
            Severity::Warning => "Warning",
            Severity::Error => "Error",
        }
    }

    /// The face of this severity's label in lists, if it is a problem.
    pub fn face(self) -> Option<FaceId> {
        match self {
            Severity::Info | Severity::Match => None,
            Severity::Warning => Some(FaceId::WARNING),
            Severity::Error => Some(FaceId::ERROR),
        }
    }
}

/// A position in a file; `line` and `col` count from 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    pub line: usize,
    pub col: usize,
}

/// A line of a list buffer that refers to `location`, as producers `push` it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub line: usize,
    pub severity: Severity,
    pub location: Location,
}

#[derive(Debug, Clone)]
struct Target {
    severity: Severity,
    location: Location,
}

/// Buffer-local: what each row of a list buffer points at, by row, and the one last
/// selected.
#[derive(Debug, Default)]
pub struct LocationList {
    targets: Vec<Target>,
    current: Option<usize>,
}

impl LocationList {
    pub fn current(&self) -> Option<usize> {
        self.current
    }

    pub fn count(&self, severity: Severity) -> usize {
        self.targets.iter().filter(|t| t.severity == severity).count()
    }
}

/// Empties the location list of `buf`, whose text is being replaced.
pub fn clear(buf: &mut Buffer) {
    buf.local_mut::<Rows>().clear();
    *buf.local_mut::<LocationList>() = LocationList::default();
}

/// Adds `entry` to the location list of `buf`; entries must arrive in line order.
pub fn push(buf: &mut Buffer, entry: Entry) {
    let list = buf.local_mut::<LocationList>();
    let spec = row_spec(list.targets.len(), entry.severity);
    list.targets.push(Target { severity: entry.severity, location: entry.location });
    buf.local_mut::<Rows>().push(entry.line, spec);
}

/// Stepping stops on everything but notes.
fn row_spec(key: impl std::hash::Hash, severity: Severity) -> RowSpec {
    let spec = RowSpec::new(key);
    if severity > Severity::Info {
        spec
    } else {
        spec.passive()
    }
}

/// The list `next-error` steps through (editor-wide).
#[derive(Default)]
struct CurrentList(Option<BufferId>);

/// Makes buffer `id` the list that `next-error` and `previous-error` step through.
pub fn set_current(ed: &mut Editor, id: BufferId) {
    ed.ext_mut::<CurrentList>().0 = Some(id);
}

/// One line of a list written by `fill_list`.
#[derive(Debug, Clone)]
pub struct ListItem {
    pub location: Location,
    pub severity: Severity,
    /// The line of the file, or a message about it.
    pub text: String,
    /// Char ranges of `text` drawn in the `match` face, such as the symbol referred to.
    pub highlights: Vec<Range<usize>>,
}

/// A read-only list mode named `name` whose keymap extends the list keys, for
/// `Editor::define_mode`.
pub fn list_mode(name: &str) -> Mode {
    Mode::new(name).set(TAB_WIDTH, 8).keymap(|k| k.parent(KEYMAP)).special()
}

/// Writes `items` into the list buffer of mode `mode`, named `name`, after `header`,
/// grouped by file in the order files first appear, each file's items by position. The
/// buffer becomes the current list for `next-error`; showing it is up to the caller.
pub fn fill_list(ed: &mut Editor, name: &str, mode: &str, header: &str, items: &[ListItem]) -> BufferId {
    let mut first_seen: HashMap<&Path, usize> = HashMap::new();
    for (i, item) in items.iter().enumerate() {
        first_seen.entry(&item.location.path).or_insert(i);
    }
    let mut order: Vec<&ListItem> = items.iter().collect();
    order.sort_by_key(|item| (first_seen[item.location.path.as_path()], item.location.line, item.location.col));
    let paths: Vec<&Path> = order.iter().map(|item| item.location.path.as_path()).collect();
    let labels = short_paths(&paths);
    let number_width = items.iter().map(|item| (item.location.line + 1).to_string().len()).max().unwrap_or(1);

    let mut text = RowText::new();
    text.line(&[(header, None)]);
    text.line(&[]);
    for (i, item) in order.iter().enumerate() {
        if i == 0 || paths[i] != paths[i - 1] {
            text.line(&[(&labels[i], Some(FaceId::HEADING))]);
        }
        let loc = &item.location;
        let number = format!("  {:>w$}  ", loc.line + 1, w = number_width);
        let label = item.severity.face().map(|face| (format!("{}: ", item.severity.label().to_lowercase()), face));
        let mut parts = vec![(number.as_str(), Some(FaceId::SHADOW))];
        if let Some((label, face)) = &label {
            parts.push((label, Some(*face)));
        }
        let prefix = parts.iter().map(|(part, _)| part.chars().count()).sum();
        let (line, highlights) = trim_highlighted(&item.text, &item.highlights);
        parts
            .extend(highlight_runs(&line, &highlights).into_iter().map(|(run, hl)| (run, hl.then_some(FaceId::MATCH))));
        let spec = row_spec((&loc.path, loc.line, loc.col, &item.text), item.severity).point_at(prefix);
        text.row(spec, &parts);
    }
    let id = ed.generated_buffer(name, mode, BufferScope::Workspace);
    text.install_fresh(ed, id, "locations");
    let targets =
        order.iter().map(|item| Target { severity: item.severity, location: item.location.clone() }).collect();
    *ed.buffers[id].local_mut::<LocationList>() = LocationList { targets, current: None };
    set_current(ed, id);
    id
}

pub(crate) fn register(ed: &mut Editor) {
    let c = &mut ed.commands;
    c.register("next-error", "Visit the next error or warning of the current location list", |ed, _| {
        step_current(ed, true);
    });
    c.register("previous-error", "Visit the previous error or warning of the current location list", |ed, _| {
        step_current(ed, false);
    });
    c.register("goto-location", "Visit the location referred to on the cursor line", |ed, _| {
        let id = ed.active_buffer_id();
        match rows::at_point(ed).filter(|_| ed.active_buffer().local::<LocationList>().is_some()) {
            Some(index) => {
                set_current(ed, id);
                select(ed, id, index, true);
            }
            None => ed.set_status("No location on this line"),
        }
    });
    c.register("next-location", "Move to the next error or warning in this location list", |ed, _| {
        step_here(ed, true);
    });
    c.register("previous-location", "Move to the previous error or warning in this location list", |ed, _| {
        step_here(ed, false);
    });

    ed.define_mode(Mode::new(MODE).set(TAB_WIDTH, 8).special().keys(&[
        ("RET", "goto-location"),
        ("n", "next-location"),
        ("p", "previous-location"),
    ]));
}

/// `next-error` / `previous-error`: steps from the current entry and visits the result.
/// The list `next-error` steps through: the current one if the active workspace has it, else
/// the workspace's most recently shown list.
fn current_list(ed: &Editor) -> Option<BufferId> {
    let shown = ed.workspaces.active().buffers();
    let current = ed.ext::<CurrentList>().and_then(|c| c.0).filter(|id| shown.contains(id));
    current.or_else(|| shown.iter().copied().find(|&id| ed.buffers[id].local::<LocationList>().is_some()))
}

fn step_current(ed: &mut Editor, forward: bool) {
    let Some(id) = current_list(ed) else {
        ed.set_status("No location list (run compile first)");
        return;
    };
    let buf = &ed.buffers[id];
    let (Some(list), Some(rows)) = (buf.local::<LocationList>(), buf.local::<Rows>()) else {
        ed.set_status("No location list (run compile first)");
        return;
    };
    let from = list.current.map(|current| rows.line(current));
    match rows.step(from, forward) {
        Some(index) => select(ed, id, index, true),
        None => ed.set_status(if forward { "No more errors" } else { "No previous errors" }),
    }
}

/// `n` / `p` in a list buffer: steps from the cursor line without visiting.
fn step_here(ed: &mut Editor, forward: bool) {
    let id = ed.active_buffer_id();
    let buf = ed.active_buffer();
    let line = buf.char_to_line(ed.active_view().cursor.pos);
    let rows = buf.local::<Rows>().filter(|_| buf.local::<LocationList>().is_some());
    match rows.and_then(|rows| rows.step(Some(line), forward)) {
        Some(index) => {
            set_current(ed, id);
            select(ed, id, index, false);
        }
        None => ed.set_status(if forward { "No more errors" } else { "No previous errors" }),
    }
}

/// Makes entry `index` of list `id` current, moves the list's cursors to it, and
/// optionally visits its location.
fn select(ed: &mut Editor, id: BufferId, index: usize, visit: bool) {
    let buf = &mut ed.buffers[id];
    let list = buf.local_mut::<LocationList>();
    list.current = Some(index);
    let target = list.targets[index].clone();
    let steppable = list.targets.iter().filter(|t| t.severity > Severity::Info).count();
    let ordinal = list.targets[..index].iter().filter(|t| t.severity > Severity::Info).count() + 1;
    let line = buf.local::<Rows>().expect("a location list's entries are rows").line(index);
    let pos = buf.line_to_char(line);
    let message = buf.line_content(line).to_string();
    for view in ed.layout.views_showing(id) {
        view.goto(pos);
    }
    if visit && !visit_from_list(ed, id, &target.location) {
        return;
    }
    let status = match target.severity {
        Severity::Info => format!("{}: {}", target.severity.label(), message.trim()),
        _ => format!("{} {} of {}: {}", target.severity.label(), ordinal, steppable, message.trim()),
    };
    ed.set_status(status);
}

/// Opens `location`, keeping list `id` on screen: from the list's own window it uses the
/// next window when there is one.
fn visit_from_list(ed: &mut Editor, id: BufferId, location: &Location) -> bool {
    if ed.active_buffer_id() == id && ed.layout.leaf_ids().len() > 1 {
        ed.layout.cycle(1);
    }
    visit(ed, location)
}

/// Opens `location` in the active window, scrolled into view. Reports failure in the
/// status line.
pub fn visit(ed: &mut Editor, location: &Location) -> bool {
    if let Err(e) = ed.open_file(&location.path) {
        ed.set_status(format!("Error opening {}: {}", location.path.display(), e));
        return false;
    }
    let mut doc = ed.doc();
    let pos = doc.buf.point_to_char(location.line, location.col);
    doc.clear_mark();
    doc.jump_to(pos);
    true
}
