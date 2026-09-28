//! What ted and the terminal host say to each other over the host's socket: length-prefixed
//! frames, each a message kind and its fields. Terminals are named by ids ted picks, so no
//! request waits for a reply.

use std::io::{self, Read};

use alacritty_terminal::event::WindowSize;
use alacritty_terminal::grid::Dimensions;

/// Bumped whenever a message changes: the socket's name carries it, so a ted never talks to
/// a host that speaks another version, and each version keeps its own terminals.
pub const VERSION: u32 = 1;

/// Frames larger than this are corrupt.
const MAX_FRAME: usize = 64 << 20;

/// A terminal's size in cells, and a cell's in pixels (for programs that ask).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub cols: u16,
    pub lines: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

impl Size {
    pub fn window_size(self) -> WindowSize {
        WindowSize {
            num_lines: self.lines,
            num_cols: self.cols,
            cell_width: self.cell_width,
            cell_height: self.cell_height,
        }
    }
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.lines as usize
    }

    fn screen_lines(&self) -> usize {
        self.lines as usize
    }

    fn columns(&self) -> usize {
        self.cols as usize
    }
}

/// A program for the host to start on a new terminal.
#[derive(Debug, Clone)]
pub struct Spawn {
    pub program: String,
    pub args: Vec<String>,
    pub dir: String,
    pub env: Vec<(String, String)>,
    /// Lines of history the terminal keeps.
    pub scrollback: u32,
}

#[derive(Debug)]
pub enum ToHost {
    /// Starts `spawn` on a new terminal `id`, attached to this ted.
    Create {
        id: u64,
        size: Size,
        spawn: Spawn,
    },
    /// Attaches terminal `id` to this ted at `size` (taking it from any other ted): the host
    /// answers with a `Snapshot`, then its output.
    Attach {
        id: u64,
        size: Size,
    },
    Input {
        id: u64,
        bytes: Vec<u8>,
    },
    Resize {
        id: u64,
        size: Size,
    },
    /// Ends the terminal's program.
    Kill {
        id: u64,
    },
}

#[derive(Debug)]
pub enum ToTed {
    /// Output of the program, to feed ted's copy of the screen.
    Output { id: u64, bytes: Vec<u8> },
    /// Escape sequences that draw the terminal's screen and history, as the host has them,
    /// on an empty terminal; what follows is output.
    Snapshot { id: u64, bytes: Vec<u8> },
    /// Terminal `id` is no longer attached to this ted.
    Ended { id: u64, end: End },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum End {
    /// The program exited, with its exit code if it had one.
    Exited(Option<i32>),
    /// There is no terminal `id` (its program exited while no ted was attached, or the host
    /// restarted).
    Missing,
    /// The program could not start.
    Failed(String),
    /// Another ted attached it.
    Detached,
}

/// A frame being encoded.
struct Frame(Vec<u8>);

impl Frame {
    fn new(kind: u8, id: u64) -> Self {
        let mut frame = Frame(vec![0; 4]);
        frame.u8(kind);
        frame.u64(id);
        frame
    }

    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }

    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn bytes(&mut self, v: &[u8]) {
        self.u32(v.len() as u32);
        self.0.extend_from_slice(v);
    }

    fn str(&mut self, v: &str) {
        self.bytes(v.as_bytes());
    }

    fn size(&mut self, v: Size) {
        for n in [v.cols, v.lines, v.cell_width, v.cell_height] {
            self.u16(n);
        }
    }

    /// The frame with its length in front.
    fn finish(mut self) -> Vec<u8> {
        let len = (self.0.len() - 4) as u32;
        self.0[..4].copy_from_slice(&len.to_le_bytes());
        self.0
    }
}

/// A received frame being decoded; every read fails past its end.
struct Fields<'a>(&'a [u8]);

impl Fields<'_> {
    fn take(&mut self, n: usize) -> io::Result<&[u8]> {
        if self.0.len() < n {
            return Err(invalid("truncated frame"));
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().expect("two bytes")))
    }

    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().expect("four bytes")))
    }

    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().expect("eight bytes")))
    }

    fn bytes(&mut self) -> io::Result<Vec<u8>> {
        let len = self.u32()? as usize;
        Ok(self.take(len)?.to_vec())
    }

    fn str(&mut self) -> io::Result<String> {
        String::from_utf8(self.bytes()?).map_err(|_| invalid("text is not UTF-8"))
    }

    fn size(&mut self) -> io::Result<Size> {
        Ok(Size { cols: self.u16()?, lines: self.u16()?, cell_width: self.u16()?, cell_height: self.u16()? })
    }
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}

/// Reads one frame's body; `None` once the other side hung up.
fn read_frame(reader: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0; 4];
    match reader.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(invalid("frame too large"));
    }
    let mut body = vec![0; len];
    reader.read_exact(&mut body)?;
    Ok(Some(body))
}

impl ToHost {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            ToHost::Create { id, size, spawn } => {
                let mut f = Frame::new(0, *id);
                f.size(*size);
                f.str(&spawn.program);
                f.u32(spawn.args.len() as u32);
                for arg in &spawn.args {
                    f.str(arg);
                }
                f.str(&spawn.dir);
                f.u32(spawn.env.len() as u32);
                for (key, value) in &spawn.env {
                    f.str(key);
                    f.str(value);
                }
                f.u32(spawn.scrollback);
                f.finish()
            }
            ToHost::Attach { id, size } => {
                let mut f = Frame::new(1, *id);
                f.size(*size);
                f.finish()
            }
            ToHost::Input { id, bytes } => {
                let mut f = Frame::new(2, *id);
                f.bytes(bytes);
                f.finish()
            }
            ToHost::Resize { id, size } => {
                let mut f = Frame::new(3, *id);
                f.size(*size);
                f.finish()
            }
            ToHost::Kill { id } => Frame::new(4, *id).finish(),
        }
    }

    pub fn read(reader: &mut impl Read) -> io::Result<Option<ToHost>> {
        let Some(body) = read_frame(reader)? else { return Ok(None) };
        let mut f = Fields(&body);
        let (kind, id) = (f.u8()?, f.u64()?);
        let message = match kind {
            0 => {
                let size = f.size()?;
                let program = f.str()?;
                let args = (0..f.u32()?).map(|_| f.str()).collect::<io::Result<_>>()?;
                let dir = f.str()?;
                let env = (0..f.u32()?).map(|_| Ok((f.str()?, f.str()?))).collect::<io::Result<_>>()?;
                let scrollback = f.u32()?;
                ToHost::Create { id, size, spawn: Spawn { program, args, dir, env, scrollback } }
            }
            1 => ToHost::Attach { id, size: f.size()? },
            2 => ToHost::Input { id, bytes: f.bytes()? },
            3 => ToHost::Resize { id, size: f.size()? },
            4 => ToHost::Kill { id },
            _ => return Err(invalid("unknown message")),
        };
        Ok(Some(message))
    }
}

impl ToTed {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            ToTed::Output { id, bytes } => {
                let mut f = Frame::new(0, *id);
                f.bytes(bytes);
                f.finish()
            }
            ToTed::Snapshot { id, bytes } => {
                let mut f = Frame::new(1, *id);
                f.bytes(bytes);
                f.finish()
            }
            ToTed::Ended { id, end } => {
                let mut f = Frame::new(2, *id);
                match end {
                    End::Exited(code) => {
                        f.u8(0);
                        f.u8(code.is_some() as u8);
                        f.u32(code.unwrap_or(0) as u32);
                    }
                    End::Missing => f.u8(1),
                    End::Failed(reason) => {
                        f.u8(2);
                        f.str(reason);
                    }
                    End::Detached => f.u8(3),
                }
                f.finish()
            }
        }
    }

    pub fn read(reader: &mut impl Read) -> io::Result<Option<ToTed>> {
        let Some(body) = read_frame(reader)? else { return Ok(None) };
        let mut f = Fields(&body);
        let (kind, id) = (f.u8()?, f.u64()?);
        let message = match kind {
            0 => ToTed::Output { id, bytes: f.bytes()? },
            1 => ToTed::Snapshot { id, bytes: f.bytes()? },
            2 => {
                let end = match f.u8()? {
                    0 => {
                        let has_code = f.u8()? != 0;
                        let code = f.u32()? as i32;
                        End::Exited(has_code.then_some(code))
                    }
                    1 => End::Missing,
                    2 => End::Failed(f.str()?),
                    3 => End::Detached,
                    _ => return Err(invalid("unknown end")),
                };
                ToTed::Ended { id, end }
            }
            _ => return Err(invalid("unknown message")),
        };
        Ok(Some(message))
    }
}
