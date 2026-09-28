//! Sessions: a workspace's buffers, windows and saved window layouts as plain data that
//! outlives the process. Named workspaces are saved as sessions (see `workspace`), and
//! `reload-ted` writes every loaded workspace to one file and has the frontend start ted
//! again from its binary on it: how a rebuilt ted picks up where the old one left off.
//!
//! File buffers are visited again. Generated buffers whose mode names a `restore` command
//! (dired, git status, terminals) are recreated by running it from their working
//! directory, with their restore argument if they have one (the terminal to reattach);
//! other generated buffers are left out, and windows that showed them show `*scratch*`. The scratch text belongs to no workspace: only a restart carries it over.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::buffer::{Buffer, BufferId, Buffers};
use crate::command::Arg;
use crate::commands::files::save_some_buffers;
use crate::editor::Editor;
use crate::layout::{Layout, SplitType, Tile};
use crate::settings;
use crate::view::{Cursor, View};
use crate::workspace::{self, WindowsRef, Workspace};

/// First line of a session file. Another format (from another build) is refused whole.
const HEADER: &str = "ted-session 2";

/// `reload-ted`: once every modified file is saved or knowingly left unsaved, writes the
/// session and stops the editor with `restart` set, for the frontend to start ted on it.
pub fn request_restart(ed: &mut Editor) {
    save_some_buffers(ed, |ed| {
        let path = handoff_path();
        match write_handoff(ed, &path) {
            Ok(()) => {
                ed.restart = Some(path);
                ed.quit();
            }
            Err(e) => ed.set_status(format!("Cannot save the session to {}: {}", path.display(), e)),
        }
    });
}

/// Writes the scratch text and every loaded workspace, the active one first, to `path`.
pub fn write_handoff(ed: &Editor, path: &Path) -> io::Result<()> {
    let scratch = ed.buffers.find(Buffer::is_scratch).map(|id| ed.buffers[id].text().to_string());
    let sessions: Vec<Session> = ed
        .workspaces
        .with_windows((&ed.layout, &ed.saved_layouts))
        .map(|(ws, windows)| Session::capture(ws, windows, &ed.buffers))
        .collect();
    fs::write(path, to_text(scratch.as_deref(), &sessions))
}

/// Restores the workspaces a restarting ted left at `path`, then deletes the file.
pub fn resume(ed: &mut Editor, path: &Path) {
    let text = fs::read_to_string(path);
    let _ = fs::remove_file(path);
    let Some((scratch, sessions)) = text.ok().as_deref().and_then(parse) else {
        return ed.set_status("Cannot restore the session");
    };
    // The active workspace, first, is restored last so it ends up active.
    for session in sessions.into_iter().rev() {
        workspace::open(ed, session.name.clone(), session.root.clone());
        session.restore(ed);
    }
    if let Some(text) = scratch {
        let id = ed.ensure_scratch();
        ed.buffers[id].set_text(&text);
    }
}

/// Where a restarting ted leaves its session for the process replacing it: the user's
/// private runtime directory where there is one.
fn handoff_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    dir.join(format!("ted-session-{}", std::process::id()))
}

/// One workspace, saved.
pub struct Session {
    pub name: Option<String>,
    pub root: PathBuf,
    /// Most recently shown first.
    buffers: Vec<SavedBuffer>,
    layout: SavedLayout,
    /// `save-window-layout` slots, by slot number.
    slots: Vec<(usize, SavedLayout)>,
    /// What commands remember for the workspace, by name.
    values: Vec<(String, String)>,
}

enum SavedBuffer {
    File(PathBuf),
    Scratch,
    /// A generated buffer, recreated by running `command` from `directory`, with
    /// `argument` if it has one.
    Generated {
        command: String,
        directory: PathBuf,
        argument: Option<String>,
    },
}

#[derive(Default)]
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
    pub fn capture(ws: &Workspace, (layout, slots): WindowsRef, buffers: &Buffers) -> Session {
        let mut index = HashMap::new();
        let mut saved_buffers = Vec::new();
        for &id in ws.buffers() {
            let buf = &buffers[id];
            let saved = match (&buf.mode().restore, buf.path()) {
                (Some(command), _) => SavedBuffer::Generated {
                    command: command.clone(),
                    directory: buf.directory(),
                    argument: buf.restore_argument().map(str::to_string),
                },
                (None, Some(path)) => SavedBuffer::File(path.to_path_buf()),
                (None, None) if buf.is_scratch() => SavedBuffer::Scratch,
                (None, None) => continue,
            };
            index.insert(id, saved_buffers.len());
            saved_buffers.push(saved);
        }
        let mut slots: Vec<_> =
            slots.iter().map(|(&slot, layout)| (slot, SavedLayout::capture(layout, &index))).collect();
        slots.sort_unstable_by_key(|&(slot, _)| slot);
        Session {
            name: ws.name.clone(),
            root: ws.root.clone(),
            buffers: saved_buffers,
            layout: SavedLayout::capture(layout, &index),
            slots,
            values: ws.values.iter().map(|(key, value)| (key.clone(), value.clone())).collect(),
        }
    }

    /// Opens the session's buffers into the active workspace and replaces its windows and
    /// layout slots with the session's.
    pub fn restore(self, ed: &mut Editor) {
        let scratch = ed.ensure_scratch();
        // What a generated buffer's command runs from: a buffer in its saved directory.
        let mut staging = None;
        let mut ids = Vec::with_capacity(self.buffers.len());
        for saved in &self.buffers {
            let id = match saved {
                SavedBuffer::File(path) => path.exists().then(|| ed.visit_file(path).ok()).flatten(),
                SavedBuffer::Scratch => Some(scratch),
                SavedBuffer::Generated { command, directory, argument } => {
                    let stage = *staging.get_or_insert_with(|| ed.add_buffer(Buffer::new("*restoring*", "")));
                    ed.buffers[stage].set_directory(directory);
                    ed.show_in_active_view(stage);
                    ed.execute_with(command, argument.as_deref().map_or(Arg::None, Arg::parse));
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
        let workspace = ed.workspaces.active_mut();
        workspace.adopt(&ids);
        workspace.values.extend(self.values);
        ed.set_status("Restored session");
    }

    /// The restore command and argument of each saved generated buffer that has one.
    pub fn restore_arguments(&self) -> impl Iterator<Item = (&str, &str)> {
        self.buffers.iter().filter_map(|saved| match saved {
            SavedBuffer::Generated { command, argument: Some(argument), .. } => {
                Some((command.as_str(), argument.as_str()))
            }
            _ => None,
        })
    }

    /// Writes the session alone, as a workspace's file.
    pub fn write(&self, path: &Path) -> io::Result<()> {
        fs::write(path, to_text(None, std::slice::from_ref(self)))
    }

    /// Reads a workspace's file.
    pub fn read(path: &Path) -> io::Result<Session> {
        let text = fs::read_to_string(path)?;
        match parse(&text) {
            Some((None, sessions)) if sessions.len() == 1 => Ok(sessions.into_iter().next().expect("one session")),
            _ => Err(io::Error::new(io::ErrorKind::InvalidData, "not a workspace this ted can read")),
        }
    }

    /// The workspace's header, its buffers, then each layout's header followed by its tiles.
    fn write_text(&self, out: &mut String) {
        let _ = writeln!(out, "workspace {}", escape(self.name.as_deref().unwrap_or_default()));
        let _ = writeln!(out, "root {}", escape(&self.root.to_string_lossy()));
        for (key, value) in &self.values {
            let _ = writeln!(out, "value {} {}", key, escape(value));
        }
        for buffer in &self.buffers {
            let _ = match buffer {
                SavedBuffer::File(path) => writeln!(out, "file {}", escape(&path.to_string_lossy())),
                SavedBuffer::Scratch => writeln!(out, "scratch"),
                SavedBuffer::Generated { command, directory, argument } => {
                    let _ = writeln!(out, "generated {} {}", command, escape(&directory.to_string_lossy()));
                    match argument {
                        Some(argument) => writeln!(out, "argument {}", escape(argument)),
                        None => Ok(()),
                    }
                }
            };
        }
        let _ = writeln!(out, "layout {}", self.layout.active);
        self.layout.write_tiles(out);
        for (slot, layout) in &self.slots {
            let _ = writeln!(out, "slot {} {}", slot, layout.active);
            layout.write_tiles(out);
        }
    }
}

/// One line per record: the scratch text if given, then each session.
fn to_text(scratch: Option<&str>, sessions: &[Session]) -> String {
    let mut out = format!("{HEADER}\n");
    if let Some(text) = scratch {
        let _ = writeln!(out, "scratch-text {}", escape(text));
    }
    for session in sessions {
        session.write_text(&mut out);
    }
    out
}

/// The scratch text and sessions `to_text` wrote.
fn parse(text: &str) -> Option<(Option<String>, Vec<Session>)> {
    let mut lines = text.lines();
    if lines.next()? != HEADER {
        return None;
    }
    let mut scratch = None;
    let mut sessions: Vec<Session> = Vec::new();
    // Whether tiles go to the last slot rather than the window layout.
    let mut in_slot = false;
    for line in lines {
        let (kind, rest) = line.split_once(' ').unwrap_or((line, ""));
        match kind {
            "scratch-text" => scratch = Some(unescape(rest)),
            "workspace" => {
                let name = Some(unescape(rest)).filter(|name| !name.is_empty());
                let (buffers, layout, slots, values) = (Vec::new(), SavedLayout::default(), Vec::new(), Vec::new());
                sessions.push(Session { name, root: PathBuf::new(), buffers, layout, slots, values });
                in_slot = false;
            }
            _ => {
                let session = sessions.last_mut()?;
                match kind {
                    "root" => session.root = unescape(rest).into(),
                    "value" => {
                        let (key, value) = rest.split_once(' ')?;
                        session.values.push((key.to_string(), unescape(value)));
                    }
                    "file" => session.buffers.push(SavedBuffer::File(unescape(rest).into())),
                    "scratch" => session.buffers.push(SavedBuffer::Scratch),
                    "generated" => {
                        let (command, directory) = rest.split_once(' ')?;
                        session.buffers.push(SavedBuffer::Generated {
                            command: command.to_string(),
                            directory: unescape(directory).into(),
                            argument: None,
                        });
                    }
                    "argument" => match session.buffers.last_mut()? {
                        SavedBuffer::Generated { argument, .. } => *argument = Some(unescape(rest)),
                        _ => return None,
                    },
                    "layout" => {
                        session.layout = SavedLayout { tiles: Vec::new(), active: rest.parse().ok()? };
                        in_slot = false;
                    }
                    "slot" => {
                        let (slot, active) = rest.split_once(' ')?;
                        let layout = SavedLayout { tiles: Vec::new(), active: active.parse().ok()? };
                        session.slots.push((slot.parse().ok()?, layout));
                        in_slot = true;
                    }
                    "split" | "view" => {
                        let tile = match kind {
                            "split" => parse_split(rest)?,
                            _ => Tile::Leaf(parse_view(rest)?),
                        };
                        let layout = if in_slot { &mut session.slots.last_mut()?.1 } else { &mut session.layout };
                        layout.tiles.push(tile);
                    }
                    _ => return None,
                }
            }
        }
    }
    sessions.iter().all(|s| s.root.is_absolute()).then_some((scratch, sessions))
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
