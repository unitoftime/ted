//! Sessions: the buffers, windows and saved window layouts the editor shows, as plain data
//! that outlives the process. `reload-ted` writes one and has the frontend start ted again
//! from its binary on it: how a rebuilt ted picks up where the old one left off.
//!
//! File buffers are visited again and `*scratch*` keeps its text. Generated buffers whose
//! mode names a `restore` command (dired, git status, terminals) are recreated by running
//! it from their working directory; other generated buffers are left out, and windows that
//! showed them show `*scratch*`.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::buffer::{Buffer, BufferId, Buffers};
use crate::commands::files::save_some_buffers;
use crate::editor::Editor;
use crate::layout::{Layout, SplitType, Tile};
use crate::settings;
use crate::view::{Cursor, View};

/// First line of a session file. Another format (from another build) is refused whole.
const HEADER: &str = "ted-session 1";

/// `reload-ted`: once every modified file is saved or knowingly left unsaved, writes the
/// session and stops the editor with `restart` set, for the frontend to start ted on it.
pub fn request_restart(ed: &mut Editor) {
    save_some_buffers(ed, |ed| {
        let path = handoff_path();
        match Session::capture(ed).write(&path) {
            Ok(()) => {
                ed.restart = Some(path);
                ed.running = false;
            }
            Err(e) => ed.set_status(format!("Cannot save the session to {}: {}", path.display(), e)),
        }
    });
}

/// Restores the session a restarting ted left at `path`, then deletes the file.
pub fn resume(ed: &mut Editor, path: &Path) {
    let session = Session::read(path);
    let _ = fs::remove_file(path);
    match session {
        Ok(session) => session.restore(ed),
        Err(e) => ed.set_status(format!("Cannot restore the session: {}", e)),
    }
}

/// Where a restarting ted leaves its session for the process replacing it: the user's
/// private runtime directory where there is one.
fn handoff_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    dir.join(format!("ted-session-{}", std::process::id()))
}

pub struct Session {
    buffers: Vec<SavedBuffer>,
    layout: SavedLayout,
    /// `save-window-layout` slots, by slot number.
    slots: Vec<(usize, SavedLayout)>,
}

enum SavedBuffer {
    File(PathBuf),
    Scratch(String),
    /// A generated buffer, recreated by running `command` from `directory`.
    Generated {
        command: String,
        directory: PathBuf,
    },
}

struct SavedLayout {
    tiles: Vec<Tile<SavedView>>,
    /// Which leaf is active, counted in pre-order.
    active: usize,
}

#[derive(Debug, Clone, Copy, Default)]
struct SavedView {
    /// Index into `Session::buffers`; `None` for a buffer the session left out.
    buffer: Option<usize>,
    pos: usize,
    mark: Option<usize>,
    top_line: usize,
    left_col: usize,
}

impl Session {
    pub fn capture(ed: &Editor) -> Session {
        let mut index = HashMap::new();
        let mut buffers = Vec::new();
        for (id, buf) in ed.buffers.iter() {
            let saved = match (&buf.mode().restore, buf.path()) {
                (Some(command), _) => SavedBuffer::Generated { command: command.clone(), directory: buf.directory() },
                (None, Some(path)) => SavedBuffer::File(path.to_path_buf()),
                (None, None) if buf.is_scratch() => SavedBuffer::Scratch(buf.text().to_string()),
                (None, None) => continue,
            };
            index.insert(id, buffers.len());
            buffers.push(saved);
        }
        let mut slots: Vec<_> =
            ed.saved_layouts.iter().map(|(&slot, layout)| (slot, SavedLayout::capture(layout, &index))).collect();
        slots.sort_unstable_by_key(|&(slot, _)| slot);
        Session { buffers, layout: SavedLayout::capture(&ed.layout, &index), slots }
    }

    /// Opens the session's buffers and replaces the windows and layout slots with its own.
    pub fn restore(self, ed: &mut Editor) {
        let scratch = ed.ensure_scratch();
        // What a generated buffer's command runs from: a buffer in its saved directory.
        let mut staging = None;
        let mut ids = Vec::with_capacity(self.buffers.len());
        for saved in &self.buffers {
            let id = match saved {
                SavedBuffer::File(path) => path.exists().then(|| ed.visit_file(path).ok()).flatten(),
                SavedBuffer::Scratch(text) => {
                    ed.buffers[scratch].set_text(text);
                    Some(scratch)
                }
                SavedBuffer::Generated { command, directory } => {
                    let stage = *staging.get_or_insert_with(|| ed.add_buffer(Buffer::new("*restoring*", "")));
                    ed.buffers[stage].set_directory(directory);
                    ed.show_in_active_view(stage);
                    ed.execute(command);
                    Some(ed.active_buffer_id()).filter(|&id| id != stage)
                }
            };
            ids.push(id.unwrap_or(scratch));
        }

        let wrap = ed.settings.get(settings::WRAP_LINES);
        if let Some(layout) = self.layout.build(&ids, scratch, &ed.buffers, wrap) {
            ed.layout.restore(&layout);
        }
        for (slot, saved) in &self.slots {
            if let Some(layout) = saved.build(&ids, scratch, &ed.buffers, wrap) {
                ed.saved_layouts.insert(*slot, layout);
            }
        }
        if let Some(stage) = staging {
            ed.kill_buffer(stage);
        }
        ed.set_status("Restored session");
    }

    pub fn write(&self, path: &Path) -> io::Result<()> {
        fs::write(path, self.to_text())
    }

    pub fn read(path: &Path) -> io::Result<Session> {
        let text = fs::read_to_string(path)?;
        Self::parse(&text).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not a session this ted can read"))
    }

    /// One line per record: the buffers, then each layout's header followed by its tiles.
    fn to_text(&self) -> String {
        let mut out = format!("{HEADER}\n");
        for buffer in &self.buffers {
            let _ = match buffer {
                SavedBuffer::File(path) => writeln!(out, "file {}", escape(&path.to_string_lossy())),
                SavedBuffer::Scratch(text) => writeln!(out, "scratch {}", escape(text)),
                SavedBuffer::Generated { command, directory } => {
                    writeln!(out, "generated {} {}", command, escape(&directory.to_string_lossy()))
                }
            };
        }
        let _ = writeln!(out, "layout {}", self.layout.active);
        self.layout.write_tiles(&mut out);
        for (slot, layout) in &self.slots {
            let _ = writeln!(out, "slot {} {}", slot, layout.active);
            layout.write_tiles(&mut out);
        }
        out
    }

    fn parse(text: &str) -> Option<Session> {
        let mut lines = text.lines();
        if lines.next()? != HEADER {
            return None;
        }
        let mut buffers = Vec::new();
        // In file order: the window layout (no slot) and the slots; tiles add to the last.
        let mut layouts: Vec<(Option<usize>, SavedLayout)> = Vec::new();
        for line in lines {
            let (kind, rest) = line.split_once(' ').unwrap_or((line, ""));
            match kind {
                "file" => buffers.push(SavedBuffer::File(unescape(rest).into())),
                "scratch" => buffers.push(SavedBuffer::Scratch(unescape(rest))),
                "generated" => {
                    let (command, directory) = rest.split_once(' ')?;
                    buffers.push(SavedBuffer::Generated {
                        command: command.to_string(),
                        directory: unescape(directory).into(),
                    });
                }
                "layout" => layouts.push((None, SavedLayout { tiles: Vec::new(), active: rest.parse().ok()? })),
                "slot" => {
                    let (slot, active) = rest.split_once(' ')?;
                    layouts.push((
                        Some(slot.parse().ok()?),
                        SavedLayout { tiles: Vec::new(), active: active.parse().ok()? },
                    ));
                }
                "split" => layouts.last_mut()?.1.tiles.push(parse_split(rest)?),
                "view" => layouts.last_mut()?.1.tiles.push(Tile::Leaf(parse_view(rest)?)),
                _ => return None,
            }
        }
        let mut slots = Vec::new();
        let mut layout = None;
        for (slot, saved) in layouts {
            match slot {
                Some(slot) => slots.push((slot, saved)),
                None => layout = Some(saved),
            }
        }
        Some(Session { buffers, layout: layout?, slots })
    }
}

impl SavedLayout {
    fn capture(layout: &Layout, index: &HashMap<BufferId, usize>) -> Self {
        let (mut leaf, mut active) = (0, 0);
        let tiles = layout
            .tiles()
            .into_iter()
            .map(|tile| match tile {
                Tile::Split(split, ratio) => Tile::Split(split, ratio),
                Tile::Leaf(view) => {
                    if view.id() == layout.active_id() {
                        active = leaf;
                    }
                    leaf += 1;
                    Tile::Leaf(SavedView::capture(view, index))
                }
            })
            .collect();
        Self { tiles, active }
    }

    /// The layout with each view on the buffer `ids` restored for it (`fallback` for one
    /// the session left out), at its saved position kept inside the buffer's text.
    fn build(&self, ids: &[BufferId], fallback: BufferId, buffers: &Buffers, wrap: bool) -> Option<Layout> {
        Layout::from_tiles(self.tiles.iter().copied(), self.active, |id, saved| {
            let buffer = saved.buffer.and_then(|i| ids.get(i).copied()).unwrap_or(fallback);
            let mut view = View::new(id, buffer, wrap);
            view.cursor = Cursor { pos: saved.pos, mark: saved.mark, ..Cursor::default() };
            view.top_line = saved.top_line;
            view.left_col = saved.left_col;
            view.clamp(buffers[buffer].len_chars(), buffers[buffer].len_lines());
            view
        })
    }

    fn write_tiles(&self, out: &mut String) {
        for tile in &self.tiles {
            let _ = match tile {
                Tile::Split(SplitType::Horizontal, ratio) => writeln!(out, "split h {}", ratio),
                Tile::Split(SplitType::Vertical, ratio) => writeln!(out, "split v {}", ratio),
                Tile::Leaf(v) => writeln!(
                    out,
                    "view {} {} {} {} {}",
                    optional(v.buffer),
                    v.pos,
                    optional(v.mark),
                    v.top_line,
                    v.left_col
                ),
            };
        }
    }
}

impl SavedView {
    fn capture(view: &View, index: &HashMap<BufferId, usize>) -> Self {
        match index.get(&view.buffer) {
            Some(&buffer) => Self {
                buffer: Some(buffer),
                pos: view.cursor.pos,
                mark: view.cursor.mark,
                top_line: view.top_line,
                left_col: view.left_col,
            },
            None => Self::default(),
        }
    }
}

fn parse_split(rest: &str) -> Option<Tile<SavedView>> {
    let (split, ratio) = rest.split_once(' ')?;
    let split = match split {
        "h" => SplitType::Horizontal,
        "v" => SplitType::Vertical,
        _ => return None,
    };
    Some(Tile::Split(split, ratio.parse().ok()?))
}

fn parse_view(rest: &str) -> Option<SavedView> {
    let mut fields = rest.split(' ');
    let mut field = || fields.next();
    Some(SavedView {
        buffer: parse_optional(field()?)?,
        pos: field()?.parse().ok()?,
        mark: parse_optional(field()?)?,
        top_line: field()?.parse().ok()?,
        left_col: field()?.parse().ok()?,
    })
}

/// An optional number as a field: `-` for none.
fn optional(n: Option<usize>) -> String {
    n.map_or_else(|| "-".to_string(), |n| n.to_string())
}

fn parse_optional(field: &str) -> Option<Option<usize>> {
    match field {
        "-" => Some(None),
        _ => field.parse().ok().map(Some),
    }
}

/// Keeps `text` on one line: backslashes and line breaks become escapes.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(c) => out.push(c),
            None => out.push('\\'),
        }
    }
    out
}
