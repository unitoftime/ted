//! Mapping between a logical line's chars and its on-screen cells.
//!
//! Every place that needs screen geometry (vertical motion, scrolling, recenter, mouse
//! hit-testing, rendering) goes through `DisplayLine`, so tab expansion and wrapping are
//! defined exactly once. A non-wrapping view uses `NO_WRAP` as its wrap width.

use ropey::RopeSlice;
use unicode_width::UnicodeWidthChar;

/// Wrap width for views that don't wrap: every line is a single visual row.
pub const NO_WRAP: usize = usize::MAX;

/// Fills the second cell of a double-width character; frontends skip it when drawing.
pub const WIDE_CONTINUATION: char = '\0';

pub struct DisplayLine {
    /// Characters as drawn, one per cell: tabs expanded to spaces, wide characters
    /// followed by `WIDE_CONTINUATION`.
    pub cells: Vec<char>,
    /// `char_col[i]` is the visual column of char `i`; one extra entry marks the line end.
    char_col: Vec<usize>,
}

impl DisplayLine {
    /// Lays out `line`, which must not include its line terminator.
    pub fn new(line: RopeSlice, tab_width: usize) -> Self {
        let tab_width = tab_width.max(1);
        let mut cells = Vec::with_capacity(line.len_chars());
        let mut char_col = Vec::with_capacity(line.len_chars() + 1);
        for ch in line.chars() {
            char_col.push(cells.len());
            if ch == '\t' {
                let next_stop = (cells.len() / tab_width + 1) * tab_width;
                cells.resize(next_stop, ' ');
            } else {
                cells.push(ch);
                let width = ch.width().unwrap_or(1).clamp(1, 2);
                cells.extend(std::iter::repeat_n(WIDE_CONTINUATION, width - 1));
            }
        }
        char_col.push(cells.len());
        Self { cells, char_col }
    }

    pub fn width(&self) -> usize {
        self.cells.len()
    }

    pub fn char_len(&self) -> usize {
        self.char_col.len() - 1
    }

    /// Visual column of char `ch` (clamped to the line end).
    pub fn col(&self, ch: usize) -> usize {
        self.char_col[ch.min(self.char_len())]
    }

    /// The char occupying visual column `col` (clamped to the line end).
    pub fn char_at(&self, col: usize) -> usize {
        self.char_col.partition_point(|&c| c <= col).saturating_sub(1)
    }

    /// Number of screen rows this line takes at `wrap` columns.
    pub fn rows(&self, wrap: usize) -> usize {
        self.width().div_ceil(wrap.max(1)).max(1)
    }

    /// Visual row within the line and column within that row for char `ch`.
    pub fn row_col(&self, ch: usize, wrap: usize) -> (usize, usize) {
        let col = self.col(ch);
        let row = (col / wrap.max(1)).min(self.rows(wrap) - 1);
        (row, col - row * wrap)
    }

    /// Char at `col` within visual `row`, staying on that row.
    pub fn char_at_row_col(&self, row: usize, col: usize, wrap: usize) -> usize {
        let row = row.min(self.rows(wrap) - 1);
        let is_last_row = row + 1 == self.rows(wrap);
        let col = if is_last_row { col } else { col.min(wrap - 1) };
        self.char_at(row.saturating_mul(wrap).saturating_add(col))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    fn layout(text: &str, tab: usize) -> DisplayLine {
        let rope = Rope::from_str(text);
        DisplayLine::new(rope.slice(..), tab)
    }

    #[test]
    fn tabs_expand_to_tab_stops() {
        let dl = layout("a\tb", 4);
        assert_eq!(dl.cells, vec!['a', ' ', ' ', ' ', 'b']);
        assert_eq!((dl.col(0), dl.col(1), dl.col(2), dl.col(3)), (0, 1, 4, 5));
        assert_eq!((dl.char_at(0), dl.char_at(2), dl.char_at(4), dl.char_at(99)), (0, 1, 2, 3));
    }

    #[test]
    fn wrapping_rows() {
        let dl = layout(&"x".repeat(10), 4);
        assert_eq!(dl.rows(4), 3);
        assert_eq!(dl.row_col(5, 4), (1, 1));
        assert_eq!(dl.row_col(10, 4), (2, 2));
        assert_eq!(dl.char_at_row_col(1, 9, 4), 7);
        assert_eq!(dl.rows(NO_WRAP), 1);
        assert_eq!(dl.row_col(7, NO_WRAP), (0, 7));
    }

    #[test]
    fn wide_chars_take_two_cells() {
        let dl = layout("a漢b", 4);
        assert_eq!(dl.width(), 4);
        assert_eq!((dl.col(1), dl.col(2)), (1, 3));
        assert_eq!((dl.char_at(1), dl.char_at(2), dl.char_at(3)), (1, 1, 2));
    }
}
