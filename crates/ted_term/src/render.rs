//! Terminal mode: draws the emulator's screen directly on the cell grid.

use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::{CursorShape, Rgb};
use ted_core::frame::{CursorVisual, Frame, Rect, Style};
use ted_core::view::{BufferRenderer, RenderCtx, WIDE_CONTINUATION};
use ted_core::FaceId;

use crate::palette::{color, rgb, Palette};
use crate::session::{TermHandle, TermSize};

pub struct TermRenderer {
    pub handle: TermHandle,
}

/// Consecutive cells on one row sharing a style.
struct Run {
    row: usize,
    start: usize,
    text: String,
    fg: Rgb,
    bg: Rgb,
    flags: Flags,
}

const STYLE_FLAGS: Flags = Flags::BOLD.union(Flags::ITALIC).union(Flags::ALL_UNDERLINES);

impl BufferRenderer for TermRenderer {
    fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool, cx: &RenderCtx) {
        let m = cx.metrics;
        let size = TermSize {
            cols: ((area.w / m.char_w).floor() as usize).max(1),
            lines: ((area.h / m.line_h).floor() as usize).max(1),
        };
        self.handle.resize(size, (m.char_w, m.line_h));

        let palette = Palette::from_faces(cx.faces);
        frame.fill_rect(area, color(palette.bg));
        let term = self.handle.term.lock();
        let content = term.renderable_content();
        let offset = content.display_offset as i32;
        let selection = content.selection;
        let selected_bg = rgb(cx.faces.bg(FaceId::REGION));

        let draw = |frame: &mut Frame, run: &Run| {
            let x = area.x + run.start as f32 * m.char_w;
            let y = area.y + run.row as f32 * m.line_h;
            let cells = run.text.chars().count() as f32;
            if run.bg != palette.bg {
                frame.fill_rect(Rect::new(x, y, cells * m.char_w, m.line_h), color(run.bg));
            }
            let underline = run.flags.intersects(Flags::ALL_UNDERLINES);
            if underline || run.text.chars().any(|c| c != ' ' && c != WIDE_CONTINUATION) {
                let style = Style {
                    fg: color(run.fg),
                    bg: color(run.bg),
                    bold: run.flags.contains(Flags::BOLD),
                    italic: run.flags.contains(Flags::ITALIC),
                    underline: underline.then(|| color(run.fg)),
                };
                frame.draw_text_clipped(x, y, run.text.clone(), style, area);
            }
        };

        let mut run: Option<Run> = None;
        for indexed in content.display_iter {
            let row = indexed.point.line.0 + offset;
            if row < 0 || row as usize >= size.lines {
                continue;
            }
            let (row, col) = (row as usize, indexed.point.column.0);
            let cell = indexed.cell;
            let ch = if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                WIDE_CONTINUATION
            } else if cell.flags.contains(Flags::HIDDEN) {
                ' '
            } else {
                cell.c
            };
            let (fg, mut bg) = palette.cell_colors(cell, content.colors);
            if selection.is_some_and(|s| s.contains(indexed.point)) {
                bg = selected_bg;
            }
            let flags = cell.flags & STYLE_FLAGS;
            match &mut run {
                Some(r) if r.row == row && r.fg == fg && r.bg == bg && r.flags == flags => r.text.push(ch),
                _ => {
                    if let Some(done) = run.take() {
                        draw(frame, &done);
                    }
                    run = Some(Run { row, start: col, text: ch.to_string(), fg, bg, flags });
                }
            }
        }
        if let Some(done) = run {
            draw(frame, &done);
        }

        let cursor = content.cursor;
        let row = cursor.point.line.0 + offset;
        if content.mode.contains(TermMode::SHOW_CURSOR)
            && cursor.shape != CursorShape::Hidden
            && row >= 0
            && (row as usize) < size.lines
        {
            let x = area.x + cursor.point.column.0 as f32 * m.char_w;
            let y = area.y + row as f32 * m.line_h;
            let color = cx.faces.bg(FaceId::CURSOR);
            let w = if cursor.shape == CursorShape::Beam { 2.0 } else { m.char_w };
            if focused {
                frame.set_cursor(CursorVisual { x, y, w, h: m.line_h, color });
            } else {
                frame.draw_rect_outline(Rect::new(x, y, m.char_w, m.line_h), 1.0, color);
            }
        }
    }
}
