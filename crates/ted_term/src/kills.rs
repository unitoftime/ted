//! The shell's kills, copied into the editor's kill ring.
//!
//! At a shell prompt, `C-k`, `C-u`, `C-w` and `M-d` cut text into readline's own kill
//! ring, which the editor can't reach. So before such a key goes to the program, the text
//! it is about to kill is read off the screen, following readline's rules, and joins the
//! editor's kill ring as well: it yanks in buffers and other terminals.
//!
//! The screen doesn't say where the prompt ends, which `C-u` and `C-w` stop at. So the
//! cursor's place when typing starts on a line (the first key or paste after `RET`,
//! `C-c`...) stands for it, counted in cells from the start of the (possibly wrapped)
//! line. Full-screen programs give these keys their own meanings, so nothing is copied
//! from them.

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Term, TermMode};
use ted_core::{Key, KeyCode};

use crate::session::Listener;

/// The text that `key` kills at the prompt, if any, noting first where typing started.
pub fn before_key(term: &Term<Listener>, input_start: &mut Option<usize>, key: Key) -> Option<String> {
    if term.mode().contains(TermMode::ALT_SCREEN) {
        return None;
    }
    note_input(term, input_start);
    let (cells, cursor) = line_cells(term);
    let start = input_start.unwrap_or(0).min(cursor);
    let range = match (key.code, key.ctrl, key.alt) {
        // kill-line: to the end of the line
        (KeyCode::Char('k'), true, false) => cursor..cells.len(),
        // unix-line-discard: back to where typing started
        (KeyCode::Char('u'), true, false) => start..cursor,
        // unix-word-rubout: back over blanks, then over the word before them
        (KeyCode::Char('w'), true, false) => {
            let mut from = cursor;
            while from > start && cells[from - 1].is_whitespace() {
                from -= 1;
            }
            while from > start && !cells[from - 1].is_whitespace() {
                from -= 1;
            }
            from..cursor
        }
        // kill-word: forward over non-word characters, then the next word
        (KeyCode::Char('d'), false, true) => {
            let mut to = cursor;
            while to < cells.len() && !cells[to].is_alphanumeric() {
                to += 1;
            }
            while to < cells.len() && cells[to].is_alphanumeric() {
                to += 1;
            }
            cursor..to
        }
        // Submitting or abandoning the line: the next line's typing starts afresh.
        (KeyCode::Enter, false, false) | (KeyCode::Char('c' | 'j' | 'm' | 'o'), true, false) => {
            *input_start = None;
            return None;
        }
        _ => return None,
    };
    let text: String = cells[range].iter().collect();
    // Blank cells past the end of the text aren't part of it.
    Some(text.trim_end().to_string()).filter(|t| !t.is_empty())
}

/// Notes the cursor as where typing starts, unless typing has already started.
pub fn note_input(term: &Term<Listener>, input_start: &mut Option<usize>) {
    if input_start.is_none() && !term.mode().contains(TermMode::ALT_SCREEN) {
        *input_start = Some(line_cells(term).1);
    }
}

/// The characters of the cursor's line, joined across wraps, and the cursor's index in them.
fn line_cells(term: &Term<Listener>) -> (Vec<char>, usize) {
    let grid = term.grid();
    let point = grid.cursor.point;
    let (first, last) = (term.line_search_left(point).line.0, term.line_search_right(point).line.0);
    let mut cells = Vec::new();
    let mut cursor = 0;
    for line in first..=last {
        let row = &grid[Line(line)];
        for col in 0..grid.columns() {
            if line == point.line.0 && col == point.column.0 {
                cursor = cells.len();
            }
            let cell = &row[Column(col)];
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            cells.push(if cell.c == '\t' { ' ' } else { cell.c });
        }
    }
    // A cursor waiting to wrap sits past the last cell, not on it.
    if grid.cursor.input_needs_wrap {
        cursor = (cursor + 1).min(cells.len());
    }
    (cells, cursor)
}
