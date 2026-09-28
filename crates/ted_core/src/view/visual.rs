//! Mapping between a logical line's chars and its on-screen cells.
//!
//! Every place that needs screen geometry (vertical motion, scrolling, recenter, mouse
//! hit-testing, rendering) goes through `VisualLine`, so tab expansion and wrapping are
//! defined exactly once. A non-wrapping view uses `NO_WRAP` as its wrap width.
//!
//! Nothing here lays out a whole line: each question walks only as far into the line as
//! its answer needs, and runs of plain ASCII are skipped a rope chunk at a time, so a
//! megabyte-long line costs no more than the part of it on screen.

use std::ops::Range;

use ropey::RopeSlice;
use unicode_width::UnicodeWidthChar;

/// Wrap width for views that don't wrap: every line is a single visual row.
pub const NO_WRAP: usize = usize::MAX;

/// Fills the second cell of a double-width character; frontends skip it when drawing.
pub const WIDE_CONTINUATION: char = '\0';

/// Cells `ch` takes when it starts at visual column `col`.
fn cell_width(ch: char, col: usize, tab_width: usize) -> usize {
    if ch == '\t' {
        tab_width - col % tab_width
    } else {
        ch.width().unwrap_or(1).clamp(1, 2)
    }
}

/// Screen rows a line `width` cells wide takes at `wrap` columns.
pub fn rows_for_width(width: usize, wrap: usize) -> usize {
    width.div_ceil(wrap.max(1)).max(1)
}

#[derive(Debug, Clone, Copy)]
enum Seek {
    /// The char at this index.
    Char(usize),
    /// The char covering this visual column.
    Col(usize),
}

/// One logical line's geometry at a wrap width.
#[derive(Clone, Copy)]
pub struct VisualLine<'a> {
    text: RopeSlice<'a>,
    tab_width: usize,
    wrap: usize,
}

impl<'a> VisualLine<'a> {
    /// `text` must not include its line terminator.
    pub fn new(text: RopeSlice<'a>, tab_width: usize, wrap: usize) -> Self {
        Self { text, tab_width: tab_width.max(1), wrap: wrap.max(1) }
    }

    /// Walks from the line start to `target`: returns (char, visual column it starts at),
    /// or (char count, line width) when the target is past the end.
    fn seek(&self, target: Seek) -> (usize, usize) {
        let (mut ch, mut col) = (0, 0);
        for chunk in self.text.chunks() {
            // Plain ASCII takes one cell per char, so the whole chunk advances at once.
            if chunk.is_ascii() && !chunk.contains('\t') {
                let n = chunk.len();
                match target {
                    Seek::Char(t) if t < ch + n => return (t, col + t - ch),
                    Seek::Col(t) if t < col + n => return (ch + t - col, t),
                    _ => (ch, col) = (ch + n, col + n),
                }
                continue;
            }
            for c in chunk.chars() {
                let w = cell_width(c, col, self.tab_width);
                match target {
                    Seek::Char(t) if t == ch => return (ch, col),
                    Seek::Col(t) if t < col + w => return (ch, col),
                    _ => (ch, col) = (ch + 1, col + w),
                }
            }
        }
        (ch, col)
    }

    /// Visual column of char `ch` (clamped to the line end).
    pub fn col(&self, ch: usize) -> usize {
        self.seek(Seek::Char(ch)).1
    }

    /// Screen rows the line takes, counting no further than `max`.
    pub fn rows(&self, max: usize) -> usize {
        if self.wrap == NO_WRAP {
            return 1;
        }
        match self.seek(Seek::Col(max.saturating_mul(self.wrap))) {
            (ch, width) if ch == self.text.len_chars() => rows_for_width(width, self.wrap).min(max),
            _ => max,
        }
    }

    /// Visual row within the line and column within that row for char `ch`.
    pub fn row_col(&self, ch: usize) -> (usize, usize) {
        let (ch, col) = self.seek(Seek::Char(ch));
        let mut row = col / self.wrap;
        // The end of a line that exactly fills its last row stays on that row.
        if ch == self.text.len_chars() && row > 0 && col % self.wrap == 0 {
            row -= 1;
        }
        (row, col - row * self.wrap)
    }

    /// Char at `col` within visual `row`, staying on that row: past the row's last cell
    /// is the row's last char, or the line end on the last row.
    pub fn char_at_row_col(&self, row: usize, col: usize) -> usize {
        let row_start = row * self.wrap;
        if col < self.wrap {
            return self.seek(Seek::Col(row_start + col)).0;
        }
        let row_end = row_start + self.wrap;
        let len = self.text.len_chars();
        let (ch, start) = self.seek(Seek::Col(row_end - 1));
        let is_last_row =
            ch == len || (ch + 1 == len && start + cell_width(self.text.char(ch), start, self.tab_width) <= row_end);
        if is_last_row {
            len
        } else {
            ch
        }
    }

    /// Lays out the cells of visual columns `cols` for drawing.
    pub fn layout(&self, cols: Range<usize>) -> DisplayLine {
        let (first_char, start_col) = self.seek(Seek::Col(cols.start));
        let mut cells = Vec::with_capacity(cols.len().min(self.text.len_chars() - first_char));
        let mut char_col = Vec::with_capacity(cells.capacity() + 1);
        let mut col = start_col;
        let mut ends_line = true;
        for ch in self.text.chars_at(first_char) {
            if col >= cols.end {
                ends_line = false;
                break;
            }
            char_col.push(col);
            let w = cell_width(ch, col, self.tab_width);
            if ch == '\t' {
                cells.extend(std::iter::repeat_n(' ', w));
            } else {
                cells.push(ch);
                cells.extend(std::iter::repeat_n(WIDE_CONTINUATION, w - 1));
            }
            col += w;
        }
        char_col.push(col);
        DisplayLine { cells, start_col, first_char, char_col, ends_line }
    }
}

/// The cells of a span of visual columns of one line, ready to draw. It starts at the
/// char covering the first requested column, so it may begin a little before it (inside a
/// tab or wide character) and end a little after.
pub struct DisplayLine {
    /// Characters as drawn, one per cell from `start_col`: tabs expanded to spaces, wide
    /// characters followed by `WIDE_CONTINUATION`.
    cells: Vec<char>,
    start_col: usize,
    /// `char_col[i]` is the visual column of char `first_char + i`; one extra entry marks
    /// the end of the last char laid out.
    first_char: usize,
    char_col: Vec<usize>,
    ends_line: bool,
}

impl DisplayLine {
    /// Visual columns laid out.
    pub fn cols(&self) -> Range<usize> {
        self.start_col..self.start_col + self.cells.len()
    }

    /// Chars laid out, as indices into the line.
    pub fn chars(&self) -> Range<usize> {
        self.first_char..self.first_char + self.char_col.len() - 1
    }

    /// Whether the layout reaches the end of the line, making `end_col` its width.
    pub fn ends_line(&self) -> bool {
        self.ends_line
    }

    /// Visual column just past the last char laid out.
    pub fn end_col(&self) -> usize {
        self.char_col[self.char_col.len() - 1]
    }

    /// Visual column of char `ch`, clamped to the chars laid out.
    pub fn col(&self, ch: usize) -> usize {
        self.char_col[ch.saturating_sub(self.first_char).min(self.char_col.len() - 1)]
    }

    /// The cells of `cols`, which must lie within `self.cols()`.
    pub fn cells(&self, cols: Range<usize>) -> &[char] {
        &self.cells[cols.start - self.start_col..cols.end - self.start_col]
    }

    /// Draws `text` over the cells from `col` (not before `self.cols()`), extending the
    /// line past its end. Returns the columns written.
    pub fn overwrite(&mut self, col: usize, text: &str) -> Range<usize> {
        let mut end = col;
        for ch in text.chars() {
            match self.cells.get_mut(end - self.start_col) {
                Some(cell) => *cell = ch,
                None => self.cells.push(ch),
            }
            end += 1;
        }
        col..end
    }
}

/// A line on screen: the visual columns it shows, laid out, and the rows they fill.
pub struct ScreenLine {
    pub line: usize,
    /// Buffer position of the line's first char.
    pub line_start: usize,
    /// Visual row of the line the first shown row is (non-zero only when the line is
    /// scrolled partly off the top).
    pub first_row: usize,
    /// Screen rows shown.
    pub rows: usize,
    /// Visual columns shown: all of the shown rows, or the horizontal scroll window.
    pub cols: Range<usize>,
    pub layout: DisplayLine,
}

impl ScreenLine {
    /// Buffer positions of the chars laid out.
    pub fn char_range(&self) -> Range<usize> {
        let chars = self.layout.chars();
        self.line_start + chars.start..self.line_start + chars.end
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    fn check(text: &str, tab: usize, wrap: usize, f: impl FnOnce(VisualLine)) {
        let rope = Rope::from_str(text);
        f(VisualLine::new(rope.slice(..), tab, wrap));
    }

    #[test]
    fn tabs_expand_to_tab_stops() {
        check("a\tb", 4, NO_WRAP, |vl| {
            let dl = vl.layout(0..usize::MAX);
            assert_eq!(dl.cells(dl.cols()), &['a', ' ', ' ', ' ', 'b']);
            assert_eq!((vl.col(0), vl.col(1), vl.col(2), vl.col(3)), (0, 1, 4, 5));
            let char_at = |col| vl.char_at_row_col(0, col);
            assert_eq!((char_at(0), char_at(2), char_at(4), char_at(99)), (0, 1, 2, 3));
        });
    }

    #[test]
    fn wrapping_rows() {
        check(&"x".repeat(10), 4, 4, |vl| {
            assert_eq!((vl.rows(usize::MAX), vl.rows(2)), (3, 2));
            assert_eq!(vl.row_col(5), (1, 1));
            assert_eq!(vl.row_col(10), (2, 2));
            assert_eq!(vl.char_at_row_col(1, 9), 7);
            assert_eq!(vl.char_at_row_col(2, 9), 10);
        });
        check(&"x".repeat(8), 4, 4, |vl| assert_eq!((vl.rows(usize::MAX), vl.row_col(8)), (2, (1, 4))));
        check(&"x".repeat(10), 4, NO_WRAP, |vl| assert_eq!((vl.rows(usize::MAX), vl.row_col(7)), (1, (0, 7))));
    }

    #[test]
    fn wide_chars_take_two_cells() {
        check("a漢b", 4, NO_WRAP, |vl| {
            assert_eq!(vl.col(3), 4);
            assert_eq!((vl.col(1), vl.col(2)), (1, 3));
            let char_at = |col| vl.char_at_row_col(0, col);
            assert_eq!((char_at(1), char_at(2), char_at(3)), (1, 1, 2));
        });
    }

    /// A layout starts at the char covering its first column and stops at its last, and the
    /// chunk-skipping walk agrees with laying out char by char across rope chunks.
    #[test]
    fn partial_layout_of_a_long_line() {
        let text = format!("{}\t漢{}", "a".repeat(5000), "b".repeat(5000));
        check(&text, 4, NO_WRAP, |vl| {
            let dl = vl.layout(5002..5010);
            assert_eq!(dl.cols(), 5000..5010);
            assert_eq!(dl.chars(), 5000..5006);
            assert_eq!(dl.cells(5002..5006), &[' ', ' ', '漢', WIDE_CONTINUATION]);
            assert!(!dl.ends_line());
            assert_eq!((vl.col(5001), vl.col(5002), vl.col(10002)), (5004, 5006, 10006));
            assert_eq!(vl.char_at_row_col(0, 5005), 5001);
            assert!(vl.layout(10000..20000).ends_line());
        });
    }
}
