//! A terminal for ted. Like any outside plugin, it uses nothing but `ted_core`'s plugin API.
//!
//! Terminal buffers have two modes, like tmux:
//!
//! - **Terminal mode**: keys go to the program (`C-c`, `C-r`, arrows...), except the
//!   editor's `C-x` commands (`C-x b`, `C-x o`, ...), `M-x` and whatever `init.rhai` binds
//!   in the `terminal` keymap; `C-x` sequences the editor doesn't bind are passed on. The
//!   screen is drawn straight from the emulator, so full-screen programs work. `C-]`
//!   switches to view mode, `C-S-v` or `C-y` pastes, and the mouse wheel scrolls programs
//!   that want it or else enters view mode. Dragging the mouse selects on the live screen
//!   (a double-click selects lines); `M-w` or `C-S-c` copies the selection.
//! - **View mode**: a frozen snapshot of scrollback + screen as a read-only buffer, where
//!   every editor command works (search, mark, copy, windows, M-x). It opens showing the
//!   same screen in the same cells and colors, without line numbers. `C-]` or `q` returns
//!   to the live terminal.
//!
//! `M-x term` or `C-x t` opens a terminal in the current buffer's directory. When the shell
//! exits (`exit`, `C-d`) the terminal buffer closes. Flow control is off, so `C-s` reaches
//! the program instead of freezing output.

mod input;
mod palette;
mod render;
mod session;
mod snapshot;

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use ted_core::kill_ring::KillMode;
use ted_core::{Arg, BufferId, Editor, KeymapDef, Mode, Plugin};

use crate::render::TermRenderer;
use crate::session::{session, Session, TermBuffer};

/// The terminal plugin. `TermPlugin::default()` runs the user's login shell.
#[derive(Default)]
pub struct TermPlugin {
    shell: Option<(String, Vec<String>)>,
}

impl TermPlugin {
    /// Runs `program args...` instead of the login shell.
    pub fn with_shell(program: &str, args: &[&str]) -> Self {
        Self { shell: Some((program.to_string(), args.iter().map(|a| a.to_string()).collect())) }
    }
}

/// Editor-wide: the shell new terminals run (`None` for the login shell).
#[derive(Default)]
struct ShellCommand(Option<(String, Vec<String>)>);

const TERMINAL_MODE: &str = "Terminal";
const VIEW_MODE: &str = "Terminal View";

/// Lines scrolled per wheel notch when the program handles scrolling itself.
const WHEEL_LINES: usize = 3;

fn unique_name(ed: &Editor) -> String {
    (1..)
        .map(|n| if n == 1 { "terminal".to_string() } else { format!("terminal {}", n) })
        .find(|name| ed.buffers.find(|b| b.name() == name).is_none())
        .expect("some name is free")
}

/// Opens a new terminal running the user's shell in the active buffer's working directory.
fn open(ed: &mut Editor) {
    let dir = ed.active_buffer().directory();
    let id = ed.new_buffer(unique_name(ed), TERMINAL_MODE);
    ed.buffers[id].set_directory(&dir);
    let shell = session::shell(ed.ext::<ShellCommand>().and_then(|s| s.0.clone()), &dir);
    match Session::spawn(ed, id, &shell) {
        Ok(session) => {
            let renderer = TermRenderer { handle: session.handle.clone() };
            let buf = &mut ed.buffers[id];
            buf.set_renderer(Some(Box::new(renderer)));
            buf.local_mut::<TermBuffer>().0 = Some(session);
            ed.show_buffer(id);
        }
        Err(e) => {
            ed.kill_buffer(id);
            ed.set_status(format!("Cannot start terminal: {}", e));
        }
    }
}

/// Terminal mode's fallback: sends the key to the program.
fn send_key(ed: &mut Editor, arg: &Arg) {
    let id = ed.active_buffer_id();
    let Some(session) = session(ed, id) else { return };
    let Some(key) = arg.key() else { return };
    let mode = {
        let mut term = session.handle.term.lock();
        term.selection = None;
        *term.mode()
    };
    if let Some(bytes) = input::encode(key, mode) {
        session.handle.scroll_to_bottom();
        session.write(bytes);
    }
}

fn paste(ed: &mut Editor) {
    ed.kill_ring.sync_from_system_clipboard();
    let Some(text) = ed.kill_ring.current().map(str::to_string) else { return };
    let id = ed.active_buffer_id();
    let Some(session) = session(ed, id) else { return };
    let bracketed = {
        let mut term = session.handle.term.lock();
        term.selection = None;
        term.mode().contains(TermMode::BRACKETED_PASTE)
    };
    let bytes = if bracketed {
        format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', ""))
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r")
    };
    session.write(bytes.into_bytes());
}

/// Starts a selection of type `ty` at the mouse (`Arg::Point`), or with `None` extends
/// the current one there.
fn select(ed: &mut Editor, arg: &Arg, ty: Option<SelectionType>) {
    let Some((x, y)) = arg.point() else { return };
    let (row, boundary) = ed.active_view().grid_point(x, y);
    let id = ed.active_buffer_id();
    let Some(session) = session(ed, id) else { return };
    let mut term = session.handle.term.lock();
    let (cols, lines) = (term.columns(), term.screen_lines());
    let line = Line(row.min(lines - 1) as i32 - term.grid().display_offset() as i32);
    // A boundary is the left side of the cell after it; the last one is the right side of
    // the last cell.
    let (point, side) = match boundary {
        b if b >= cols => (Point::new(line, Column(cols - 1)), Side::Right),
        b => (Point::new(line, Column(b)), Side::Left),
    };
    match ty {
        Some(ty) => term.selection = Some(Selection::new(ty, point, side)),
        None => {
            if let Some(selection) = term.selection.as_mut() {
                selection.update(point, side);
            }
        }
    }
}

fn copy(ed: &mut Editor) {
    let id = ed.active_buffer_id();
    let Some(session) = session(ed, id) else { return };
    let text = {
        let mut term = session.handle.term.lock();
        let text = term.selection_to_string();
        term.selection = None;
        text
    };
    match text.filter(|t| !t.is_empty()) {
        Some(text) => {
            ed.kill_ring.push(text, KillMode::New);
            ed.set_status("Copied selection");
        }
        None => ed.set_status("Nothing selected (drag the mouse to select)"),
    }
}

/// The wheel in terminal mode: programs that track the mouse get wheel events; full-screen
/// programs without mouse support get arrow keys; a plain shell switches to view mode.
fn wheel(ed: &mut Editor, lines: usize, up: bool) {
    let id = ed.active_buffer_id();
    let Some(session) = session(ed, id) else { return };
    let (mode, size) = (*session.handle.term.lock().mode(), session.handle.size());
    if mode.intersects(TermMode::MOUSE_MODE) {
        let button = if up { 64 } else { 65 };
        let (col, row) = (size.cols / 2 + 1, size.lines / 2 + 1);
        let event = if mode.contains(TermMode::SGR_MOUSE) {
            format!("\x1b[<{};{};{}M", button, col, row)
        } else {
            let byte = |v: usize| (32 + v.min(223)) as u8 as char;
            format!("\x1b[M{}{}{}", byte(button), byte(col), byte(row))
        };
        session.write(event.repeat(lines.max(1)).into_bytes());
    } else if mode.contains(TermMode::ALT_SCREEN) {
        let arrow = if up { "A" } else { "B" };
        let prefix = if mode.contains(TermMode::APP_CURSOR) { "\x1bO" } else { "\x1b[" };
        session.write(format!("{}{}", prefix, arrow).repeat(lines * WHEEL_LINES).into_bytes());
    } else if up {
        view_mode(ed, id);
        let mut doc = ed.doc();
        doc.scroll(-((lines * WHEEL_LINES) as isize));
    }
}

/// Freezes terminal `id` into a read-only buffer for navigating with editor commands.
/// Windows showing it land on the terminal's cursor.
fn view_mode(ed: &mut Editor, id: BufferId) {
    let Some(term) = session(ed, id).map(|s| s.handle.term.clone()) else { return };
    let snap = {
        let term = term.lock();
        snapshot::take(&term, &mut ed.faces)
    };
    ed.set_buffer_mode(id, VIEW_MODE);
    let buf = &mut ed.buffers[id];
    buf.set_renderer(None);
    buf.set_styled("terminal", snap.text);
    let pos = buf.point_to_char(snap.cursor.0, snap.cursor.1);
    // Scrolled like the live screen, so every character stays in its cell.
    for view in ed.layout.views_showing(id) {
        view.goto(pos);
        view.top_line = snap.screen_top;
        view.top_row = 0;
    }
}

fn terminal_mode(ed: &mut Editor) {
    let id = ed.active_buffer_id();
    let Some(session) = session(ed, id) else { return };
    if session.exited {
        ed.set_status("The terminal's process has exited");
        return;
    }
    session.handle.term.lock().scroll_display(Scroll::Bottom);
    let renderer = TermRenderer { handle: session.handle.clone() };
    ed.set_buffer_mode(id, TERMINAL_MODE);
    let buf = &mut ed.buffers[id];
    buf.set_text("");
    buf.set_renderer(Some(Box::new(renderer)));
    ed.set_status("");
}

/// The shell exited (`exit`, `C-d`): close the terminal; its windows go back to what they
/// showed before.
pub(crate) fn exited(ed: &mut Editor, id: BufferId, code: Option<i32>) {
    let Some(session) = session(ed, id) else { return };
    session.exited = true;
    let name = ed.buffers[id].name().to_string();
    ed.kill_buffer(id);
    ed.set_status(match code {
        Some(code) if code != 0 => format!("{} exited with code {}", name, code),
        _ => format!("{} exited", name),
    });
}

impl Plugin for TermPlugin {
    fn name(&self) -> &str {
        "terminal"
    }

    fn init(&mut self, ed: &mut Editor) {
        let term_face = palette::register_faces(&mut ed.faces);
        ed.ext_mut::<ShellCommand>().0 = self.shell.clone();

        let c = &mut ed.commands;
        c.register("term", "Open a terminal in the current directory", |ed, _| open(ed));
        c.register_hidden("term-send-key", "Send the typed key to the terminal program", send_key);
        c.register("term-paste", "Paste the clipboard into the terminal", |ed, _| paste(ed));
        c.register("term-view-mode", "Freeze the terminal to navigate it with editor keys", |ed, _| {
            let id = ed.active_buffer_id();
            view_mode(ed, id);
            ed.set_status("Terminal view mode: editor keys work here; C-] or q returns");
        });
        c.register("term-terminal-mode", "Return to the live terminal", |ed, _| terminal_mode(ed));
        c.register("term-copy", "Copy the terminal's mouse selection", |ed, _| copy(ed));
        c.register_hidden("term-mouse-select", "Start selecting at the mouse", |ed, arg| {
            select(ed, arg, Some(SelectionType::Simple))
        });
        c.register_hidden("term-mouse-select-line", "Select the line under the mouse", |ed, arg| {
            select(ed, arg, Some(SelectionType::Lines))
        });
        c.register_hidden("term-mouse-drag", "Extend the selection to the mouse", |ed, arg| select(ed, arg, None));
        c.register_hidden("term-wheel-up", "Scroll the terminal up", |ed, arg| {
            wheel(ed, arg.int().unwrap_or(1) as usize, true)
        });
        c.register_hidden("term-wheel-down", "Scroll the terminal down", |ed, arg| {
            wheel(ed, arg.int().unwrap_or(1) as usize, false)
        });

        ed.define_mode(
            Mode::new(TERMINAL_MODE)
                .read_only()
                .restore("term")
                // The editor keeps its C-x commands and M-x; unbound C-x sequences still
                // reach the program. Other editor keys are bound in the `terminal` keymap
                // (from `init.rhai`), since bindings there win over passing the key through.
                .keymap(|k| k.fallback("term-send-key").fallback_exempt(&["C-x", "M-x"]))
                .keys(&[
                    ("C-]", "term-view-mode"),
                    ("C-S-v", "term-paste"),
                    ("C-y", "term-paste"),
                    ("M-w", "term-copy"),
                    ("C-S-c", "term-copy"),
                    ("<mouse-1>", "term-mouse-select"),
                    ("<double-mouse-1>", "term-mouse-select-line"),
                    ("<drag-mouse-1>", "term-mouse-drag"),
                    ("<wheel-up>", "term-wheel-up"),
                    ("<wheel-down>", "term-wheel-down"),
                ]),
        );
        // Looks like the live terminal, so switching modes doesn't move anything.
        ed.define_mode(
            Mode::new(VIEW_MODE)
                .read_only()
                .restore("term")
                .face(term_face)
                .line_numbers(false)
                .highlight_line(false)
                .keys(&[("C-]", "term-terminal-mode"), ("q", "term-terminal-mode")]),
        );
        ed.define_keymap(KeymapDef::new("global").keys(&[("C-x t", "term")]));

        ed.hooks.on_buffer_killed(|ed, id| {
            if let Some(session) = session(ed, id) {
                session.shutdown();
            }
        });
    }
}
