//! `Doc`: a view and its buffer borrowed together. All cursor motion and text editing is
//! implemented here as plain operations; commands wrap them with editor concerns
//! (kill ring, status messages, read-only checks).

use std::ops::Range;

use crate::buffer::{Buffer, EditKind};
use crate::mode::IndentStyle;
use crate::text::{word_len_backward, word_len_forward};
use crate::view::{rows_for_width, ScreenLine, View, VisualLine, NO_WRAP};

pub struct Doc<'a> {
    pub view: &'a mut View,
    pub buf: &'a mut Buffer,
}

/// Where `recenter` places the cursor line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecenterTarget {
    Center,
    Top,
    Bottom,
}

impl<'a> Doc<'a> {
    pub fn new(view: &'a mut View, buf: &'a mut Buffer) -> Self {
        Self { view, buf }
    }

    pub fn pos(&self) -> usize {
        self.view.cursor.pos
    }

    pub fn set_cursor(&mut self, pos: usize) {
        self.view.cursor.pos = pos.min(self.buf.len_chars());
        self.view.cursor.goal_col = None;
    }

    // ---------------------------------------------------------------------------------
    // Geometry
    // ---------------------------------------------------------------------------------

    pub fn content_cols(&self) -> usize {
        self.view.content_cols(self.gutter_cols())
    }

    /// Columns left of the text: the buffer's margin, then line numbers (unless the mode
    /// hides them).
    pub fn gutter_cols(&self) -> usize {
        let numbers = if self.buf.mode().line_numbers { self.view.line_number_cols(self.buf.len_lines()) } else { 0 };
        self.margin_cols() + numbers
    }

    pub fn margin_cols(&self) -> usize {
        self.buf.margin().map_or(0, |m| m.width)
    }

    fn wrap(&self) -> usize {
        if self.view.wrap {
            self.content_cols()
        } else {
            NO_WRAP
        }
    }

    /// `line`'s geometry as this view draws it.
    pub fn visual_line(&self, line: usize) -> VisualLine<'_> {
        VisualLine::new(self.buf.line_content(line), self.buf.tab_width(), self.wrap())
    }

    /// Screen rows `line` takes, counting no further than `max`.
    fn line_rows(&self, line: usize, max: usize) -> usize {
        self.visual_line(line).rows(max)
    }

    /// The lines on screen from the top of the view, each laid out over just the columns
    /// it shows.
    pub fn screen_lines(&self) -> Vec<ScreenLine> {
        let (text_rows, content_cols, wrap) = (self.view.text_rows(), self.content_cols(), self.view.wrap);
        let total = self.buf.len_lines();
        let (mut line, mut first_row) = (self.view.top_line, if wrap { self.view.top_row } else { 0 });
        let mut used = 0;
        let mut lines = Vec::new();
        while used < text_rows && line < total {
            let left = text_rows - used;
            let cols = if wrap {
                first_row * content_cols..(first_row + left) * content_cols
            } else {
                self.view.left_col..self.view.left_col + content_cols
            };
            let layout = self.visual_line(line).layout(cols.clone());
            let rows = match (wrap, layout.ends_line()) {
                (false, _) => 1,
                (true, true) => rows_for_width(layout.end_col(), content_cols).saturating_sub(first_row).min(left),
                (true, false) => left,
            };
            lines.push(ScreenLine { line, line_start: self.buf.line_to_char(line), first_row, rows, cols, layout });
            used += rows;
            line += 1;
            first_row = 0;
        }
        lines
    }

    /// (line, visual row within the line, column within that row) of `pos`.
    pub fn visual_pos(&self, pos: usize) -> (usize, usize, usize) {
        let (line, col) = self.buf.char_to_point(pos);
        let (row, row_col) = self.visual_line(line).row_col(col);
        (line, row, row_col)
    }

    fn pos_at_visual(&self, line: usize, row: usize, col: usize) -> usize {
        self.buf.line_to_char(line) + self.visual_line(line).char_at_row_col(row, col)
    }

    /// The visual row `delta` rows from visual `row` of `line`, stopping at the buffer's
    /// first and last rows.
    fn step_rows(&self, (mut line, row): (usize, usize), delta: isize) -> (usize, usize) {
        let mut row = row.min(self.line_rows(line, row + 1) - 1);
        let mut left = delta.unsigned_abs();
        if delta < 0 {
            while left > row {
                if line == 0 {
                    return (0, 0);
                }
                left -= row + 1;
                line -= 1;
                row = self.line_rows(line, usize::MAX) - 1;
            }
            return (line, row - left);
        }
        let last_line = self.buf.len_lines().saturating_sub(1);
        loop {
            let rows = self.line_rows(line, row.saturating_add(left).saturating_add(1));
            if row + left < rows || line == last_line {
                return (line, (row + left).min(rows - 1));
            }
            left -= rows - row;
            line += 1;
            row = 0;
        }
    }

    fn top(&self) -> (usize, usize) {
        (self.view.top_line, self.view.top_row)
    }

    fn set_top(&mut self, (line, row): (usize, usize)) {
        (self.view.top_line, self.view.top_row) = (line, row);
    }

    /// Scrolls just enough to keep the cursor on screen.
    pub fn ensure_cursor_visible(&mut self) {
        self.view.scrolled_from = None;
        let text_rows = self.view.text_rows();
        let (line, row, col) = self.visual_pos(self.pos());
        let top = self.top();
        if (line, row) < top {
            self.set_top((line, row));
        } else if (line, row) > self.step_rows(top, text_rows as isize - 1) {
            self.set_top(self.step_rows((line, row), -(text_rows as isize - 1)));
        }

        if self.view.wrap {
            self.view.left_col = 0;
        } else {
            let content_cols = self.content_cols();
            if col < self.view.left_col {
                self.view.left_col = col;
            } else if col >= self.view.left_col + content_cols {
                self.view.left_col = col + 5 - content_cols;
            }
        }
    }

    /// Brings the cursor into view after a jump: left alone if it is on screen, centered
    /// otherwise.
    pub fn reveal(&mut self) {
        let before = (self.top(), self.view.left_col);
        self.ensure_cursor_visible();
        if (self.top(), self.view.left_col) != before {
            self.recenter(RecenterTarget::Center);
        }
    }

    pub fn recenter(&mut self, target: RecenterTarget) {
        self.view.scrolled_from = None;
        let text_rows = self.view.text_rows();
        let (line, row, _) = self.visual_pos(self.pos());
        let target_row = match target {
            RecenterTarget::Center => text_rows / 2,
            RecenterTarget::Top => 0,
            RecenterTarget::Bottom => text_rows - 1,
        };
        self.set_top(self.step_rows((line, row), -(target_row as isize)));
    }

    /// Scrolls the view by `delta_rows` screen rows, leaving the cursor where it is (even
    /// off screen) until it moves.
    pub fn scroll(&mut self, delta_rows: isize) {
        self.view.scrolled_from = Some(self.pos());
        self.set_top(self.step_rows(self.top(), delta_rows));
    }

    /// Buffer position under screen point (`x`, `y`), using the last rendered geometry.
    pub fn pos_at_point(&self, x: f32, y: f32) -> Option<usize> {
        let (bounds, m) = (self.view.bounds, self.view.metrics);
        if !bounds.contains(x, y) || m.line_h <= 0.0 || m.char_w <= 0.0 {
            return None;
        }
        let screen_row = (((y - bounds.y) / m.line_h).floor() as usize).min(self.view.text_rows() - 1);
        let gutter_w = self.gutter_cols() as f32 * m.char_w;
        let col = ((x - bounds.x - gutter_w).max(0.0) / m.char_w).round() as usize;
        let (line, row) = self.step_rows(self.top(), screen_row as isize);
        let col = if self.view.wrap { col } else { self.view.left_col + col };
        Some(self.pos_at_visual(line, row, col))
    }

    // ---------------------------------------------------------------------------------
    // Motion
    // ---------------------------------------------------------------------------------

    /// Starts a motion: shift-translated motions extend a shift selection, others end one.
    pub fn begin_motion(&mut self, shift: bool) {
        self.buf.end_edit_group();
        let cursor = &mut self.view.cursor;
        if shift {
            if cursor.mark.is_none() {
                cursor.mark = Some(cursor.pos);
            }
            cursor.shift_selected = true;
        } else if cursor.shift_selected {
            cursor.mark = None;
            cursor.shift_selected = false;
        }
    }

    pub fn move_left(&mut self) {
        self.set_cursor(self.pos().saturating_sub(1));
    }

    pub fn move_right(&mut self) {
        self.set_cursor(self.pos() + 1);
    }

    /// Moves by `delta` visual rows, keeping the goal column.
    pub fn move_rows(&mut self, delta: isize) {
        let (line, row, col) = self.visual_pos(self.pos());
        let goal = self.view.cursor.goal_col.unwrap_or(col);
        let (line, row) = self.step_rows((line, row), delta);
        self.view.cursor.pos = self.pos_at_visual(line, row, goal);
        self.view.cursor.goal_col = Some(goal);
    }

    pub fn move_page(&mut self, direction: isize) {
        self.move_rows(direction * self.view.text_rows() as isize);
    }

    pub fn move_line_start(&mut self) {
        let line = self.buf.char_to_line(self.pos());
        self.set_cursor(self.buf.line_to_char(line));
    }

    pub fn move_line_end(&mut self) {
        let line = self.buf.char_to_line(self.pos());
        self.set_cursor(self.buf.line_end(line));
    }

    pub fn move_buffer_start(&mut self) {
        self.set_cursor(0);
    }

    /// End of the last line's content (before a trailing newline).
    pub fn move_buffer_end(&mut self) {
        let last = self.buf.len_lines().saturating_sub(1);
        let last_content = if last > 0 && self.buf.line(last).len_chars() == 0 { last - 1 } else { last };
        self.set_cursor(self.buf.line_end(last_content));
    }

    pub fn move_word_forward(&mut self) {
        let n = word_len_forward(self.buf.text().chars_at(self.pos()));
        self.set_cursor(self.pos() + n);
    }

    pub fn move_word_backward(&mut self) {
        let n = word_len_backward(self.buf.text().chars_at(self.pos()).reversed());
        self.set_cursor(self.pos() - n);
    }

    // ---------------------------------------------------------------------------------
    // Mark & region
    // ---------------------------------------------------------------------------------

    pub fn region(&self) -> Option<Range<usize>> {
        self.view.cursor.region()
    }

    /// Lines the region touches (a region ending at a line's start leaves that line out),
    /// or the cursor's line.
    pub fn region_lines(&self) -> Range<usize> {
        let cur_line = self.buf.char_to_line(self.pos());
        let Some(region) = self.region() else {
            return cur_line..cur_line + 1;
        };
        let first = self.buf.char_to_line(region.start);
        let last = self.buf.char_to_line(region.end);
        let ends_at_line_start = last > first && self.buf.line_to_char(last) == region.end;
        first..if ends_at_line_start { last } else { last + 1 }
    }

    pub fn clear_mark(&mut self) {
        self.view.cursor.mark = None;
        self.view.cursor.shift_selected = false;
    }

    /// Sets the mark at point, or clears it if set. Returns whether the mark is now set.
    pub fn toggle_mark(&mut self) -> bool {
        let set = self.view.cursor.mark.is_none();
        self.view.cursor.mark = set.then_some(self.pos());
        self.view.cursor.shift_selected = false;
        set
    }

    pub fn exchange_point_and_mark(&mut self) -> bool {
        let Some(mark) = self.view.cursor.mark else {
            return false;
        };
        self.view.cursor.mark = Some(self.pos());
        self.set_cursor(mark);
        true
    }

    pub fn mark_whole_buffer(&mut self) {
        self.view.cursor.mark = Some(self.buf.len_chars());
        self.view.cursor.shift_selected = false;
        self.set_cursor(0);
    }

    /// Mouse drag: selects whole lines from `from` (where the drag started) through `to`, with
    /// point at the `to` end, so a drag upwards leaves point on the top line.
    pub fn select_lines(&mut self, from: usize, to: usize) {
        let (first, last) = (from.min(to), from.max(to));
        let (start, end) = (self.buf.line_to_char(first), self.buf.line_to_char(last + 1));
        let (mark, point) = if to >= from { (start, end) } else { (end, start) };
        self.view.cursor.mark = Some(mark);
        self.view.cursor.shift_selected = false;
        self.set_cursor(point);
    }

    pub fn drag_to(&mut self, pos: usize) {
        if self.view.cursor.mark.is_none() {
            self.view.cursor.mark = Some(self.pos());
        }
        self.view.cursor.shift_selected = false;
        self.set_cursor(pos);
    }

    // ---------------------------------------------------------------------------------
    // Editing
    // ---------------------------------------------------------------------------------

    /// Runs `f` as its own undo step.
    fn atomic<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        self.buf.end_edit_group();
        self.buf.snapshot(self.pos());
        f(self)
    }

    fn replace(&mut self, range: Range<usize>, text: &str) {
        self.buf.remove(range.clone());
        self.buf.insert(range.start, text);
    }

    /// Deletes the active region (delete-selection semantics). Returns whether it did.
    pub fn delete_selection(&mut self) -> bool {
        let region = self.region();
        self.clear_mark();
        let Some(region) = region else {
            return false;
        };
        self.atomic(|d| {
            d.buf.remove(region.clone());
            d.set_cursor(region.start);
        });
        true
    }

    pub fn insert_char(&mut self, ch: char) {
        self.delete_selection();
        let pos = self.pos();
        if ch == '\n' {
            self.atomic(|d| d.buf.insert(pos, "\n"));
            self.set_cursor(pos + 1);
            return;
        }
        let is_space = ch == ' ' || ch == '\t';
        self.buf.begin_grouped_edit(EditKind::Typing, pos, is_space);
        let mut utf8 = [0u8; 4];
        self.buf.insert(pos, ch.encode_utf8(&mut utf8));
        self.set_cursor(pos + 1);
        self.buf.advance_grouped_edit(pos + 1, is_space);
    }

    /// Inserts `text` at point as one undo step and returns the inserted range.
    pub fn insert_text(&mut self, text: &str) -> Range<usize> {
        self.delete_selection();
        let start = self.pos();
        let end = start + text.chars().count();
        self.atomic(|d| d.buf.insert(start, text));
        self.set_cursor(end);
        start..end
    }

    /// Replaces `range` with `text` as one undo step, leaving point after it.
    pub fn replace_range(&mut self, range: Range<usize>, text: &str) -> Range<usize> {
        let end = range.start + text.chars().count();
        self.atomic(|d| d.replace(range.clone(), text));
        self.set_cursor(end);
        range.start..end
    }

    /// Leading whitespace of the cursor's line.
    pub fn current_indentation(&self) -> String {
        let line = self.buf.char_to_line(self.pos());
        self.buf.line_content(line).chars().take_while(|c| c.is_whitespace()).collect()
    }

    /// Newline that keeps the current line's indentation.
    pub fn newline(&mut self) {
        let text = format!("\n{}", self.current_indentation());
        self.insert_text(&text);
    }

    pub fn open_line(&mut self) {
        self.delete_selection();
        let pos = self.pos();
        self.atomic(|d| d.buf.insert(pos, "\n"));
        self.set_cursor(pos);
    }

    fn indent_unit(&self) -> String {
        match self.buf.indent_style() {
            IndentStyle::Tabs => "\t".to_string(),
            IndentStyle::Spaces => " ".repeat(self.buf.tab_width()),
        }
    }

    pub fn indent(&mut self) {
        let unit = self.indent_unit();
        self.insert_text(&unit);
    }

    /// TAB: reindents the region's lines, or the cursor's line, to where the syntax tree puts
    /// them. Without indentation rules for the buffer's language, it shifts the region one
    /// level right (keeping it, to shift again), or indents at point.
    pub fn indent_for_tab(&mut self) {
        let lines = self.region_lines();
        let region = self.region().is_some();
        let unit = self.indent_unit();
        match self.structural_levels(lines.clone()) {
            Some(levels) => {
                let targets: Vec<Option<String>> = levels
                    .into_iter()
                    .zip(lines.clone())
                    .map(|(level, line)| match level {
                        // Reindenting a region clears blank lines instead of indenting them.
                        Some(_) if region && self.is_blank(line) => Some(String::new()),
                        level => level.map(|n| unit.repeat(n)),
                    })
                    .collect();
                self.set_indentation(lines.start, &targets);
                self.clear_mark();
            }
            None if region => {
                let targets: Vec<Option<String>> = lines
                    .clone()
                    .map(|line| (!self.is_blank(line)).then(|| format!("{}{}", unit, self.indentation(line))))
                    .collect();
                self.set_indentation(lines.start, &targets);
            }
            None => self.indent(),
        }
    }

    /// Structural indent levels of `lines`, if the buffer's grammar has indentation rules.
    fn structural_levels(&mut self, lines: Range<usize>) -> Option<Vec<Option<usize>>> {
        let parsed = self.buf.parsed()?;
        Some(parsed.grammar.indents.as_ref()?.levels(parsed.tree, parsed.text, lines))
    }

    fn indentation(&self, line: usize) -> String {
        self.buf.line_content(line).chars().take_while(|&c| c == ' ' || c == '\t').collect()
    }

    fn is_blank(&self, line: usize) -> bool {
        self.buf.line_content(line).chars().all(|c| c == ' ' || c == '\t')
    }

    /// Replaces the indentation of the lines from `first` with `targets` (`None` leaves a
    /// line alone) as one undo step. Point and mark stay on the same text; point inside the
    /// indentation moves past it.
    fn set_indentation(&mut self, first: usize, targets: &[Option<String>]) {
        // (old indentation range, new indentation), bottom-up so that applying each edit
        // leaves the positions of those still to come alone.
        let edits: Vec<(Range<usize>, &str)> = (first..first + targets.len())
            .zip(targets)
            .rev()
            .filter_map(|(line, target)| {
                let start = self.buf.line_to_char(line);
                Some((start..start + self.indentation(line).chars().count(), target.as_deref()?))
            })
            .collect();
        let shift = |mut pos: usize, into_text: bool| {
            for (range, text) in &edits {
                let new_len = text.chars().count();
                if pos >= range.end {
                    pos = pos + new_len - range.len();
                } else if pos >= range.start {
                    pos = range.start + if into_text { new_len } else { (pos - range.start).min(new_len) };
                }
            }
            pos
        };
        let (pos, mark) = (shift(self.pos(), true), self.view.cursor.mark.map(|m| shift(m, false)));

        let changed: Vec<&(Range<usize>, &str)> =
            edits.iter().filter(|(range, text)| self.buf.slice_to_string(range.clone()) != *text).collect();
        if !changed.is_empty() {
            self.atomic(|d| changed.into_iter().for_each(|(range, text)| d.replace(range.clone(), text)));
        }
        self.set_cursor(pos);
        self.view.cursor.mark = mark;
    }

    /// Indents the cursor's line at its start, keeping point on the same text.
    pub fn indent_line(&mut self, text: &str) {
        let line_start = self.buf.line_to_char(self.buf.char_to_line(self.pos()));
        let pos = self.pos();
        self.atomic(|d| d.buf.insert(line_start, text));
        self.set_cursor(pos + text.chars().count());
    }

    /// Removes one level of indentation (a tab or up to `tab_width` spaces) from the
    /// region's lines, or the cursor's line. The region stays, to outdent again.
    pub fn outdent(&mut self) {
        let lines = self.region_lines();
        let width = self.buf.tab_width();
        let targets: Vec<Option<String>> = lines
            .clone()
            .map(|line| {
                let current = self.indentation(line);
                let n = match current.chars().next() {
                    Some('\t') => 1,
                    _ => current.chars().take(width).take_while(|&c| c == ' ').count(),
                };
                Some(current[n..].to_string())
            })
            .collect();
        self.set_indentation(lines.start, &targets);
    }

    pub fn backspace(&mut self) {
        if self.delete_selection() || self.pos() == 0 {
            return;
        }
        let pos = self.pos();
        self.buf.begin_grouped_edit(EditKind::Backspace, pos, false);
        self.buf.remove(pos - 1..pos);
        self.set_cursor(pos - 1);
        self.buf.advance_grouped_edit(pos - 1, false);
    }

    pub fn delete_forward(&mut self) {
        if self.delete_selection() || self.pos() >= self.buf.len_chars() {
            return;
        }
        let pos = self.pos();
        self.buf.begin_grouped_edit(EditKind::Delete, pos, false);
        self.buf.remove(pos..pos + 1);
        self.set_cursor(pos);
        self.buf.advance_grouped_edit(pos, false);
    }

    /// Removes `range` as one undo step, returning its text.
    fn cut(&mut self, range: Range<usize>) -> Option<String> {
        if range.is_empty() {
            return None;
        }
        let text = self.buf.slice_to_string(range.clone());
        self.atomic(|d| d.buf.remove(range.clone()));
        self.set_cursor(range.start);
        Some(text)
    }

    /// Kills to the end of the line, or the newline itself when already there.
    pub fn kill_line(&mut self) -> Option<String> {
        self.delete_selection();
        let pos = self.pos();
        let line_end = self.buf.line_end(self.buf.char_to_line(pos));
        let end = if pos >= line_end { (pos + 1).min(self.buf.len_chars()) } else { line_end };
        self.cut(pos..end)
    }

    pub fn kill_word_forward(&mut self) -> Option<String> {
        if self.delete_selection() {
            return None;
        }
        let pos = self.pos();
        let n = word_len_forward(self.buf.text().chars_at(pos));
        self.cut(pos..pos + n)
    }

    pub fn kill_word_backward(&mut self) -> Option<String> {
        let pos = self.pos();
        let n = word_len_backward(self.buf.text().chars_at(pos).reversed());
        self.cut(pos - n..pos)
    }

    pub fn kill_region(&mut self) -> Option<String> {
        let region = self.region();
        self.clear_mark();
        self.cut(region?)
    }

    pub fn copy_region(&mut self) -> Option<String> {
        let region = self.region();
        self.clear_mark();
        Some(self.buf.slice_to_string(region?))
    }

    pub fn undo(&mut self) -> bool {
        self.buf.end_edit_group();
        if !self.buf.can_undo() {
            return false;
        }
        match self.buf.undo(self.pos()) {
            Some(pos) => {
                self.set_cursor(pos);
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self) -> bool {
        self.buf.end_edit_group();
        if !self.buf.can_redo() {
            return false;
        }
        match self.buf.redo(self.pos()) {
            Some(pos) => {
                self.set_cursor(pos);
                true
            }
            None => false,
        }
    }

    /// Toggles line comments over the region's lines (or the current line).
    pub fn comment_region(&mut self) {
        let prefix = self.buf.mode().comment_prefix.clone();
        let bare = prefix.trim();
        let lines = self.region_lines();
        let (first, last) = (lines.start, lines.end - 1);

        let lines: Vec<String> = (first..=last).map(|l| self.buf.line_content(l).to_string()).collect();
        let all_commented = lines.iter().all(|l| l.trim().is_empty() || l.trim_start().starts_with(bare));

        let pos = self.pos();
        self.atomic(|d| {
            for (line, text) in (first..last + 1).zip(&lines).rev() {
                if text.trim().is_empty() {
                    continue;
                }
                let line_start = d.buf.line_to_char(line);
                if !all_commented {
                    d.buf.insert(line_start, &prefix);
                    continue;
                }
                let found = text
                    .find(prefix.as_str())
                    .map(|b| (b, prefix.len()))
                    .or_else(|| text.find(bare).map(|b| (b, bare.len())));
                if let Some((byte, byte_len)) = found {
                    let start = line_start + text[..byte].chars().count();
                    let len = text[byte..byte + byte_len].chars().count();
                    d.buf.remove(start..start + len);
                }
            }
        });
        self.set_cursor(pos);
    }
}
