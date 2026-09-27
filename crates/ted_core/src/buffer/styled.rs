//! Styled text: the way generated buffers (git status, location lists, process logs,
//! terminal snapshots) build their content. Text is appended piece by piece with optional
//! faces; `Buffer::set_styled` / `append_styled` then apply it as text plus a decoration
//! layer, so producers never track char offsets themselves.

use std::ops::Range;

use crate::buffer::Decoration;
use crate::face::FaceId;

#[derive(Debug, Default, Clone)]
pub struct StyledText {
    text: String,
    chars: usize,
    decorations: Vec<Decoration>,
}

impl StyledText {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends `text`, styled with `face` if given, and returns its char range.
    pub fn push(&mut self, text: &str, face: Option<FaceId>) -> Range<usize> {
        let start = self.chars;
        self.text.push_str(text);
        self.chars += text.chars().count();
        if let Some(face) = face {
            self.style(start..self.chars, face);
        }
        start..self.chars
    }

    /// Appends one line made of `(text, face)` parts, then a newline.
    pub fn line(&mut self, parts: &[(&str, Option<FaceId>)]) {
        for &(text, face) in parts {
            self.push(text, face);
        }
        self.push("\n", None);
    }

    /// Styles an already appended char range.
    pub fn style(&mut self, range: Range<usize>, face: FaceId) {
        if range.start < range.end {
            self.decorations.push(Decoration::new(range, face));
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn len_chars(&self) -> usize {
        self.chars
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The text as consecutive runs with the face styling each (`None` for plain text).
    /// Assumes the decorations don't overlap, which holds for text built with `push`.
    /// Meant for short pieces such as margin annotations.
    pub fn runs(&self) -> Vec<(&str, Option<FaceId>)> {
        let byte = |ch: usize| self.text.char_indices().nth(ch).map_or(self.text.len(), |(b, _)| b);
        let mut decorations: Vec<&Decoration> = self.decorations.iter().collect();
        decorations.sort_by_key(|d| d.range.start);
        let (mut runs, mut at) = (Vec::new(), 0);
        for d in decorations {
            if d.range.start > at {
                runs.push((&self.text[byte(at)..byte(d.range.start)], None));
            }
            runs.push((&self.text[byte(d.range.start)..byte(d.range.end)], Some(d.face)));
            at = d.range.end;
        }
        if at < self.chars {
            runs.push((&self.text[byte(at)..], None));
        }
        runs
    }

    pub(crate) fn into_parts(self) -> (String, Vec<Decoration>) {
        (self.text, self.decorations)
    }
}
