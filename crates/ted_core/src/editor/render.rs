//! Drawing the whole editor: windows, minibuffer line, then modal overlays.

use crate::doc::Doc;
use crate::editor::Editor;
use crate::face::FaceId;
use crate::frame::{Frame, Metrics, Rect};
use crate::key::format_seq;
use crate::settings;
use crate::ui::{render_minibuffer_input, Modal};
use crate::view::RenderCtx;

impl Editor {
    pub fn render(&mut self, frame: &mut Frame, metrics: Metrics) {
        if frame.width <= 0.0 || frame.height <= 0.0 {
            return;
        }
        let cursor_w = match self.settings.get(settings::CURSOR_SHAPE) {
            "line" => (self.settings.get(settings::CURSOR_WIDTH) as f32).max(1.0),
            _ => metrics.char_w,
        };
        let minibuffer_h = metrics.line_h + 4.0;
        let windows = Rect::new(0.0, 0.0, frame.width, (frame.height - minibuffer_h).max(0.0));
        let minibuffer = Rect::new(0.0, frame.height - minibuffer_h, frame.width, minibuffer_h);

        {
            let Editor { layout, buffers, faces, settings, .. } = self;
            let cx = RenderCtx { faces, metrics, cursor_w };
            let (rects, separators) = layout.rects(windows, settings.get(settings::SPLIT_SEPARATOR_SIZE) as f32);
            let active = self.focused.then(|| layout.active_id());
            for (id, rect) in rects {
                if let Some(view) = layout.view_mut(id) {
                    let buf = &mut buffers[view.buffer];
                    Doc::new(view, buf).render(frame, rect, Some(id) == active, &cx);
                }
            }
            for sep in separators {
                frame.fill_rect(sep, faces.bg(FaceId::WINDOW_DIVIDER));
            }
        }

        let mut modals = std::mem::take(&mut self.modals);
        let cx = RenderCtx { faces: &self.faces, metrics, cursor_w };
        self.render_minibuffer(&modals, frame, minibuffer, &cx);
        for modal in &mut modals {
            modal.render_overlay(self, frame, &cx);
        }
        self.modals = modals;
        if !self.focused {
            frame.hollow_cursor();
        }
    }

    fn render_minibuffer(&self, modals: &[Box<dyn Modal>], frame: &mut Frame, rect: Rect, cx: &RenderCtx) {
        let faces = cx.faces;
        frame.fill_rect(rect, faces.bg(FaceId::MINIBUFFER));
        if let Some(modal) = modals.iter().rev().find(|m| m.uses_minibuffer()) {
            render_minibuffer_input(frame, rect, cx, modal.label(), modal.input(), &self.status);
            return;
        }
        let text = if self.pending_keys.is_empty() {
            self.status.clone()
        } else {
            format!("{}-", format_seq(&self.pending_keys))
        };
        let row_y = rect.y + ((rect.h - cx.metrics.line_h) / 2.0).max(0.0);
        frame.draw_text_clipped(rect.x + 8.0, row_y, text, faces.text(FaceId::MINIBUFFER), rect);
    }
}
