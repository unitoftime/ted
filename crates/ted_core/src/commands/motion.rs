//! Cursor motion, scrolling and the mark.

use crate::commands::motion;
use crate::doc::RecenterTarget;
use crate::editor::Editor;

/// Chain value of `recenter-top-bottom`: which position it used, so repeats cycle.
struct Recentered(usize);

/// Chain value of `mouse-select-line` and the drags after it: the line selected first, so
/// dragging extends the selection by whole lines.
struct LineDrag(usize);

pub fn register(ed: &mut Editor) {
    let c = &mut ed.commands;
    c.register("forward-char", "Move point right one character", |ed, _| motion(ed, |d| d.move_right()));
    c.register("backward-char", "Move point left one character", |ed, _| motion(ed, |d| d.move_left()));
    c.register("next-line", "Move point down one visual line", |ed, _| motion(ed, |d| d.move_rows(1)));
    c.register("previous-line", "Move point up one visual line", |ed, _| motion(ed, |d| d.move_rows(-1)));
    c.register("beginning-of-line", "Move point to start of line", |ed, _| motion(ed, |d| d.move_line_start()));
    c.register("end-of-line", "Move point to end of line", |ed, _| motion(ed, |d| d.move_line_end()));
    c.register("beginning-of-buffer", "Move point to beginning of buffer", |ed, _| {
        motion(ed, |d| d.move_buffer_start())
    });
    c.register("end-of-buffer", "Move point to end of buffer", |ed, _| motion(ed, |d| d.move_buffer_end()));
    c.register("forward-word", "Move point forward one word", |ed, _| motion(ed, |d| d.move_word_forward()));
    c.register("backward-word", "Move point backward one word", |ed, _| motion(ed, |d| d.move_word_backward()));
    c.register("scroll-down", "Scroll down one page, point staying on its screen line", |ed, _| {
        motion(ed, |d| d.move_page(1))
    });
    c.register("scroll-up", "Scroll up one page, point staying on its screen line", |ed, _| {
        motion(ed, |d| d.move_page(-1))
    });

    c.register_hidden("mouse-set-point", "Move point to the mouse", |ed, arg| {
        let Some((x, y)) = arg.point() else { return };
        let mut doc = ed.doc();
        if let Some(pos) = doc.pos_at_point(x, y) {
            doc.clear_mark();
            doc.set_cursor(pos);
        }
    });
    c.register_hidden("mouse-select-line", "Select the line under the mouse", |ed, arg| {
        let Some((x, y)) = arg.point() else { return };
        let mut doc = ed.doc();
        let Some(line) = doc.pos_at_point(x, y).map(|pos| doc.buf.char_to_line(pos)) else { return };
        doc.select_lines(line, line);
        ed.set_chain(LineDrag(line));
    });
    c.register_hidden("mouse-drag-region", "Extend the selection to the mouse", |ed, arg| {
        let Some((x, y)) = arg.point() else { return };
        let anchor = ed.last_chain::<LineDrag>().map(|l| l.0);
        let mut doc = ed.doc();
        let Some(pos) = doc.pos_at_point(x, y) else { return };
        match anchor {
            Some(anchor) => {
                let line = doc.buf.char_to_line(pos);
                doc.select_lines(anchor, line);
                ed.set_chain(LineDrag(anchor));
            }
            None => doc.drag_to(pos),
        }
    });

    c.register("goto-line", "Go to a line, or LINE:COLUMN", |ed, _| {
        ed.prompt("goto-line", "Goto line: ", "", goto_line)
    });

    c.register("recenter-top-bottom", "Scroll so point is centered, then at top, then bottom", |ed, _| {
        let step = ed.last_chain::<Recentered>().map_or(0, |r| (r.0 + 1) % 3);
        let target = [RecenterTarget::Center, RecenterTarget::Top, RecenterTarget::Bottom][step];
        ed.doc().recenter(target);
        ed.set_chain(Recentered(step));
    });

    c.register("set-mark-command", "Set the mark at point, or clear it", |ed, _| {
        let set = ed.focused_doc().toggle_mark();
        ed.set_status(if set { "Mark set" } else { "Mark deactivated" });
    });
    c.register("exchange-point-and-mark", "Swap point and the mark", |ed, _| {
        let swapped = ed.focused_doc().exchange_point_and_mark();
        ed.set_status(if swapped { "Exchanged point and mark" } else { "No mark set in this buffer" });
    });
    c.register("mark-whole-buffer", "Select the whole buffer", |ed, _| {
        ed.focused_doc().mark_whole_buffer();
        ed.set_status("Mark set (buffer)");
    });
}

/// Moves to `LINE` or `LINE:COLUMN` (both 1-based), remembering the old position for `M-,`.
fn goto_line(ed: &mut Editor, input: String) {
    let mut numbers = input.splitn(2, ':').map(|n| n.trim().parse::<usize>());
    let (line, col) = match (numbers.next(), numbers.next()) {
        (Some(Ok(line)), None) => (line, 1),
        (Some(Ok(line)), Some(Ok(col))) => (line, col),
        _ => {
            ed.set_status(format!("Not a line number: {}", input));
            return;
        }
    };
    crate::xref::push_mark(ed);
    let mut doc = ed.doc();
    let last = doc.buf.len_lines().saturating_sub(1);
    let pos = doc.buf.point_to_char(line.saturating_sub(1).min(last), col.saturating_sub(1));
    doc.jump_to(pos);
}
