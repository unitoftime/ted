//! Modal UI: anything that temporarily takes over the keyboard (prompts, pickers, the undo
//! tree). Modals live on a stack in the editor; the top one receives keys through its own
//! keymap, so every modal key is rebindable like any other.

mod history;
mod input;
mod menu;
pub mod picker;
mod prompt;

use std::any::Any;

pub use history::{HistoryCursor, InputHistory};
pub use input::LineInput;
pub use menu::Menu;
pub use picker::{Feed, Picker, PickerItem};
pub use prompt::{Choice, Completion, Prompt};

use crate::editor::Editor;
use crate::face::FaceId;
use crate::frame::{CursorVisual, Frame, Rect};
use crate::keymap::KeymapId;
use crate::view::RenderCtx;

pub trait AsAny: Any {
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn into_any(self: Box<Self>) -> Box<dyn Any>;
}

impl<T: Any> AsAny for T {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

pub trait Modal: AsAny {
    /// Identifies the modal, e.g. `find-file` or `search`.
    fn id(&self) -> &str;

    /// Keymap that receives keys while this modal is on top.
    fn keymap(&self) -> KeymapId;

    /// Text shown before the input in the minibuffer.
    fn label(&self) -> &str {
        ""
    }

    /// The editable line, if any. The shared `input-*` commands edit it.
    fn input(&self) -> Option<&LineInput> {
        None
    }

    fn input_mut(&mut self) -> Option<&mut LineInput> {
        None
    }

    /// Called after an `input-*` command changed the text.
    fn input_changed(&mut self, _ed: &mut Editor) {}

    /// The history `M-p` / `M-n` step through, if the modal has one: its id in
    /// `Editor::input_history`, the modal's position in it, and the input it fills.
    fn history(&mut self) -> Option<(&str, &mut HistoryCursor, &mut LineInput)> {
        None
    }

    /// Whether this modal draws its label and input in the minibuffer line.
    fn uses_minibuffer(&self) -> bool {
        false
    }

    /// Draws anything shown above the windows (popups, panels).
    fn render_overlay(&mut self, _ed: &Editor, _frame: &mut Frame, _cx: &RenderCtx) {}

    /// Called when the modal is dismissed with `modal-quit`.
    fn cancel(self: Box<Self>, _ed: &mut Editor) {}
}

/// Draws `label` + `input` into the minibuffer line, with `status` tagged on the right.
pub fn render_minibuffer_input(
    frame: &mut Frame,
    rect: Rect,
    cx: &RenderCtx,
    label: &str,
    input: Option<&LineInput>,
    status: &str,
) {
    let (faces, m) = (cx.faces, cx.metrics);
    let row_y = rect.y + ((rect.h - m.line_h) / 2.0).max(0.0);
    let input_x = rect.x + 8.0 + label.chars().count() as f32 * m.char_w;
    let (text, cursor) = input.map_or(("", 0), |i| (i.text(), i.cursor()));
    if let Some(input) = input {
        render_input_region(frame, input_x, row_y, input, cx);
    }
    let style = faces.text(FaceId::MINIBUFFER);
    frame.draw_text_clipped(rect.x + 8.0, row_y, format!("{}{}", label, text), style, rect);

    let cursor_x = input_x + cursor as f32 * m.char_w;
    let color = faces.bg(FaceId::MINIBUFFER_CURSOR);
    frame.set_cursor(CursorVisual { x: cursor_x, y: row_y, w: cx.cursor_w, h: m.line_h, color });

    if status.is_empty() {
        return;
    }
    let tag = format!("[{}]", status);
    let tag_w = tag.chars().count() as f32 * m.char_w;
    let x = (rect.x + rect.w - tag_w - 12.0).max(cursor_x + m.char_w * 2.0);
    if x < rect.x + rect.w - 20.0 {
        frame.draw_text_clipped(x, row_y, tag, faces.text(FaceId::MINIBUFFER_MESSAGE), rect);
    }
}

/// Shades the input's active region; `x` is where its text starts.
pub fn render_input_region(frame: &mut Frame, x: f32, y: f32, input: &LineInput, cx: &RenderCtx) {
    if let Some(region) = input.region() {
        let w = cx.metrics.char_w;
        let rect = Rect::new(x + region.start as f32 * w, y, region.len() as f32 * w, cx.metrics.line_h);
        frame.fill_rect(rect, cx.faces.bg(FaceId::REGION));
    }
}

/// Draws a centered popup panel with a title bar and returns the rect below the title.
pub fn render_panel(frame: &mut Frame, cx: &RenderCtx, size: (f32, f32), title: &str, backdrop: bool) -> Rect {
    let faces = cx.faces;
    let (w, h) = size;
    let rect = Rect::new(((frame.width - w) / 2.0).floor(), ((frame.height - h) / 2.0).floor(), w, h);
    if backdrop {
        frame.fill_rect(Rect::new(0.0, 0.0, frame.width, frame.height), faces.bg(FaceId::POPUP_BACKDROP));
    }
    frame.fill_rect(rect, faces.bg(FaceId::POPUP));
    frame.draw_rect_outline(rect, 2.0, faces.bg(FaceId::POPUP_BORDER));

    let inner = Rect::new(rect.x + 2.0, rect.y + 2.0, (w - 4.0).max(0.0), (h - 4.0).max(0.0));
    let header_h = cx.metrics.line_h + 4.0;
    frame.fill_rect(Rect::new(inner.x, inner.y, inner.w, header_h), faces.bg(FaceId::POPUP_HEADER));
    frame.draw_text_clipped(rect.x + 12.0, rect.y + 4.0, title, faces.text(FaceId::POPUP_HEADER), inner);

    Rect::new(inner.x, inner.y + header_h, inner.w, (inner.h - header_h).max(0.0))
}
