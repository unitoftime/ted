//! Renders a view into a `Frame`.
//!
//! One path handles wrapped and unwrapped views: each logical line is laid out once as a
//! `DisplayLine`, then drawn as one or more row segments. Everything styled on a line —
//! syntax, buffer decorations, the selection, search matches — becomes a face span over
//! visual columns: spans with a background paint rects, the rest restyle the text.

use std::ops::Range;

use crate::brackets;
use crate::buffer::find_in_slice;
use crate::doc::Doc;
use crate::face::{Face, FaceId, Faces};
use crate::frame::{CursorVisual, Frame, Metrics, Rect};
use crate::view::{DisplayLine, JumpLabel};

pub struct RenderCtx<'a> {
    pub faces: &'a Faces,
    pub metrics: Metrics,
    /// Width of the primary cursor (a full cell for box cursors).
    pub cursor_w: f32,
}

/// Draws a buffer's content itself instead of its text, e.g. a terminal's live screen.
/// Installed with `Buffer::set_renderer`; the window still draws its modeline.
pub trait BufferRenderer {
    /// Draws into `area`, the window minus its modeline.
    fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool, cx: &RenderCtx);
}

/// A face applied over visual columns of one line.
struct Span {
    cols: Range<usize>,
    face: FaceId,
}

impl Doc<'_> {
    pub fn render(&mut self, frame: &mut Frame, bounds: Rect, focused: bool, cx: &RenderCtx) {
        let m = cx.metrics;
        self.view.bounds = bounds;
        self.view.metrics = m;
        if bounds.is_empty() || m.line_h <= 0.0 || m.char_w <= 0.0 {
            return;
        }
        if let Some(renderer) = self.buf.renderer_mut() {
            let area = Rect::new(bounds.x, bounds.y, bounds.w, (bounds.h - m.line_h).max(0.0));
            renderer.render(frame, area, focused, cx);
            self.render_modeline(frame, bounds, focused, cx, None);
            return;
        }
        if self.view.scrolled_from != Some(self.pos()) {
            self.ensure_cursor_visible();
        }

        let faces = cx.faces;
        let text_rows = self.view.text_rows();
        let gutter_cols = self.gutter_cols();
        let margin_cols = self.margin_cols();
        let gutter_w = gutter_cols as f32 * m.char_w;
        let margin_w = margin_cols as f32 * m.char_w;
        let content_cols = self.content_cols();
        let wrap = self.view.wrap;
        let text_x = bounds.x + gutter_w;
        let text_w = (bounds.w - gutter_w).max(0.0);
        let text_h = (bounds.h - m.line_h).max(0.0);
        let content_clip = Rect::new(text_x, bounds.y, text_w, text_h);
        let gutter_clip = Rect::new(bounds.x, bounds.y, gutter_w.min(bounds.w), text_h);
        let margin_clip = Rect::new(bounds.x, bounds.y, margin_w.min(bounds.w), text_h);

        let mode = self.buf.mode().clone();
        frame.fill_rect(bounds, faces.bg(mode.face.unwrap_or(FaceId::DEFAULT)));
        // The background is filled above; text only takes the mode face's colors.
        let mode_face = mode.face.map_or_else(Face::default, |f| Face { bg: None, ..faces.get(f) });
        let line_numbers = gutter_cols > margin_cols;
        if gutter_cols > 0 {
            frame.fill_rect(gutter_clip, faces.bg(FaceId::LINE_NUMBER));
        }

        let total_lines = self.buf.len_lines();
        let top = self.view.top_line;
        let visible_lines = top..(top + text_rows).min(total_lines);
        let tokens = self.buf.highlight(visible_lines.clone());
        let (cur_line, cur_col) = self.buf.char_to_point(self.pos());
        let region = self.region();
        let line_face = mode.line_face.clone();
        let visible_chars = self.buf.line_to_char(visible_lines.start)..self.buf.line_to_char(visible_lines.end);
        let decorations: Vec<_> = self.buf.decorations().overlapping(visible_chars).cloned().collect();
        // Highlighting just parsed, so the tree is current.
        let brackets =
            if focused { brackets::matching_pair(self.buf.text(), self.buf.syntax_tree(), self.pos()) } else { None };
        let margin = self.buf.margin();

        let mut cursor_at: Option<(f32, f32)> = None;
        let mut screen_row = 0;
        for line in visible_lines.clone() {
            if screen_row >= text_rows {
                break;
            }
            let content = self.buf.line_content(line);
            let mut dl = self.display_line(line);
            let line_start = self.buf.line_to_char(line);
            let line_end = line_start + dl.char_len();
            let jump_spans = self.view.jump.as_deref().map(|labels| {
                let on_line = &labels
                    [labels.partition_point(|l| l.pos < line_start)..labels.partition_point(|l| l.pos < line_end)];
                write_labels(&mut dl, on_line, line_start)
            });
            let is_current = line == cur_line;
            let whole_line_face = line_face.as_ref().and_then(|f| match content.as_str() {
                Some(text) => f(text),
                None => f(&content.to_string()),
            });

            // Columns of a char range clipped to this line; running past the end covers the newline.
            let cols_of = |range: &Range<usize>| -> Option<Range<usize>> {
                let (start, end) = (range.start.max(line_start), range.end.min(line_end + 1));
                (start < end).then(|| {
                    let end_col = if end > line_end { dl.width() + 1 } else { dl.col(end - line_start) };
                    dl.col(start - line_start)..end_col
                })
            };

            // During a jump only the labels are styled: the rest of the text is dimmed.
            let spans = jump_spans.unwrap_or_else(|| {
                let mut spans: Vec<Span> = Vec::new();
                if whole_line_face.is_none() {
                    for t in tokens.get(line - top).into_iter().flatten() {
                        spans.push(Span { cols: dl.col(t.start_col)..dl.col(t.end_col), face: t.kind.face() });
                    }
                }
                for d in &decorations {
                    if let Some(cols) = cols_of(&d.range) {
                        spans.push(Span { cols, face: d.face });
                    }
                }
                if let Some(cols) = region.as_ref().and_then(cols_of) {
                    spans.push(Span { cols, face: FaceId::REGION });
                }
                for pos in brackets.into_iter().flat_map(|(a, b)| [a, b]) {
                    if let Some(cols) = cols_of(&(pos..pos + 1)) {
                        spans.push(Span { cols, face: FaceId::MATCH_PAREN });
                    }
                }
                if let Some(pattern) = self.view.highlight.as_deref().filter(|p| !p.is_empty()) {
                    let len = pattern.chars().count();
                    for m in find_in_slice(content, pattern) {
                        let current = is_current && (m..=m + len).contains(&cur_col);
                        let face = if current { FaceId::SEARCH } else { FaceId::LAZY_HIGHLIGHT };
                        spans.push(Span { cols: dl.col(m)..dl.col(m + len), face });
                    }
                }
                spans
            });

            let (segments, first_col) = if wrap { (dl.rows(content_cols), 0) } else { (1, self.view.left_col) };
            let (cursor_row, cursor_col) =
                if wrap { dl.row_col(cur_col, content_cols) } else { (0, dl.col(cur_col).saturating_sub(first_col)) };
            let base = match whole_line_face {
                _ if self.view.jump.is_some() => mode_face.merge(faces.get(FaceId::SHADOW)),
                Some(face) => mode_face.merge(faces.get(face)),
                None => mode_face,
            };

            for segment in 0..segments {
                if screen_row >= text_rows {
                    break;
                }
                let y = bounds.y + screen_row as f32 * m.line_h;
                let cols = first_col + segment * content_cols..first_col + (segment + 1) * content_cols;

                if is_current && focused && mode.highlight_line {
                    frame.fill_rect(Rect::new(text_x, y, text_w, m.line_h), faces.bg(FaceId::CURRENT_LINE));
                }
                for span in &spans {
                    let Some(bg) = faces.get(span.face).bg else { continue };
                    let (start, end) = (span.cols.start.max(cols.start), span.cols.end.min(cols.end));
                    if start < end {
                        let x = text_x + (start - cols.start) as f32 * m.char_w;
                        frame.fill_rect(Rect::new(x, y, (end - start) as f32 * m.char_w, m.line_h), bg);
                    }
                }
                if let Some(annotation) = margin.and_then(|mg| mg.lines.get(line)).filter(|_| segment == 0) {
                    let mut x = bounds.x;
                    for (text, face) in annotation.runs() {
                        let face =
                            faces.get(FaceId::LINE_NUMBER).merge(face.map_or_else(Face::default, |f| faces.get(f)));
                        frame.draw_text_clipped(x, y, text, faces.style(face), margin_clip);
                        x += text.chars().count() as f32 * m.char_w;
                    }
                }
                if line_numbers && segment == 0 {
                    let face = if is_current && focused {
                        faces.get(FaceId::LINE_NUMBER).merge(faces.get(FaceId::LINE_NUMBER_CURRENT))
                    } else {
                        faces.get(FaceId::LINE_NUMBER)
                    };
                    let number = format!("{:>w$} ", line + 1, w = (gutter_cols - margin_cols).saturating_sub(1));
                    frame.draw_text_clipped(bounds.x + margin_w, y, number, faces.style(face), gutter_clip);
                }

                let visible = cols.start.min(dl.width())..cols.end.min(dl.width());
                draw_runs(frame, &dl, &spans, base, visible, (text_x, y), m.char_w, content_clip, faces);

                if is_current && segment == cursor_row {
                    cursor_at = Some((text_x + cursor_col as f32 * m.char_w, y));
                }
                screen_row += 1;
            }
        }

        self.render_modeline(frame, bounds, focused, cx, Some((cur_line, cur_col)));

        if let Some((x, y)) = cursor_at {
            if x >= text_x && x < bounds.x + bounds.w {
                let rect = Rect::new(x, y, cx.cursor_w, m.line_h);
                let color = faces.bg(FaceId::CURSOR);
                if focused {
                    frame.set_cursor(CursorVisual { x, y, w: rect.w, h: rect.h, color });
                } else {
                    frame.draw_rect_outline(rect, 1.0, color);
                }
            }
        }
    }

    /// Name, state and mode; `position` adds line/column info for text buffers.
    fn render_modeline(
        &self,
        frame: &mut Frame,
        bounds: Rect,
        focused: bool,
        cx: &RenderCtx,
        position: Option<(usize, usize)>,
    ) {
        let m = cx.metrics;
        let face = if focused { FaceId::MODE_LINE } else { FaceId::MODE_LINE_INACTIVE };
        let y = bounds.y + bounds.h - m.line_h;
        let rect = Rect::new(bounds.x, y, bounds.w, m.line_h);
        frame.fill_rect(rect, cx.faces.bg(face));

        let location = match position {
            Some((line, col)) => {
                let total = self.buf.len_lines();
                let pct = if total > 1 { line * 100 / (total - 1) } else { 100 };
                let wrap = if self.view.wrap { " [Wrap]" } else { "" };
                format!("{}   L{}:C{}   {}% ({} lines)", wrap, line + 1, col + 1, pct, total)
            }
            None => String::new(),
        };
        let dirty = if self.buf.is_dirty() { "*" } else { "-" };
        let text = format!(" {} {}{}   ({}) ", dirty, self.buf.name(), location, self.buf.mode().name);
        frame.draw_text_clipped(bounds.x + 8.0, y, text, cx.faces.text(face), rect);
    }
}

/// Writes each jump label over the cells of its target (extending the row when a label
/// runs past the line's end) and returns the spans that color them.
fn write_labels(dl: &mut DisplayLine, labels: &[JumpLabel], line_start: usize) -> Vec<Span> {
    labels
        .iter()
        .map(|label| {
            let start = dl.col(label.pos - line_start);
            let mut end = start;
            for ch in label.text.chars() {
                match dl.cells.get_mut(end) {
                    Some(cell) => *cell = ch,
                    None => dl.cells.push(ch),
                }
                end += 1;
            }
            Span { cols: start..end, face: FaceId::JUMP_LABEL }
        })
        .collect()
}

/// Draws `cols` of a display line as runs of identically styled text: `base`, then each
/// span's face layered in order.
#[allow(clippy::too_many_arguments)]
fn draw_runs(
    frame: &mut Frame,
    dl: &DisplayLine,
    spans: &[Span],
    base: Face,
    cols: Range<usize>,
    (x, y): (f32, f32),
    char_w: f32,
    clip: Rect,
    faces: &Faces,
) {
    if cols.is_empty() {
        return;
    }
    let mut cell_faces = vec![base; cols.len()];
    for span in spans {
        let face = Face { bg: None, ..faces.get(span.face) };
        if face == Face::default() {
            continue;
        }
        let (start, end) = (span.cols.start.max(cols.start), span.cols.end.min(cols.end));
        for cell in cell_faces.iter_mut().take(end.saturating_sub(cols.start)).skip(start.saturating_sub(cols.start)) {
            *cell = cell.merge(face);
        }
    }

    let mut run_start = 0;
    while run_start < cell_faces.len() {
        let face = cell_faces[run_start];
        let run_len = cell_faces[run_start..].iter().take_while(|f| **f == face).count();
        let text: String = dl.cells[cols.start + run_start..cols.start + run_start + run_len].iter().collect();
        frame.draw_text_clipped(x + run_start as f32 * char_w, y, text, faces.style(face), clip);
        run_start += run_len;
    }
}
