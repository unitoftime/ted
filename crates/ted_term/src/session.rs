//! A running terminal: the shell's PTY, the emulator state, and the bridge that turns the
//! emulator's events into UI-thread work.

use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{self, Term};
use alacritty_terminal::tty;
use ted_core::jobs::{JobContext, JobHandle};
use ted_core::kill_ring::KillMode;
use ted_core::process::Program;
use ted_core::{BufferId, Editor};

use crate::palette::Palette;

/// Terminal size in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermSize {
    pub cols: usize,
    pub lines: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

impl TermSize {
    fn window_size(self, cell: (f32, f32)) -> WindowSize {
        WindowSize {
            num_lines: self.lines as u16,
            num_cols: self.cols as u16,
            cell_width: cell.0 as u16,
            cell_height: cell.1 as u16,
        }
    }
}

/// Receives emulator events on the PTY thread and forwards them to the UI thread.
#[derive(Clone)]
pub struct Listener {
    ctx: JobContext,
    buffer: BufferId,
    /// Set while a redraw is queued, so bursts of output wake the UI once.
    redraw_pending: Arc<AtomicBool>,
    /// Writes replies (e.g. cursor position reports) straight back to the PTY.
    writer: Arc<OnceLock<EventLoopSender>>,
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
            Event::PtyWrite(text) => {
                if let Some(writer) = self.writer.get() {
                    let _ = writer.send(Msg::Input(text.into_bytes().into()));
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
                        write(ed, id, format(size.window_size((0.0, 0.0))).into_bytes());
                    }
                });
            }
            Event::ChildExit(status) => {
                self.ctx.send(move |ed| crate::exited(ed, id, status.code()));
            }
            _ => {}
        }
    }
}

/// Shared access to a running terminal, held by both the session and its renderer.
#[derive(Clone)]
pub struct TermHandle {
    pub term: Arc<FairMutex<Term<Listener>>>,
    sender: EventLoopSender,
    size: Rc<Cell<TermSize>>,
}

impl TermHandle {
    pub fn size(&self) -> TermSize {
        self.size.get()
    }

    pub fn write(&self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            let _ = self.sender.send(Msg::Input(bytes.into()));
        }
    }

    /// Resizes the emulator and the PTY (the program gets SIGWINCH).
    pub fn resize(&self, size: TermSize, cell: (f32, f32)) {
        if size == self.size.get() || size.cols == 0 || size.lines == 0 {
            return;
        }
        self.size.set(size);
        self.term.lock().resize(size);
        let _ = self.sender.send(Msg::Resize(size.window_size(cell)));
    }

    /// Jumps back to the live screen after scrolling the emulator's history.
    pub fn scroll_to_bottom(&self) {
        self.term.lock().scroll_display(Scroll::Bottom);
    }
}

pub struct Session {
    pub handle: TermHandle,
    job: JobHandle,
    pub exited: bool,
}

/// Buffer-local slot holding a terminal buffer's session.
#[derive(Default)]
pub struct TermBuffer(pub Option<Session>);

pub fn session(ed: &mut Editor, id: BufferId) -> Option<&mut Session> {
    ed.buffers.get_mut(id)?.local_mut::<TermBuffer>().0.as_mut()
}

impl Session {
    /// Starts `shell` (see `shell`) on a new terminal, reporting to terminal buffer `buffer`.
    pub fn spawn(ed: &Editor, buffer: BufferId, shell: &Program) -> std::io::Result<Session> {
        let size = TermSize { cols: 80, lines: 24 };
        let (job, ctx) = ed.job_context();
        let writer = Arc::new(OnceLock::new());
        let listener =
            Listener { ctx, buffer, redraw_pending: Arc::new(AtomicBool::new(false)), writer: writer.clone() };

        let config = term::Config { kitty_keyboard: true, ..Default::default() };
        let term = Arc::new(FairMutex::new(Term::new(config, &size, listener.clone())));
        let options = tty::Options {
            shell: Some(without_flow_control(shell)),
            working_directory: Some(shell.dir.clone()),
            drain_on_exit: true,
            env: shell.env.iter().cloned().collect(),
        };
        let pty = tty::new(&options, size.window_size((0.0, 0.0)), buffer_window_id(buffer))?;
        let event_loop = EventLoop::new(term.clone(), listener, pty, true, false)?;
        let sender = event_loop.channel();
        let _ = writer.set(sender.clone());
        event_loop.spawn();
        let handle = TermHandle { term, sender, size: Rc::new(Cell::new(size)) };
        Ok(Session { handle, job, exited: false })
    }

    pub fn write(&self, bytes: Vec<u8>) {
        if !self.exited {
            self.handle.write(bytes);
        }
    }

    pub fn shutdown(&self) {
        self.job.cancel();
        let _ = self.handle.sender.send(Msg::Shutdown);
    }
}

/// Wraps the shell so it starts with XON/XOFF flow control off: otherwise `C-s` freezes
/// all output until `C-q`, and Emacs habits hit `C-s` constantly. With it off, `C-s`
/// reaches the program (bash's forward history search).
/// What a new terminal runs in `dir`: `shell`, else the user's login shell.
pub fn shell(shell: Option<(String, Vec<String>)>, dir: &Path) -> Program {
    let (program, args) =
        shell.unwrap_or_else(|| (std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()), Vec::new()));
    Program::new(program, dir).args(args).env("TERM", "xterm-256color").env("COLORTERM", "truecolor")
}

fn without_flow_control(shell: &Program) -> tty::Shell {
    let flow_control_off = "stty -ixon 2>/dev/null; exec \"$0\" \"$@\"".to_string();
    let mut wrapped = vec!["-c".to_string(), flow_control_off, shell.program.clone()];
    wrapped.extend(shell.args.iter().cloned());
    tty::Shell::new("/bin/sh".to_string(), wrapped)
}

pub fn write(ed: &mut Editor, id: BufferId, bytes: Vec<u8>) {
    if let Some(session) = session(ed, id) {
        session.write(bytes);
    }
}

/// A stable number per terminal, exported to the shell as `ALACRITTY_WINDOW_ID`.
fn buffer_window_id(buffer: BufferId) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    buffer.hash(&mut h);
    h.finish()
}
