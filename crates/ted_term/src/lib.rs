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
//!
//! Programs run in the terminal host (`host`), a process ted starts when it first needs
//! one, so a named workspace's terminals keep running after ted quits and the next ted
//! reattaches them, screen and history included (`terminal.scrollback` lines of it).
//! Killing a terminal's buffer ends its program, as does quitting while only the unnamed
//! workspace shows it.

mod client;
pub mod host;
mod input;
mod palette;
mod protocol;
mod render;
mod replay;
mod session;
mod snapshot;

use std::collections::HashSet;
use std::io;
use std::path::PathBuf;
use std::rc::Rc;

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use ted_core::kill_ring::KillMode;
use ted_core::settings::Setting;
use ted_core::{Arg, BufferId, Editor, KeymapDef, Mode, Plugin};

use crate::client::{Connection, Launch};
use crate::protocol::{End, ToHost};
use crate::render::TermRenderer;
use crate::session::{session, Session, TermBuffer};

/// The terminal plugin. `TermPlugin::default()` runs the user's login shell, in a host on
/// a thread of this process (terminals end with it); `persistent` runs them in the host
/// process instead.
pub struct TermPlugin {
    shell: Option<(String, Vec<String>)>,
    launch: Launch,
}

impl Default for TermPlugin {
    fn default() -> Self {
        Self { shell: None, launch: Launch::Thread }
    }
}

impl TermPlugin {
    /// Runs `program args...` instead of the login shell.
    pub fn with_shell(program: &str, args: &[&str]) -> Self {
        let shell = Some((program.to_string(), args.iter().map(|a| a.to_string()).collect()));
        Self { shell, ..Self::default() }
    }

    /// Runs terminals in the host process, started as `exe --term-host <socket>` (see
    /// `host`), so they outlive this ted.
    pub fn persistent(self, exe: PathBuf) -> Self {
        Self { launch: Launch::Process(exe), ..self }
    }
}

/// Editor-wide terminal configuration.
struct Config {
    /// The shell new terminals run (`None` for the login shell).
    shell: Option<(String, Vec<String>)>,
    launch: Launch,
    scrollback: Setting<i64>,
}

/// The connection to the host, made when a terminal first needs it.
#[derive(Default)]
struct HostLink(Option<Rc<Connection>>);

fn config(ed: &Editor) -> &Config {
    ed.ext::<Config>().expect("set when the terminal plugin starts")
}

/// The connection to the host, starting the host first if none is running and `start`.
fn connection(ed: &mut Editor, start: bool) -> io::Result<Rc<Connection>> {
    if let Some(connection) = ed.ext::<HostLink>().and_then(|link| link.0.clone()).filter(|c| c.is_alive()) {
        return Ok(connection);
    }
    let connection = Rc::new(Connection::open(&config(ed).launch, start)?);
    ed.ext_mut::<HostLink>().0 = Some(connection.clone());
    Ok(connection)
}

const TERMINAL_MODE: &str = "Terminal";
const VIEW_MODE: &str = "Terminal View";

/// Lines scrolled per wheel notch when the program handles scrolling itself.
const WHEEL_LINES: usize = 3;

/// The first of "terminal", "terminal 2", ... no buffer of the active workspace has, so
/// each workspace numbers its own terminals.
fn unique_name(ed: &Editor) -> String {
    let taken = ed.workspaces.active().buffers();
    (1..)
        .map(|n| if n == 1 { "terminal".to_string() } else { format!("terminal {}", n) })
        .find(|name| taken.iter().all(|&id| ed.buffers[id].name() != name))
        .expect("some name is free")
}

/// Shows the active workspace's terminal `step` places from the active one, in the order
/// of their numbers and wrapping around, in the active window.
fn cycle(ed: &mut Editor, step: isize) {
    let is_terminal = |id| ed.buffers[id].local::<TermBuffer>().is_some_and(|t| t.0.is_some());
    let mut terminals: Vec<BufferId> =
        ed.workspaces.active().buffers().iter().copied().filter(|&id| is_terminal(id)).collect();
    if terminals.is_empty() {
        return ed.set_status("No terminals in this workspace");
    }
    // Shorter names first puts "terminal 10" after "terminal 9".
    terminals.sort_by(|&a, &b| {
        let (a, b) = (ed.buffers[a].name(), ed.buffers[b].name());
        a.len().cmp(&b.len()).then_with(|| a.cmp(b))
    });
    let len = terminals.len() as isize;
    let next = match terminals.iter().position(|&id| id == ed.active_buffer_id()) {
        Some(i) => (i as isize + step).rem_euclid(len),
        None if step > 0 => 0,
        None => len - 1,
    };
    ed.show_in_active_view(terminals[next as usize]);
}

/// Opens a terminal in the active buffer's working directory: terminal `attach` (one an
/// earlier ted left running) if given, else a new one running the user's shell.
fn open(ed: &mut Editor, attach: Option<u64>) {
    let dir = ed.active_buffer().directory();
    let id = ed.new_buffer(unique_name(ed), TERMINAL_MODE);
    ed.buffers[id].set_directory(&dir);
    match start(ed, id, attach) {
        Ok(()) => ed.show_buffer(id),
        Err(e) => {
            ed.kill_buffer(id);
            ed.set_status(format!("Cannot start terminal: {}", e));
        }
    }
}

/// Makes terminal buffer `id` show terminal `attach`, or a new one running the user's
/// shell in the buffer's directory. The buffer saves the terminal's id, for a later ted
/// to reattach it.
fn start(ed: &mut Editor, id: BufferId, attach: Option<u64>) -> io::Result<()> {
    let connection = connection(ed, true)?;
    let config = config(ed);
    let scrollback = ed.settings.get(config.scrollback).max(0) as usize;
    let session = match attach {
        Some(terminal) => Session::attach(ed, connection, id, terminal, scrollback),
        None => {
            let shell = session::shell(config.shell.clone(), &ed.buffers[id].directory());
            Session::spawn(ed, connection, id, &shell, scrollback)
        }
    };
    let renderer = TermRenderer { handle: session.handle.clone() };
    let buf = &mut ed.buffers[id];
    buf.set_restore_argument(Some(session.handle.id.to_string()));
    buf.set_renderer(Some(Box::new(renderer)));
    buf.local_mut::<TermBuffer>().0 = Some(session);
    Ok(())
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
        let (col, row) = (size.cols as usize / 2 + 1, size.lines as usize / 2 + 1);
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
    if session.ended {
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

/// Terminal buffer `id`'s terminal is no longer attached to this ted. A shell that exited
/// (`exit`, `C-d`) closes the terminal, and its windows go back to what they showed
/// before; a terminal to reattach that is gone is replaced by a new shell.
pub(crate) fn ended(ed: &mut Editor, id: BufferId, end: End) {
    let Some(session) = session(ed, id) else { return };
    session.ended = true;
    if end == End::Missing {
        if let Some(session) = ed.buffers[id].local_mut::<TermBuffer>().0.take() {
            session.shutdown();
        }
        if start(ed, id, None).is_ok() {
            return;
        }
    }
    let name = ed.buffers[id].name().to_string();
    ed.kill_buffer(id);
    ed.set_status(match end {
        End::Exited(Some(code)) if code != 0 => format!("{} exited with code {}", name, code),
        End::Exited(_) => format!("{} exited", name),
        End::Missing => format!("Cannot start {}", name),
        End::Failed(reason) => format!("Cannot run {}: {}", name, reason),
        End::Detached => format!("{} was attached in another ted", name),
    });
}

/// As the editor quits: terminals only the unnamed workspace shows end, since no later ted
/// could reattach them. A restart keeps them all, to reattach them.
fn quitting(ed: &mut Editor) {
    if ed.restart.is_some() {
        return;
    }
    let named = ed.workspaces.loaded().iter().filter(|ws| ws.name.is_some());
    let kept: HashSet<BufferId> = named.flat_map(|ws| ws.buffers().iter().copied()).collect();
    for id in ed.buffers.ids() {
        if kept.contains(&id) {
            continue;
        }
        if let Some(session) = session(ed, id) {
            session.shutdown();
            session.ended = true;
        }
    }
}

impl Plugin for TermPlugin {
    fn name(&self) -> &str {
        "terminal"
    }

    fn init(&mut self, ed: &mut Editor) {
        let term_face = palette::register_faces(&mut ed.faces);
        let scrollback = ed.settings.define(
            "terminal.scrollback",
            2000,
            "Lines of history each terminal keeps, and a ted reattaching it is shown",
        );
        ed.set_ext(Config { shell: self.shell.clone(), launch: self.launch.clone(), scrollback });

        let c = &mut ed.commands;
        // With a terminal's id (restoring a session), reattaches that terminal.
        c.register("term", "Open a terminal in the current directory", |ed, arg| {
            open(ed, arg.int().map(|id| id as u64))
        });
        c.register_hidden("term-send-key", "Send the typed key to the terminal program", send_key);
        c.register("term-paste", "Paste the clipboard into the terminal", |ed, _| paste(ed));
        c.register("term-next", "Show the workspace's next terminal", |ed, _| cycle(ed, 1));
        c.register("term-previous", "Show the workspace's previous terminal", |ed, _| cycle(ed, -1));
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
        ed.hooks.on_quit(quitting);
        // A deleted workspace's terminals end with it.
        ed.hooks.on_restore_discarded(|ed, command, argument| {
            let Ok(id) = argument.parse() else { return };
            if command == "term" {
                if let Ok(connection) = connection(ed, false) {
                    connection.send(&ToHost::Kill { id });
                }
            }
        });
    }
}
