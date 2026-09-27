//! The undo tree visualizer (C-x u): walk the active buffer's history with the arrows.

use crate::commands::edit::report_revision;
use crate::doc::Doc;
use crate::editor::{Editor, UndoChain};
use crate::face::FaceId;
use crate::frame::{Frame, Rect};
use crate::keymap::KeymapId;
use crate::ui::{render_panel, Modal};
use crate::view::RenderCtx;

const HINT: &str = "[↑/p] Undo   [↓/n] Redo   [←/b] Prev Branch   [→/f] Next Branch   [q/RET] Quit";

pub struct UndoTreeView {
    scroll: usize,
}

impl Modal for UndoTreeView {
    fn id(&self) -> &str {
        "undo-tree"
    }

    fn keymap(&self) -> KeymapId {
        KeymapId::UNDO_TREE
    }

    fn cancel(self: Box<Self>, ed: &mut Editor) {
        ed.active_buffer_mut().break_undo_chain();
        ed.set_status("Exited undo tree");
    }

    fn render_overlay(&mut self, ed: &Editor, frame: &mut Frame, cx: &RenderCtx) {
        let (faces, m) = (cx.faces, cx.metrics);
        let history = ed.active_buffer().history();
        let lines = history.format_tree();
        let size = (620f32.min(frame.width - 40.0), 480f32.min(frame.height - 60.0));
        let title =
            format!("Undo Tree Visualizer (Revision {}/{})", history.current_id, history.nodes.len().saturating_sub(1));
        let body = render_panel(frame, cx, size, &title, true);

        let hint_y = body.y + 6.0;
        frame.draw_text_clipped(body.x + 10.0, hint_y, HINT, faces.text(FaceId::SHADOW), body);
        let sep_y = hint_y + m.line_h + 4.0;
        frame.fill_rect(Rect::new(body.x + 4.0, sep_y, body.w - 8.0, 1.0), faces.bg(FaceId::POPUP_SEPARATOR));

        let list_y = sep_y + 6.0;
        let rows = ((body.y + body.h - list_y - 6.0) / m.line_h).floor().max(0.0) as usize;
        let current = lines.iter().position(|l| l.is_current).unwrap_or(0);
        if current < self.scroll {
            self.scroll = current;
        } else if current >= self.scroll + rows {
            self.scroll = current.saturating_sub(rows / 2);
        }

        let highlight = faces.get(FaceId::REGION).merge(faces.get(FaceId::POPUP_HIGHLIGHT));
        for (row, line) in lines.iter().skip(self.scroll).take(rows).enumerate() {
            let y = list_y + row as f32 * m.line_h;
            let clip = Rect::new(body.x + 4.0, y, (body.w - 8.0).max(0.0), m.line_h);
            let style = if line.is_current { faces.style(highlight) } else { faces.text(FaceId::POPUP) };
            if line.is_current {
                frame.fill_rect(clip, style.bg);
            }
            frame.draw_text_clipped(body.x + 10.0, y, line.text.as_str(), style, clip);
            if line.is_current {
                let badge = "<-- CURRENT";
                let x = body.x + body.w - badge.len() as f32 * m.char_w - 14.0;
                frame.draw_text_clipped(x, y, badge, style, clip);
            }
        }
    }
}

pub fn register(ed: &mut Editor) {
    let c = &mut ed.commands;
    c.register("undo-tree-visualize", "Open the interactive undo tree", |ed, _| {
        let doc = ed.doc();
        let pos = doc.pos();
        doc.buf.end_edit_group();
        doc.buf.snapshot(pos);
        ed.push_modal(UndoTreeView { scroll: 0 });
        ed.set_status(HINT);
    });
    c.register_hidden("undo-tree-undo", "Move up the undo tree", |ed, _| step(ed, |d| d.undo()));
    c.register_hidden("undo-tree-redo", "Move down the undo tree", |ed, _| step(ed, |d| d.redo()));
    c.register_hidden("undo-tree-prev-branch", "Switch to the previous sibling branch", |ed, _| {
        step(ed, |d| switch_branch(d, -1))
    });
    c.register_hidden("undo-tree-next-branch", "Switch to the next sibling branch", |ed, _| {
        step(ed, |d| switch_branch(d, 1))
    });
}

fn switch_branch(doc: &mut Doc, delta: isize) -> bool {
    match doc.buf.switch_undo_branch(delta) {
        Some(pos) => {
            doc.set_cursor(pos);
            true
        }
        None => false,
    }
}

fn step(ed: &mut Editor, f: impl FnOnce(&mut Doc) -> bool) {
    if f(&mut ed.doc()) {
        report_revision(ed, "Undo tree");
    }
    ed.set_chain(UndoChain);
}
