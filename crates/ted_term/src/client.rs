//! ted's side of the terminal host: the connection, which starts the host when none is
//! running, and the copy of each attached terminal's emulator that the host's output
//! feeds. Everything that draws or reads a terminal works on that copy.

use std::collections::HashMap;
use std::io::{self, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{self, Term, TermMode};
use alacritty_terminal::vte::ansi::Processor;

use crate::host;
use crate::protocol::{End, Size, ToHost, ToTed, VERSION};
use crate::session::Listener;

/// How long a started host has to begin listening.
const START_TIMEOUT: Duration = Duration::from_secs(3);

/// Where terminals' programs run.
#[derive(Clone)]
pub enum Launch {
    /// In a host process, `exe --term-host <socket>`, shared by every ted: terminals outlive
    /// the ted that started them.
    Process(PathBuf),
    /// In a host on a thread of this process, shared by its editors: terminals end with it.
    Thread,
}

/// The host process's socket, in the user's private runtime directory where there is one.
/// Named by the protocol version, so hosts of different versions keep to themselves.
fn socket() -> PathBuf {
    let dir = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(runtime) => PathBuf::from(runtime).join("ted"),
        None => std::env::temp_dir().join(format!("ted-{}", std::env::var("USER").unwrap_or_default())),
    };
    let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir);
    dir.join(format!("term-host-v{}.sock", VERSION))
}

impl Launch {
    /// A new connection to the host, which is started if `start` and none is running.
    fn connect(&self, start: bool) -> io::Result<UnixStream> {
        let Launch::Process(exe) = self else { return host::connect_in_process(start) };
        let socket = socket();
        match UnixStream::connect(&socket) {
            Ok(stream) => return Ok(stream),
            Err(e) if !start => return Err(e),
            Err(_) => {}
        }
        let mut child = Command::new(exe)
            .arg(host::FLAG)
            .arg(&socket)
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // Out of ted's process group, so signals for ted (C-c in the shell that started
            // it) leave it alone.
            .process_group(0)
            .spawn()?;
        std::thread::spawn(move || child.wait());
        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            match UnixStream::connect(&socket) {
                Ok(stream) => return Ok(stream),
                Err(e) if Instant::now() >= deadline => return Err(e),
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
    }
}

pub struct Connection {
    writer: Arc<Mutex<UnixStream>>,
    mirrors: Arc<Mutex<HashMap<u64, Mirror>>>,
    alive: Arc<AtomicBool>,
}

impl Connection {
    /// Connects to the host, starting it if none is running (and `start`).
    pub fn open(launch: &Launch, start: bool) -> io::Result<Connection> {
        let stream = launch.connect(start)?;
        let reader = BufReader::new(stream.try_clone()?);
        let connection = Connection {
            writer: Arc::new(Mutex::new(stream)),
            mirrors: Arc::default(),
            alive: Arc::new(AtomicBool::new(true)),
        };
        let (writer, mirrors, alive) =
            (connection.writer.clone(), connection.mirrors.clone(), connection.alive.clone());
        std::thread::spawn(move || receive(reader, &writer, &mirrors, &alive));
        Ok(connection)
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    pub fn send(&self, message: &ToHost) {
        let _ = self.writer.lock().expect("writer").write_all(&message.encode());
    }

    /// Shows terminal `id` in `mirror` from now on. Called before creating or attaching it,
    /// so none of its output arrives unseen.
    pub fn add(&self, id: u64, mirror: Mirror) {
        self.mirrors.lock().expect("mirrors").insert(id, mirror);
    }

    pub fn remove(&self, id: u64) {
        self.mirrors.lock().expect("mirrors").remove(&id);
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.writer.lock().expect("writer").shutdown(Shutdown::Both);
    }
}

/// Reader thread: feeds each terminal's output to its mirror until the host hangs up.
fn receive(
    mut reader: BufReader<UnixStream>,
    writer: &Mutex<UnixStream>,
    mirrors: &Mutex<HashMap<u64, Mirror>>,
    alive: &AtomicBool,
) {
    while let Ok(Some(message)) = ToTed::read(&mut reader) {
        let mut mirrors = mirrors.lock().expect("mirrors");
        match message {
            ToTed::Output { id, bytes } => {
                let Some(mirror) = mirrors.get_mut(&id) else { continue };
                mirror.feed(&bytes);
                // A snapshot taken on the alternate screen lacks the normal one; now that
                // the program left it, ask again.
                if mirror.partial && !mirror.term.lock().mode().contains(TermMode::ALT_SCREEN) {
                    mirror.partial = false;
                    let _ =
                        writer.lock().expect("writer").write_all(&ToHost::Attach { id, size: mirror.size() }.encode());
                }
            }
            ToTed::Snapshot { id, bytes } => {
                if let Some(mirror) = mirrors.get_mut(&id) {
                    mirror.reset();
                    mirror.feed(&bytes);
                    mirror.partial = mirror.term.lock().mode().contains(TermMode::ALT_SCREEN);
                }
            }
            ToTed::Ended { id, end } => {
                if let Some(mirror) = mirrors.remove(&id) {
                    mirror.listener.ended(end);
                }
            }
        }
    }
    alive.store(false, Ordering::Release);
    for (_, mirror) in mirrors.lock().expect("mirrors").drain() {
        mirror.listener.ended(End::Failed("the terminal host stopped".to_string()));
    }
}

/// ted's copy of a terminal's emulator, fed the host's output.
pub struct Mirror {
    pub term: Arc<FairMutex<Term<Listener>>>,
    parser: Processor,
    config: term::Config,
    listener: Listener,
    /// Whether it was drawn from a snapshot of the alternate screen alone.
    partial: bool,
}

impl Mirror {
    pub fn new(config: term::Config, size: Size, listener: Listener) -> Mirror {
        let term = Arc::new(FairMutex::new(Term::new(config.clone(), &size, listener.clone())));
        Mirror { term, parser: Processor::new(), config, listener, partial: false }
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut *self.term.lock(), bytes);
        self.listener.send_event(Event::Wakeup);
    }

    fn size(&self) -> Size {
        let term = self.term.lock();
        Size { cols: term.columns() as u16, lines: term.screen_lines() as u16, cell_width: 0, cell_height: 0 }
    }

    /// Empties it for a snapshot to draw it again.
    fn reset(&mut self) {
        let size = self.size();
        *self.term.lock() = Term::new(self.config.clone(), &size, self.listener.clone());
        self.parser = Processor::new();
    }
}
