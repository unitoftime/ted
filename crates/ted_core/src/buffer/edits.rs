//! Batches of edits applied together: what formatters, renames and completions hand back.

use std::ops::Range;

use super::Buffer;

/// Replaces the chars in `range` with `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub range: Range<usize>,
    pub text: String,
}

impl Edit {
    pub fn new(range: Range<usize>, text: impl Into<String>) -> Self {
        Self { range, text: text.into() }
    }
}

/// Where `pos` ends up after `edits` (sorted and non-overlapping, all in the coordinates of
/// the text before them). At or past an edit's end it moves by how much the edit grew;
/// inside a replaced range it keeps its offset into the replacement, up to its end.
pub fn map_pos(edits: &[Edit], pos: usize) -> usize {
    let mut shifted = pos as isize;
    for edit in edits.iter().take_while(|e| e.range.start <= pos) {
        let new_len = edit.text.chars().count();
        if pos < edit.range.end {
            let start = edit.range.start as isize + shifted - pos as isize;
            return start as usize + (pos - edit.range.start).min(new_len);
        }
        shifted += new_len as isize - edit.range.len() as isize;
    }
    shifted as usize
}

impl Buffer {
    /// Applies `edits` as one undo step; undoing it returns point to `cursor`. They are
    /// sorted by position first (edits at the same position keep their order). Edits that
    /// overlap are refused, and then none is applied.
    pub fn apply_edits(&mut self, edits: &mut [Edit], cursor: usize) -> Result<(), String> {
        if self.read_only {
            return Err("Buffer is read-only".to_string());
        }
        let len = self.len_chars();
        for edit in edits.iter_mut() {
            edit.range = edit.range.start.min(len)..edit.range.end.clamp(edit.range.start.min(len), len);
        }
        edits.sort_by_key(|e| (e.range.start, e.range.end));
        if edits.windows(2).any(|w| w[0].range.end > w[1].range.start) {
            return Err("Overlapping edits".to_string());
        }
        self.end_edit_group();
        self.snapshot(cursor);
        for edit in edits.iter().rev() {
            self.remove(edit.range.clone());
            self.insert(edit.range.start, &edit.text);
        }
        Ok(())
    }
}
