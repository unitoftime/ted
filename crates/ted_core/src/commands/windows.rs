//! Windows (views), saved layouts and display toggles.

use crate::editor::Editor;
use crate::layout::SplitType;
use crate::settings::{self, Value};
use crate::theme::Theme;

pub fn register(ed: &mut Editor) {
    let c = &mut ed.commands;
    c.register("split-window-below", "Split the active window into top and bottom panes", |ed, _| {
        ed.layout.split(SplitType::Horizontal);
        ed.set_status("Split window below");
    });
    c.register("split-window-right", "Split the active window into side-by-side panes", |ed, _| {
        ed.layout.split(SplitType::Vertical);
        ed.set_status("Split window right");
    });
    c.register("delete-other-windows", "Maximize the active window, closing all others", |ed, _| {
        ed.remember_places();
        ed.layout.maximize_active();
        ed.set_status("Maximized current window");
    });
    c.register("delete-window", "Close the active window (exits when it is the last)", |ed, _| {
        ed.remember_places();
        if ed.layout.close_active() {
            ed.set_status("Closed window");
        } else {
            ed.request_exit();
        }
    });
    c.register("other-window", "Focus the next window", |ed, _| {
        ed.layout.cycle(1);
        ed.set_status("Switched to other window");
    });
    c.register("previous-window", "Focus the previous window", |ed, _| {
        ed.layout.cycle(-1);
        ed.set_status("Switched to previous window");
    });
    c.register("save-window-layout", "Save the window layout to a numbered slot", |ed, arg| {
        let slot = arg.int().unwrap_or(1) as usize;
        ed.saved_layouts.insert(slot, ed.layout.clone());
        ed.set_status(format!("Saved layout to slot {}", slot));
    });
    c.register("restore-window-layout", "Restore the window layout from a numbered slot", |ed, arg| {
        let slot = arg.int().unwrap_or(1) as usize;
        if !ed.saved_layouts.contains_key(&slot) {
            ed.set_status(format!("Slot {} is empty", slot));
            return;
        }
        ed.remember_places();
        let Editor { layout, buffers, saved_layouts, .. } = ed;
        layout.restore(&saved_layouts[&slot]);
        // Buffers may have shrunk since the layout was saved.
        for view in layout.views_mut() {
            let buffer = &buffers[view.buffer];
            view.clamp(buffer.len_chars(), buffer.len_lines());
        }
        ed.set_status(format!("Loaded layout from slot {}", slot));
    });

    c.register("quit-window", "Go back to what this window showed before", |ed, _| {
        let scratch = ed.ensure_scratch();
        let Editor { layout, buffers, .. } = ed;
        layout.active_mut().go_back(buffers, scratch);
    });
    c.register("toggle-theme", "Cycle through the built-in color themes", |ed, _| {
        let next = Theme::next_name(ed.settings.get(settings::THEME));
        if let Err(e) = ed.set_setting("theme", &Value::from(next)) {
            ed.set_status(e);
            return;
        }
        ed.set_status(format!("Theme: {}", next));
    });
    c.register("text-scale-increase", "Make the font larger", |ed, _| zoom(ed, 1));
    c.register("text-scale-decrease", "Make the font smaller", |ed, _| zoom(ed, -1));
    c.register("text-scale-reset", "Return the font to its configured size", |ed, _| zoom(ed, 0));
    c.register("toggle-word-wrap", "Toggle visual line wrapping in the active window", |ed, _| {
        let view = ed.active_view_mut();
        view.wrap = !view.wrap;
        let msg = if view.wrap { "Word wrap enabled" } else { "Word wrap disabled" };
        ed.set_status(msg);
    });
}

/// Zoom in 1px font steps, applied on top of the `font_size` setting.
#[derive(Default)]
struct Zoom(i32);

const MIN_FONT_SIZE: f64 = 6.0;

/// Adds `delta` zoom steps; 0 resets to the configured size.
fn zoom(ed: &mut Editor, delta: i32) {
    let steps = if delta == 0 { 0 } else { ed.ext::<Zoom>().map_or(0, |z| z.0) + delta };
    let base = ed.settings.get(settings::FONT_SIZE).max(MIN_FONT_SIZE);
    // Stop shrinking at the minimum instead of piling up steps that do nothing.
    ed.ext_mut::<Zoom>().0 = steps.max((MIN_FONT_SIZE - base).ceil() as i32);
    let (size, _) = ed.font_metrics();
    ed.set_status(format!("Font size {}", size));
}

impl Editor {
    /// Font size and line height in pixels: the settings with the zoom applied, the line
    /// height scaled in proportion. Frontends read this every frame.
    pub fn font_metrics(&self) -> (f32, f32) {
        let (base, line) =
            (self.settings.get(settings::FONT_SIZE).max(MIN_FONT_SIZE), self.settings.get(settings::LINE_HEIGHT));
        let size = (base + f64::from(self.ext::<Zoom>().map_or(0, |z| z.0))).max(MIN_FONT_SIZE);
        (size as f32, (line * size / base).round() as f32)
    }
}
