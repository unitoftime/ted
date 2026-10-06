//! Rows: generated buffers whose lines stand for items, such as dired's entries, git
//! status sections, files and hunks, commits in a log, or a build's errors.
//!
//! A mode renders its items with `RowText`: `StyledText` plus which lines are rows. Each
//! row carries a stable key (a hash of whatever identifies its item, such as a file name),
//! so installing a new rendering keeps every window on the item it was on and keeps marks
//! on the items they were set on. The buffer-local `Rows` maps lines back to row indices,
//! which index the mode's own item data; the mode keeps that data in row order.
//!
//! `n` / `p` (`row-next` / `row-previous`) step between rows in any buffer that has them
//! (and move by lines in buffers that don't). Marks are drawn as a decoration layer, so
//! marking never re-renders the text; modes that act on marks bind `row-mark`,
//! `row-unmark` and `row-unmark-all`.

use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash, Hasher};

use crate::buffer::{Buffer, BufferId, Decoration, Place, StyledText};
use crate::commands::motion;
use crate::editor::Editor;
use crate::face::FaceId;

const MARKS: &str = "row-marks";

fn hash_key(value: impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// How a row is added: the identity of its item, whether `n` / `p` stop on it, and where
/// point goes on it.
#[derive(Debug, Clone, Copy)]
pub struct RowSpec {
    key: u64,
    stop: bool,
    col: usize,
}

impl RowSpec {
    /// A row for the item identified by `key` (hashed; any `Hash` value works).
    pub fn new(key: impl Hash) -> Self {
        Self { key: hash_key(key), stop: true, col: 0 }
    }

    /// Makes `n` / `p` pass over the row (lines inside a hunk, notes under an error).
    pub fn passive(mut self) -> Self {
        self.stop = false;
        self
    }

    /// Puts point `col` chars into the row, e.g. on a name after columns of details.
    pub fn point_at(mut self, col: usize) -> Self {
        self.col = col;
        self
    }
}

#[derive(Debug, Clone, Copy)]
struct Row {
    line: usize,
    spec: RowSpec,
}

/// Buffer-local: the rows of a buffer in line order, and the keys of the marked ones.
#[derive(Debug, Default)]
pub struct Rows {
    rows: Vec<Row>,
    marked: HashSet<u64>,
}

impl Rows {
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Removes every row and mark, for a buffer about to be refilled by `push`.
    pub fn clear(&mut self) {
        self.rows.clear();
        self.marked.clear();
    }

    /// Adds a row on `line`, after every existing row (for buffers that grow by appending,
    /// such as a build's output).
    pub fn push(&mut self, line: usize, spec: RowSpec) {
        debug_assert!(self.rows.last().is_none_or(|last| last.line < line));
        self.rows.push(Row { line, spec });
    }

    /// The row on `line`.
    pub fn at_line(&self, line: usize) -> Option<usize> {
        let i = self.rows.partition_point(|r| r.line < line);
        (self.rows.get(i)?.line == line).then_some(i)
    }

    /// The buffer line of row `row`.
    pub fn line(&self, row: usize) -> usize {
        self.rows[row].line
    }

    /// The row whose item has `key`.
    pub fn find(&self, key: impl Hash) -> Option<usize> {
        let key = hash_key(key);
        self.rows.iter().position(|r| r.spec.key == key)
    }

    /// The nearest row `n` / `p` stop on after (`forward`) or before `line`; `None` starts
    /// from the edge of the buffer.
    pub fn step(&self, line: Option<usize>, forward: bool) -> Option<usize> {
        let stops = |&i: &usize| self.rows[i].spec.stop;
        if forward {
            let start = line.map_or(0, |l| self.rows.partition_point(|r| r.line <= l));
            (start..self.rows.len()).find(stops)
        } else {
            let end = line.map_or(self.rows.len(), |l| self.rows.partition_point(|r| r.line < l));
            (0..end).rev().find(stops)
        }
    }

    /// Where point goes on row `row` of `buf`.
    pub fn point(&self, buf: &Buffer, row: usize) -> usize {
        let Row { line, spec } = self.rows[row];
        (buf.line_to_char(line) + spec.col).min(buf.line_end(line))
    }

    pub fn is_marked(&self, row: usize) -> bool {
        self.marked.contains(&self.rows[row].spec.key)
    }

    /// The marked rows, in line order.
    pub fn marked(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.rows.len()).filter(|&i| self.is_marked(i))
    }
}

/// Styled text whose lines are either rows standing for items or plain lines (headings,
/// blanks). Installing it into a buffer replaces the buffer's text and rows.
#[derive(Default)]
pub struct RowText {
    text: StyledText,
    rows: Vec<Row>,
    lines: usize,
}

impl RowText {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a plain line made of `(text, face)` parts.
    pub fn line(&mut self, parts: &[(&str, Option<FaceId>)]) {
        self.text.line(parts);
        self.lines += 1;
    }

    /// Appends a row standing for the item `spec` identifies.
    pub fn row(&mut self, spec: RowSpec, parts: &[(&str, Option<FaceId>)]) {
        self.rows.push(Row { line: self.lines, spec });
        self.line(parts);
    }

    /// Makes this buffer `id`'s text (faces in decoration layer `layer`) and rows, keeping
    /// each window, and the place the buffer is shown at next, on the item it was on (else
    /// on the same line) and marks on their items. For re-renders of the same content: a
    /// refresh, an expanded section.
    pub fn install(self, ed: &mut Editor, id: BufferId, layer: &str) {
        let buf = &ed.buffers[id];
        let old = buf.local::<Rows>();
        let place = buf.place();
        // Each window's point, then (no window) the buffer's place.
        let anchors: Vec<_> = ed
            .layout
            .views()
            .into_iter()
            .filter(|v| v.buffer == id)
            .map(|v| (Some(v.id()), v.cursor.pos))
            .chain([(None, place.pos)])
            .map(|(view, pos)| {
                let line = buf.char_to_line(pos);
                let key = old.and_then(|rows| Some(rows.rows[rows.at_line(line)?].spec.key));
                (view, key, line)
            })
            .collect();

        let keys: HashSet<u64> = self.rows.iter().map(|r| r.spec.key).collect();
        let buf = &mut ed.buffers[id];
        buf.set_styled(layer, self.text);
        let rows = buf.local_mut::<Rows>();
        rows.rows = self.rows;
        rows.marked.retain(|key| keys.contains(key));
        draw_marks(buf);

        let buf = &ed.buffers[id];
        let rows = buf.local::<Rows>().expect("installed above");
        let (len_chars, len_lines) = (buf.len_chars(), buf.len_lines());
        let moved: Vec<_> = anchors
            .into_iter()
            .map(|(view, key, line)| {
                let row = key.and_then(|key| rows.rows.iter().position(|r| r.spec.key == key));
                let line = line.min(len_lines - 1);
                let pos = match row.or_else(|| rows.at_line(line)) {
                    Some(row) => rows.point(buf, row),
                    None => buf.line_to_char(line),
                };
                (view, pos)
            })
            .collect();
        for (view, pos) in moved {
            match view {
                Some(view) => {
                    if let Some(view) = ed.layout.view_mut(view) {
                        view.goto(pos);
                        view.clamp(len_chars, len_lines);
                    }
                }
                None => ed.buffers[id].set_place(Place { pos, mark: None, top_line: place.top_line }),
            }
        }
    }

    /// Like `install`, for new content (another directory, another query): marks are
    /// cleared and windows start at the top, on the first row `n` stops on.
    pub fn install_fresh(self, ed: &mut Editor, id: BufferId, layer: &str) {
        let buf = &mut ed.buffers[id];
        buf.set_styled(layer, self.text);
        let rows = buf.local_mut::<Rows>();
        rows.rows = self.rows;
        rows.marked.clear();

        let buf = &ed.buffers[id];
        let rows = buf.local::<Rows>().expect("installed above");
        let pos = rows.step(None, true).map_or(0, |row| rows.point(buf, row));
        ed.buffers[id].set_place(Place { pos, ..Place::default() });
        for view in ed.layout.views_showing(id) {
            view.reset();
            view.goto(pos);
        }
    }
}

/// The row at point in the active window, if its buffer has rows.
pub fn at_point(ed: &Editor) -> Option<usize> {
    let buf = ed.active_buffer();
    buf.local::<Rows>()?.at_line(buf.char_to_line(ed.active_view().cursor.pos))
}

/// Moves point in the active window to the row whose item has `key`, if there is one.
pub fn focus(ed: &mut Editor, key: impl Hash) {
    let buf = ed.active_buffer();
    let Some(rows) = buf.local::<Rows>() else { return };
    if let Some(row) = rows.find(key) {
        let pos = rows.point(buf, row);
        ed.doc().set_cursor(pos);
    }
}

/// Unmarks every row of buffer `id`; returns how many were marked.
pub fn clear_marks(ed: &mut Editor, id: BufferId) -> usize {
    let buf = &mut ed.buffers[id];
    let count = buf.local_mut::<Rows>().marked.len();
    if count > 0 {
        buf.local_mut::<Rows>().marked.clear();
        draw_marks(buf);
    }
    count
}

fn draw_marks(buf: &mut Buffer) {
    let Some(rows) = buf.local::<Rows>() else { return };
    let decorations = rows
        .marked()
        .map(|i| {
            let line = rows.rows[i].line;
            Decoration::new(buf.line_to_char(line)..buf.line_end(line), FaceId::MARKED)
        })
        .collect();
    buf.decorations_mut().set(MARKS, decorations);
}

/// `n` / `p`: moves to the next or previous row that stops, else by a line.
fn step(ed: &mut Editor, forward: bool) {
    let buf = ed.active_buffer();
    let Some(rows) = buf.local::<Rows>().filter(|rows| !rows.is_empty()) else {
        motion(ed, |d| d.move_rows(if forward { 1 } else { -1 }));
        return;
    };
    let line = buf.char_to_line(ed.active_view().cursor.pos);
    if let Some(row) = rows.step(Some(line), forward) {
        let pos = rows.point(buf, row);
        // Over rows that do not stop, the step is a jump to the next heading.
        let jumps = rows.at_line(line).is_some_and(|from| from.abs_diff(row) > 1);
        motion(ed, |d| if jumps { d.jump_to(pos) } else { d.set_cursor(pos) });
    }
}

/// Marks or unmarks the row at point, then moves to the next row.
fn mark_at_point(ed: &mut Editor, marked: bool) {
    let Some(row) = at_point(ed) else { return };
    let buf = ed.active_buffer_mut();
    let rows = buf.local_mut::<Rows>();
    let key = rows.rows[row].spec.key;
    if marked {
        rows.marked.insert(key);
    } else {
        rows.marked.remove(&key);
    }
    draw_marks(buf);
    step(ed, true);
}

pub(crate) fn register(ed: &mut Editor) {
    let c = &mut ed.commands;
    c.register("row-next", "Move to the next item", |ed, _| step(ed, true));
    c.register("row-previous", "Move to the previous item", |ed, _| step(ed, false));
    c.register("row-mark", "Mark the item at point and move to the next", |ed, _| mark_at_point(ed, true));
    c.register("row-unmark", "Unmark the item at point and move to the next", |ed, _| mark_at_point(ed, false));
    c.register("row-unmark-all", "Unmark every item", |ed, _| {
        let id = ed.active_buffer_id();
        let count = clear_marks(ed, id);
        ed.set_status(format!("Unmarked {} {}", count, if count == 1 { "item" } else { "items" }));
    });
}
