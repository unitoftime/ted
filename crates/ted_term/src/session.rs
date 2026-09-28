//! A terminal as ted holds it: its id on the host (see `host`), ted's copy of its
//! emulator, and the bridge that turns the copy's events into UI-thread work.

use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{self, Term};
use ted_core::jobs::{JobContext, JobHandle};
use ted_core::kill_ring::KillMode;
use ted_core::process::Program;
use ted_core::{BufferId, Editor};

use crate::client::{Connection, Mirror};
use crate::palette::Palette;
use crate::protocol::{End, Size, Spawn, ToHost};

/// A terminal's size until a window draws it.
const INITIAL_SIZE: Size = Size { cols: 80, lines: 24, cell_width: 0, cell_height: 0 };

/// Receives the emulator copy's events on the connection's thread and forwards them to the
/// UI thread. Answers to programs' queries come from the host, so the copy's are dropped.
#[derive(Clone)]
pub struct Listener {
    ctx: JobContext,
    buffer: BufferId,
    /// Set while a redraw is queued, so bursts of output wake the UI once.
    redraw_pending: Arc<AtomicBool>,
}

impl Listener {
    /// The terminal is no longer attached to this ted.
    pub(crate) fn ended(&self, end: End) {
        let id = self.buffer;
        self.ctx.send(move |ed| crate::ended(ed, id, end));
    }
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let id = self.buffer;
        match event {
            Event::Wakeup => {
                if !self.redraw_pending.swap(true, Ordering::AcqRel) {
                    let pending = self.redraw_pending.clone();
                    self.ctx.send(move |_| pending.store(false, Ordering::Release));
                }
            }
            Event::ClipboardStore(_, text) => {
                self.ctx.send(move |ed| ed.kill_ring.push(text, KillMode::New));
            }
            Event::ClipboardLoad(_, format) => {
                self.ctx.send(move |ed| {
                    ed.kill_ring.sync_from_system_clipboard();
                    let text = ed.kill_ring.current().unwrap_or_default().to_string();
                    write(ed, id, format(&text).into_bytes());
                });
            }
            Event::ColorRequest(index, format) => {
                self.ctx.send(move |ed| {
                    let palette = Palette::from_faces(&ed.faces);
                    write(ed, id, format(palette.indexed_rgb(index)).into_bytes());
                });
            }
            Event::TextAreaSizeRequest(format) => {
                self.ctx.send(move |ed| {
                    if let Some(size) = session(ed, id).map(|s| s.handle.size()) {
                        write(ed, id, format(size.window_size()).into_bytes());
                    }
                });
            }
            _ => {}
        }
    }
}

/// Shared access to an attached terminal, held by both the session and its renderer.
#[derive(Clone)]
pub struct TermHandle {
    pub term: Arc<FairMutex<Term<Listener>>>,
    /// The terminal's id on the host.
    pub id: u64,
    connection: Rc<Connection>,
    size: Rc<Cell<Size>>,
}

impl TermHandle {
    pub fn size(&self) -> Size {
        self.size.get()
    }

    pub fn write(&self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            self.connection.send(&ToHost::Input { id: self.id, bytes });
        }
    }

    /// Resizes the emulator here and on the host (the program gets SIGWINCH).
    pub fn resize(&self, size: Size) {
        if size == self.size.get() || size.cols == 0 || size.lines == 0 {
            return;
        }
        self.size.set(size);
        self.term.lock().resize(size);
        self.connection.send(&ToHost::Resize { id: self.id, size });
    }

    /// Jumps back to the live screen after scrolling the emulator's history.
    pub fn scroll_to_bottom(&self) {
        self.term.lock().scroll_display(Scroll::Bottom);
    }
}

pub struct Session {
    pub handle: TermHandle,
    job: JobHandle,
    /// Whether the terminal is no longer this ted's to end: its program exited, or another
    /// ted attached it.
    pub ended: bool,
}

/// Buffer-local slot holding a terminal buffer's session.
#[derive(Default)]
pub struct TermBuffer(pub Option<Session>);

pub fn session(ed: &mut Editor, id: BufferId) -> Option<&mut Session> {
    ed.buffers.get_mut(id)?.local_mut::<TermBuffer>().0.as_mut()
}

impl Session {
    /// Starts `shell` (see `shell`) on a new terminal keeping `scrollback` lines of
    /// history, shown in terminal buffer `buffer`.
    pub fn spawn(
        ed: &Editor,
        connection: Rc<Connection>,
        buffer: BufferId,
        shell: &Program,
        scrollback: usize,
    ) -> Session {
        let session = Session::new(ed, connection, buffer, new_id(), scrollback);
        let mut env: Vec<(String, String)> = std::env::vars().collect();
        env.extend(shell.env.iter().cloned());
        let (program, args) = without_flow_control(shell);
        let spawn =
            Spawn { program, args, dir: shell.dir.to_string_lossy().into_owned(), env, scrollback: scrollback as u32 };
        let handle = &session.handle;
        handle.connection.send(&ToHost::Create { id: handle.id, size: handle.size(), spawn });
        session
    }

    /// Attaches terminal `id`, which an earlier ted left running, to buffer `buffer`.
    pub fn attach(ed: &Editor, connection: Rc<Connection>, buffer: BufferId, id: u64, scrollback: usize) -> Session {
        let session = Session::new(ed, connection, buffer, id, scrollback);
        let handle = &session.handle;
        handle.connection.send(&ToHost::Attach { id, size: handle.size() });
        session
    }

    /// The session of terminal `id`, receiving its output from now on.
    fn new(ed: &Editor, connection: Rc<Connection>, buffer: BufferId, id: u64, scrollback: usize) -> Session {
        let (job, ctx) = ed.job_context();
        let listener = Listener { ctx, buffer, redraw_pending: Arc::new(AtomicBool::new(false)) };
        let config = term::Config { scrolling_history: scrollback, kitty_keyboard: true, ..Default::default() };
        let mirror = Mirror::new(config, INITIAL_SIZE, listener);
        let term = mirror.term.clone();
        connection.add(id, mirror);
        let handle = TermHandle { term, id, connection, size: Rc::new(Cell::new(INITIAL_SIZE)) };
        Session { handle, job, ended: false }
    }

    pub fn write(&self, bytes: Vec<u8>) {
        if !self.ended {
            self.handle.write(bytes);
        }
    }

    /// Stops showing the terminal, ending its program unless it is no longer this ted's.
    pub fn shutdown(&self) {
        self.job.cancel();
        let handle = &self.handle;
        handle.connection.remove(handle.id);
        if !self.ended {
            handle.connection.send(&ToHost::Kill { id: handle.id });
        }
    }
}

/// A new terminal id, unique across teds and hosts: the time, kept increasing, and within
/// `i64` so it reads back as a number from a saved session.
fn new_id() -> u64 {
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |t| t.as_nanos() as u64) & i64::MAX as u64;
    let previous = LAST.fetch_update(Ordering::AcqRel, Ordering::Acquire, |last| Some(now.max(last + 1)));
    previous.map_or(now, |last| now.max(last + 1))
}

/// What a new terminal runs in `dir`: `shell`, else the user's login shell.
pub fn shell(shell: Option<(String, Vec<String>)>, dir: &Path) -> Program {
    let (program, args) =
        shell.unwrap_or_else(|| (std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()), Vec::new()));
    Program::new(program, dir).args(args).env("TERM", "xterm-256color").env("COLORTERM", "truecolor")
}

/// The program and arguments that start `shell` with XON/XOFF flow control off: otherwise
/// `C-s` freezes all output until `C-q`, and Emacs habits hit `C-s` constantly. With it
/// off, `C-s` reaches the program (bash's forward history search).
fn without_flow_control(shell: &Program) -> (String, Vec<String>) {
    let flow_control_off = "stty -ixon 2>/dev/null; exec \"$0\" \"$@\"".to_string();
    let mut args = vec!["-c".to_string(), flow_control_off, shell.program.clone()];
    args.extend(shell.args.iter().cloned());
    ("/bin/sh".to_string(), args)
}

pub fn write(ed: &mut Editor, id: BufferId, bytes: Vec<u8>) {
    if let Some(session) = session(ed, id) {
        session.write(bytes);
    }
}
