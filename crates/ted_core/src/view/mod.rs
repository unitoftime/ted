//! Views (windows): a viewport and cursor onto a buffer, referenced by id.

mod render;
pub mod visual;

use std::ops::Range;

pub use render::{BufferRenderer, RenderCtx};
pub use visual::{rows_for_width, DisplayLine, ScreenLine, VisualLine, NO_WRAP, WIDE_CONTINUATION};

use crate::buffer::{BufferId, Buffers, Place};
use crate::frame::{Metrics, Rect};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ViewId(pub(crate) u32);

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cursor {
    pub pos: usize,
    pub mark: Option<usize>,
    /// Column within the visual row that vertical motion tries to keep.
    pub goal_col: Option<usize>,
    /// The mark was set by shift-selection and is cleared by the next unshifted motion.
    pub shift_selected: bool,
}

impl Cursor {
    /// The active region, if the mark is set and differs from point.
    pub fn region(&self) -> Option<Range<usize>> {
        let mark = self.mark?;
        (mark != self.pos).then(|| mark.min(self.pos)..mark.max(self.pos))
    }
}

/// A jump label: `text` is drawn in place of the characters at `pos`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpLabel {
    pub pos: usize,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct View {
    id: ViewId,
    pub buffer: BufferId,
    pub cursor: Cursor,
    pub top_line: usize,
    /// Visual row of `top_line` shown first: a wrapped line taller than the window scrolls
    /// within itself. Always 0 without wrapping.
    pub top_row: usize,
    pub left_col: usize,
    pub wrap: bool,
    pub line_numbers: bool,
    /// Pattern highlighted throughout the view (incremental search).
    pub highlight: Option<String>,
    /// While a jump shows its targets: this view's labels, sorted by position. The text
    /// is drawn dimmed and each label replaces the characters at its target.
    pub jump: Option<Vec<JumpLabel>>,
    /// What this view showed before its current buffer, most recent last, for
    /// `quit-window`. Each buffer appears at most once and never while it is shown, so
    /// going back always reaches something older instead of cycling. Positions are the
    /// buffers' own places.
    pub back: Vec<BufferId>,
    /// Where the cursor was when the view was scrolled away from it (mouse wheel). Until
    /// the cursor moves, rendering keeps that scroll rather than bringing the cursor back.
    pub(crate) scrolled_from: Option<usize>,
    /// Geometry of the last render; drives paging, wrapping and hit-testing.
    pub(crate) bounds: Rect,
    pub(crate) metrics: Metrics,
    /// Where the last render drew the cursor, if it was on screen.
    pub(crate) caret: Option<Rect>,
}

impl View {
    pub(crate) fn new(id: ViewId, buffer: BufferId, wrap: bool) -> Self {
        Self {
            id,
            buffer,
            cursor: Cursor::default(),
            top_line: 0,
            top_row: 0,
            left_col: 0,
            wrap,
            line_numbers: true,
            highlight: None,
            jump: None,
            back: Vec::new(),
            scrolled_from: None,
            bounds: Rect::new(0.0, 0.0, 0.0, 0.0),
            metrics: Metrics::default(),
            caret: None,
        }
    }

    /// A view onto no buffer, for editing a buffer outside the layout (modal inputs) or as a
    /// stand-in while the layout restructures.
    pub(crate) fn detached() -> Self {
        Self::new(ViewId(u32::MAX), BufferId::DETACHED, false)
    }

    /// A new view onto the same buffer with the same position and settings.
    pub(crate) fn split_from(&self, id: ViewId) -> Self {
        Self { id, highlight: None, jump: None, ..self.clone() }
    }

    pub fn id(&self) -> ViewId {
        self.id
    }

    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    /// The cell the cursor was drawn in by the last render, if it was on screen: where
    /// popups about point (completions, hover) attach.
    pub fn caret(&self) -> Option<Rect> {
        self.caret
    }

    /// The row and the nearest column boundary of frame point `(x, y)`, counted from the
    /// window's top-left and clamped to its text area: where a buffer drawn by a
    /// `BufferRenderer` (which has no gutter) was clicked.
    pub fn grid_point(&self, x: f32, y: f32) -> (usize, usize) {
        let m = self.metrics;
        if m.char_w <= 0.0 || m.line_h <= 0.0 {
            return (0, 0);
        }
        let row = ((y - self.bounds.y) / m.line_h).floor().max(0.0) as usize;
        let col = ((x - self.bounds.x) / m.char_w).round().max(0.0) as usize;
        let cols = (self.bounds.w / m.char_w).floor() as usize;
        (row.min(self.text_rows() - 1), col.min(cols))
    }

    /// Shows `buffer` where a window last left it, remembering the current buffer and
    /// leaving this window's position in it. Showing the buffer already shown does nothing.
    pub fn set_buffer(&mut self, buffers: &mut Buffers, buffer: BufferId) {
        if buffer == self.buffer {
            return;
        }
        if buffers.contains(self.buffer) {
            self.remember_place(buffers);
            self.back.retain(|&b| b != buffer);
            self.back.push(self.buffer);
        }
        self.show(buffers, buffer);
    }

    /// Shows `buffer` at its place (kept inside its text) without touching the back stack.
    fn show(&mut self, buffers: &Buffers, buffer: BufferId) {
        let buf = &buffers[buffer];
        let place = buf.place();
        self.buffer = buffer;
        self.cursor = Cursor { pos: place.pos, mark: place.mark, ..Cursor::default() };
        self.top_line = place.top_line;
        self.top_row = 0;
        self.left_col = 0;
        self.highlight = None;
        self.jump = None;
        self.clamp(buf.len_chars(), buf.len_lines());
    }

    /// This view's position in its buffer.
    pub fn place(&self) -> Place {
        Place { pos: self.cursor.pos, mark: self.cursor.mark, top_line: self.top_line }
    }

    /// Leaves this view's position in its buffer (if it still exists), for the next view
    /// that shows it.
    pub(crate) fn remember_place(&self, buffers: &mut Buffers) {
        if let Some(buf) = buffers.get_mut(self.buffer) {
            buf.set_place(self.place());
        }
    }

    /// Puts the cursor at `pos` with no mark (callers that edit through a `Doc` get
    /// scrolling too; this is for views of buffers updated in the background).
    pub fn goto(&mut self, pos: usize) {
        self.cursor = Cursor { pos, ..Cursor::default() };
    }

    /// Keeps the cursor, mark and scroll inside a buffer of `len_chars` chars and
    /// `len_lines` lines, for a view whose buffer changed without it (a revert, a restored
    /// layout). A mark past the end is dropped.
    pub fn clamp(&mut self, len_chars: usize, len_lines: usize) {
        self.cursor.pos = self.cursor.pos.min(len_chars);
        self.cursor.mark = self.cursor.mark.filter(|&m| m <= len_chars);
        if self.top_line >= len_lines {
            self.top_line = len_lines.saturating_sub(1);
            self.top_row = 0;
        }
    }

    /// Back to the start of the buffer: cursor, mark and scroll reset.
    pub fn reset(&mut self) {
        self.cursor = Cursor::default();
        self.top_line = 0;
        self.top_row = 0;
        self.left_col = 0;
    }

    /// Leaves the current buffer for the most recent one on the back stack still in
    /// `buffers`, at its place. With nothing to go back to, shows `fallback`.
    pub fn go_back(&mut self, buffers: &mut Buffers, fallback: BufferId) {
        self.remember_place(buffers);
        let previous = std::iter::from_fn(|| self.back.pop()).find(|&b| buffers.contains(b));
        let target = previous.unwrap_or(fallback);
        if target != self.buffer {
            self.show(buffers, target);
        }
    }

    /// Drops `buffer` from the back stack (it was killed).
    pub(crate) fn forget(&mut self, buffer: BufferId) {
        self.back.retain(|&b| b != buffer);
    }

    /// Text rows available (the last row is the modeline).
    pub fn text_rows(&self) -> usize {
        if self.bounds.h > 0.0 && self.metrics.line_h > 0.0 {
            ((self.bounds.h / self.metrics.line_h).floor() as usize).saturating_sub(1).max(1)
        } else {
            24
        }
    }

    /// Columns of the line-number gutter for a buffer of `len_lines` lines.
    pub fn line_number_cols(&self, len_lines: usize) -> usize {
        if !self.line_numbers {
            return 0;
        }
        let digits = len_lines.to_string().len().max(2);
        digits + 1
    }

    /// Text columns beside a gutter `gutter_cols` wide.
    pub fn content_cols(&self, gutter_cols: usize) -> usize {
        let gutter_w = gutter_cols as f32 * self.metrics.char_w;
        if self.bounds.w > gutter_w && self.metrics.char_w > 0.0 {
            ((self.bounds.w - gutter_w) / self.metrics.char_w).floor().max(1.0) as usize
        } else {
            80
        }
    }
}
