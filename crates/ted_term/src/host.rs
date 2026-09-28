//! The terminal host: the process that owns terminals' programs, so they outlive the ted
//! that started them, as tmux's server does. ted starts it (`ted --term-host <socket>`)
//! the first time it needs a terminal and connects to it over a Unix socket; the host
//! exits once it has neither terminals nor a ted connected.
//!
//! The host keeps each terminal's emulator: the screen and history a ted attaching later
//! is drawn from (`replay`), and the answers to programs' queries while no ted is there.
//! Output goes to the attached ted as it is read, where a copy of the emulator feeds on it
//! and handles what needs the editor (colors, the clipboard, redraws).

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::Duration;

use alacritty_terminal::event::{Event, EventListener, OnResize, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{self, Term};
use alacritty_terminal::tty::{self, ChildEvent, EventedPty, EventedReadWrite, Pty};
use polling::{Event as PollEvent, PollMode, Poller};

use crate::protocol::{End, Size, Spawn, ToHost, ToTed};
use crate::replay;

/// Runs ted as the host: `ted --term-host <socket>`.
pub const FLAG: &str = "--term-host";

/// How long a new host waits for its first ted before giving up.
const FIRST_CLIENT_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs a host on `socket` until it has no terminals and no ted connected: `ted
/// --term-host`. Returns early only if it can't listen (another host already does).
pub fn run(socket: &Path) -> io::Result<()> {
    let listener = bind(socket)?;
    let host = Host::new(Some(socket.to_path_buf()));
    let watchdog = host.clone();
    std::thread::spawn(move || {
        std::thread::sleep(FIRST_CLIENT_TIMEOUT);
        watchdog.exit_if_idle();
    });
    host.serve(listener);
    Ok(())
}

/// A new connection to the host running on threads of this process, which serves until
/// the process exits. It is started first if `start`.
pub fn connect_in_process(start: bool) -> io::Result<UnixStream> {
    static HOST: OnceLock<Arc<Host>> = OnceLock::new();
    let host = if start {
        HOST.get_or_init(|| Host::new(None))
    } else {
        HOST.get().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no terminal host"))?
    };
    let (ours, theirs) = UnixStream::pair()?;
    host.accept(theirs);
    Ok(ours)
}

/// Listens on `socket`, replacing one left by a host that died.
fn bind(socket: &Path) -> io::Result<UnixListener> {
    match UnixListener::bind(socket) {
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
            if UnixStream::connect(socket).is_ok() {
                return Err(e);
            }
            fs::remove_file(socket)?;
            UnixListener::bind(socket)
        }
        result => result,
    }
}

struct Host {
    terminals: Mutex<HashMap<u64, Terminal>>,
    clients: Mutex<usize>,
    /// The socket to remove on exiting once idle; `None` for a host that serves until its
    /// process exits.
    socket: Option<PathBuf>,
}

struct Terminal {
    term: Arc<FairMutex<Term<Listener>>>,
    sender: EventLoopSender,
    /// Where output goes as it is read.
    attached: Attached,
}

/// The ted a terminal is attached to, if any.
type Attached = Arc<Mutex<Option<Client>>>;

/// A connected ted: frames queued here are written to it in order.
#[derive(Clone)]
struct Client {
    id: u64,
    frames: mpsc::Sender<Vec<u8>>,
}

impl Client {
    fn send(&self, message: ToTed) {
        let _ = self.frames.send(message.encode());
    }
}

impl Host {
    fn new(socket: Option<PathBuf>) -> Arc<Host> {
        Arc::new(Host { terminals: Mutex::default(), clients: Mutex::new(0), socket })
    }

    fn serve(self: Arc<Self>, listener: UnixListener) {
        for stream in listener.incoming().flatten() {
            self.accept(stream);
        }
    }

    /// Serves a newly connected ted on its own thread.
    fn accept(self: &Arc<Self>, stream: UnixStream) {
        static CLIENTS: AtomicU64 = AtomicU64::new(0);
        *self.clients.lock().expect("clients") += 1;
        let id = CLIENTS.fetch_add(1, Ordering::Relaxed);
        let host = self.clone();
        std::thread::spawn(move || host.talk(id, stream));
    }

    /// Serves one ted until it disconnects.
    fn talk(self: Arc<Self>, id: u64, stream: UnixStream) {
        let (frames, queue) = mpsc::channel::<Vec<u8>>();
        if let Ok(mut writer) = stream.try_clone() {
            std::thread::spawn(move || {
                for frame in queue {
                    if writer.write_all(&frame).is_err() {
                        break;
                    }
                }
            });
        }
        let client = Client { id, frames };
        let mut reader = io::BufReader::new(stream);
        while let Ok(Some(message)) = ToHost::read(&mut reader) {
            self.handle(&client, message);
        }
        for terminal in self.terminals.lock().expect("terminals").values() {
            let mut attached = terminal.attached.lock().expect("attached");
            if attached.as_ref().is_some_and(|c| c.id == id) {
                *attached = None;
            }
        }
        *self.clients.lock().expect("clients") -= 1;
        self.exit_if_idle();
    }

    fn handle(self: &Arc<Self>, client: &Client, message: ToHost) {
        match message {
            ToHost::Create { id, size, spawn } => {
                if let Err(e) = self.create(client, id, size, spawn) {
                    client.send(ToTed::Ended { id, end: End::Failed(e.to_string()) });
                }
            }
            ToHost::Attach { id, size } => self.attach(client, id, size),
            ToHost::Input { id, bytes } => {
                if let Some(sender) = self.sender(id) {
                    let _ = sender.send(Msg::Input(bytes.into()));
                }
            }
            ToHost::Resize { id, size } => {
                let term =
                    self.terminals.lock().expect("terminals").get(&id).map(|t| (t.term.clone(), t.sender.clone()));
                if let Some((term, sender)) = term {
                    term.lock().resize(size);
                    let _ = sender.send(Msg::Resize(size.window_size()));
                }
            }
            ToHost::Kill { id } => {
                let terminal = self.terminals.lock().expect("terminals").remove(&id);
                if let Some(terminal) = terminal {
                    let _ = terminal.sender.send(Msg::Shutdown);
                }
                self.exit_if_idle();
            }
        }
    }

    fn sender(&self, id: u64) -> Option<EventLoopSender> {
        self.terminals.lock().expect("terminals").get(&id).map(|t| t.sender.clone())
    }

    /// Starts `spawn` on terminal `id`, attached to `client`.
    fn create(self: &Arc<Self>, client: &Client, id: u64, size: Size, spawn: Spawn) -> io::Result<()> {
        if self.terminals.lock().expect("terminals").contains_key(&id) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "the terminal id is taken"));
        }
        let listener = Listener { host: self.clone(), id, writer: Arc::default(), code: Arc::default() };
        let config =
            term::Config { scrolling_history: spawn.scrollback as usize, kitty_keyboard: true, ..Default::default() };
        let term = Arc::new(FairMutex::new(Term::new(config, &size, listener.clone())));
        let options = tty::Options {
            shell: Some(tty::Shell::new(spawn.program, spawn.args)),
            working_directory: Some(spawn.dir.into()),
            drain_on_exit: true,
            env: spawn.env.into_iter().collect(),
        };
        let pty = tty::new(&options, size.window_size(), id)?;
        let attached: Attached = Arc::new(Mutex::new(Some(client.clone())));
        let output = Tee { file: pty.file().try_clone()?, attached: attached.clone(), id };
        let event_loop = EventLoop::new(term.clone(), listener.clone(), TeePty { pty, output }, true, false)?;
        let sender = event_loop.channel();
        let _ = listener.writer.set(sender.clone());
        // Registered before its program can exit.
        self.terminals.lock().expect("terminals").insert(id, Terminal { term, sender, attached });
        event_loop.spawn();
        Ok(())
    }

    /// Attaches terminal `id` to `client` at `size`: sends it a snapshot, then output.
    fn attach(&self, client: &Client, id: u64, size: Size) {
        let terminal = self
            .terminals
            .lock()
            .expect("terminals")
            .get(&id)
            .map(|t| (t.term.clone(), t.sender.clone(), t.attached.clone()));
        let Some((term, sender, attached)) = terminal else {
            return client.send(ToTed::Ended { id, end: End::Missing });
        };
        // The reader holds the lease from reading output until it is on the screen, so with
        // it no output is both in the snapshot and sent after it, or in neither.
        let _lease = term.lease();
        let mut term = term.lock_unfair();
        term.resize(size);
        let _ = sender.send(Msg::Resize(size.window_size()));
        client.send(ToTed::Snapshot { id, bytes: replay::snapshot(&term) });
        let previous = attached.lock().expect("attached").replace(client.clone());
        if let Some(previous) = previous.filter(|previous| previous.id != client.id) {
            previous.send(ToTed::Ended { id, end: End::Detached });
        }
    }

    /// Terminal `id`'s program exited.
    fn exited(&self, id: u64, code: Option<i32>) {
        let terminal = self.terminals.lock().expect("terminals").remove(&id);
        if let Some(client) = terminal.and_then(|t| t.attached.lock().expect("attached").clone()) {
            client.send(ToTed::Ended { id, end: End::Exited(code) });
        }
        self.exit_if_idle();
    }

    fn exit_if_idle(&self) {
        let Some(socket) = &self.socket else { return };
        let terminals = self.terminals.lock().expect("terminals");
        if terminals.is_empty() && *self.clients.lock().expect("clients") == 0 {
            let _ = fs::remove_file(socket);
            std::process::exit(0);
        }
    }
}

/// Receives a host terminal's emulator events.
#[derive(Clone)]
struct Listener {
    host: Arc<Host>,
    id: u64,
    /// Writes answers to programs' queries straight back to them.
    writer: Arc<OnceLock<EventLoopSender>>,
    /// The program's exit code, once it exited with one.
    code: Arc<Mutex<Option<i32>>>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        match event {
            Event::PtyWrite(text) => {
                if let Some(writer) = self.writer.get() {
                    let _ = writer.send(Msg::Input(text.into_bytes().into()));
                }
            }
            Event::ChildExit(status) => *self.code.lock().expect("code") = status.code(),
            // Sent once the program is gone, with or without an exit code.
            Event::Exit => {
                let code = *self.code.lock().expect("code");
                self.host.exited(self.id, code);
            }
            // The attached ted's copy of the emulator handles the rest.
            _ => {}
        }
    }
}

/// A terminal's PTY whose output is also sent to the attached ted as it is read.
struct TeePty {
    pty: Pty,
    output: Tee,
}

/// Reads the PTY, sending what it reads to the attached ted.
struct Tee {
    file: File,
    attached: Attached,
    id: u64,
}

impl Read for Tee {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.file.read(buf)?;
        if n > 0 {
            if let Some(client) = self.attached.lock().expect("attached").as_ref() {
                client.send(ToTed::Output { id: self.id, bytes: buf[..n].to_vec() });
            }
        }
        Ok(n)
    }
}

impl EventedReadWrite for TeePty {
    type Reader = Tee;
    type Writer = File;

    unsafe fn register(&mut self, poll: &Arc<Poller>, event: PollEvent, mode: PollMode) -> io::Result<()> {
        // SAFETY: the PTY is registered as long as it is alive, as for the plain PTY.
        unsafe { self.pty.register(poll, event, mode) }
    }

    fn reregister(&mut self, poll: &Arc<Poller>, event: PollEvent, mode: PollMode) -> io::Result<()> {
        self.pty.reregister(poll, event, mode)
    }

    fn deregister(&mut self, poll: &Arc<Poller>) -> io::Result<()> {
        self.pty.deregister(poll)
    }

    fn reader(&mut self) -> &mut Tee {
        &mut self.output
    }

    fn writer(&mut self) -> &mut File {
        self.pty.writer()
    }
}

impl EventedPty for TeePty {
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        self.pty.next_child_event()
    }
}

impl OnResize for TeePty {
    fn on_resize(&mut self, window_size: WindowSize) {
        self.pty.on_resize(window_size);
    }
}
