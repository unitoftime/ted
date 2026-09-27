//! A popup of text at point, such as a symbol's documentation. It closes on the next key,
//! which then does what it normally does, so showing one never gets in the way.

use crate::editor::Editor;
use crate::face::FaceId;
use crate::frame::{Frame, Rect};
use crate::keymap::KeymapId;
use crate::text::{truncate_with_ellipsis, wrap_words};
use crate::ui::{popup_rect, render_box, Modal};
use crate::view::RenderCtx;

/// Longest line before wrapping, and most lines shown, in cells.
const MAX_COLS: usize = 90;
const MAX_ROWS: usize = 24;
const PAD: f32 = 6.0;

pub struct Tooltip {
    id: String,
    /// Wrapped to `MAX_COLS`, each with the face drawn over the popup's.
    rows: Vec<(String, Option<FaceId>)>,
}

impl Tooltip {
    /// Lines of text, each with an optional face (a code line, a heading).
    pub fn new(id: &str, lines: Vec<(String, Option<FaceId>)>) -> Self {
        let mut rows: Vec<(String, Option<FaceId>)> = lines
            .into_iter()
            .flat_map(|(line, face)| wrap_words(&line, MAX_COLS).into_iter().map(move |row| (row, face)))
            .collect();
        if rows.len() > MAX_ROWS {
            rows.truncate(MAX_ROWS);
            rows[MAX_ROWS - 1] = ("…".to_string(), Some(FaceId::SHADOW));
        }
        Self { id: id.to_string(), rows }
    }
}

impl Modal for Tooltip {
    fn id(&self) -> &str {
        &self.id
    }

    fn keymap(&self) -> KeymapId {
        KeymapId::TOOLTIP
    }

    fn render_overlay(&mut self, ed: &Editor, frame: &mut Frame, cx: &RenderCtx) {
        let Some(anchor) = ed.active_view().caret() else {
            return;
        };
        let (faces, m) = (cx.faces, cx.metrics);
        let cols = self.rows.iter().map(|(row, _)| row.chars().count()).max().unwrap_or(0);
        let size = (cols as f32 * m.char_w + 2.0 * PAD + 2.0, self.rows.len() as f32 * m.line_h + 2.0 * PAD);
        let rect = popup_rect(frame, anchor, size);
        let inner = render_box(frame, cx, rect, 1.0);
        let clip = Rect::new(inner.x + PAD, inner.y, (inner.w - 2.0 * PAD).max(0.0), inner.h);
        let fit = (clip.w / m.char_w.max(1.0)).floor() as usize;
        let base = faces.get(FaceId::POPUP);
        for (i, (row, face)) in self.rows.iter().enumerate() {
            let y = inner.y + PAD + i as f32 * m.line_h;
            let face = face.map_or(base, |f| base.merge(faces.get(f)));
            frame.draw_text_clipped(clip.x, y, truncate_with_ellipsis(row, fit), faces.style(face), clip);
        }
    }
}
