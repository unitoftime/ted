//! Dired: a directory listing to move around the file system and manage files from.
//!
//! One buffer walks the tree in place: RET enters a directory or visits a file, `q` goes
//! up with point on the directory just left (closing the listing instead of going above the
//! directory it was opened on), `.` shows dotfiles, `(` shows or hides the details columns
//! and `g` re-reads. File operations (`ops`) act on the marked entries, or on the entry at
//! point when none are.
//!
//! The listing is data: buffer-local `Dired` holds the entries in line order, and each is a
//! row (`rows`) keyed by its name, so every re-read keeps point and marks on their
//! entries. Details (`details`) cost a `stat` per entry, so a directory with more entries
//! than `dired.details_limit` lists names only until `(` asks for them.

mod details;
mod ops;

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::buffer::{Buffer, BufferId};
use crate::commands::files::anchor_directory;
use crate::editor::Editor;
use crate::face::{Face, FaceId};
use crate::frame::Color;
use crate::mode::Mode;
use crate::rows::{self, RowSpec, RowText};
use crate::settings::Setting;
use crate::text::{collapse_tilde, natural_cmp};

use details::{Cells, Details, Formatter};

pub const MODE: &str = "Dired";
const DECORATIONS: &str = "dired";

#[derive(Debug, Clone)]
struct Entry {
    name: OsString,
    /// A directory, or a symlink to one.
    is_dir: bool,
    is_link: bool,
    /// Read when the listing shows details.
    details: Option<Details>,
}

/// Whether a listing shows the details columns.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum DetailView {
    /// When the directory has at most `dired.details_limit` entries.
    #[default]
    Auto,
    /// Whatever the size of the directory: `(` asked for them here.
    Shown,
    Hidden,
}

/// Buffer-local state of a dired buffer, which visits the listed directory.
#[derive(Default)]
struct Dired {
    /// In line order: entry `i` is row `i`.
    entries: Vec<Entry>,
    show_hidden: bool,
    details: DetailView,
    detailed: bool,
    /// The directory the listing was opened on, which `q` never goes above.
    start: Option<PathBuf>,
}

/// Faces themes can restyle, and the settings listings follow.
#[derive(Clone, Copy)]
struct Config {
    directory: FaceId,
    symlink: FaceId,
    details: Setting<bool>,
    details_limit: Setting<i64>,
}

fn config(ed: &Editor) -> Config {
    *ed.ext::<Config>().expect("set when dired registers")
}

pub fn register(ed: &mut Editor) {
    let rgb = Color::rgb;
    let config = Config {
        directory: ed.faces.register("dired-directory", Face::fg(rgb(100, 180, 255)).bold()),
        symlink: ed.faces.register("dired-symlink", Face::fg(rgb(120, 200, 200))),
        details: ed.settings.define(
            "dired.details",
            true,
            "Show sizes and dates (and unusual permissions and owners) in listings (`(` toggles)",
        ),
        details_limit: ed.settings.define(
            "dired.details_limit",
            10_000,
            "Directories with more entries list names only until `(` asks for details",
        ),
    };
    ed.set_ext(config);

    let c = &mut ed.commands;
    c.register("dired", "List the active buffer's directory, with point on its file", |ed, _| {
        let buf = ed.active_buffer();
        let dir = buf.directory();
        let file = buf.path().filter(|p| p.parent() == Some(&dir)).and_then(Path::file_name).map(OsStr::to_os_string);
        if let Err(e) = open(ed, &dir, file.as_deref()) {
            ed.set_status(format!("Error listing {}: {}", collapse_tilde(&dir), e));
        }
    });
    c.register("dired-open", "Enter the directory or visit the file under point", |ed, _| {
        let Some((dir, entry)) = entry_at_point(ed) else { return };
        let target = dir.join(&entry.name);
        if entry.is_dir {
            navigate(ed, &target, None);
        } else if let Err(e) = ed.open_file(&target) {
            ed.set_status(format!("Error opening {}: {}", target.display(), e));
        }
    });
    c.register(
        "dired-up",
        "List the parent directory, with point on this one; close the listing at its start",
        |ed, _| {
            let id = ed.active_buffer_id();
            let Some(dir) = listed_dir(&ed.buffers[id]) else { return };
            let start = ed.buffers[id].local::<Dired>().and_then(|d| d.start.as_deref());
            let above_start =
                start.is_some_and(|start| dir.strip_prefix(start).map_or(true, |rest| rest.as_os_str().is_empty()));
            match dir.parent() {
                Some(parent) if !above_start => navigate(ed, parent, dir.file_name()),
                _ => ed.kill_buffer(id),
            }
        },
    );
    c.register("dired-toggle-hidden", "Show or hide dotfiles", |ed, _| {
        relist(ed, |state| {
            state.show_hidden = !state.show_hidden;
            if state.show_hidden {
                "Showing dotfiles"
            } else {
                "Hiding dotfiles"
            }
        });
    });
    c.register("dired-toggle-details", "Show or hide sizes, dates, permissions and owners", |ed, _| {
        relist(ed, |state| {
            state.details = if state.detailed { DetailView::Hidden } else { DetailView::Shown };
            if state.detailed {
                "Hiding details"
            } else {
                "Showing details"
            }
        });
    });
    c.register("dired-refresh", "Re-read the directory listing", |ed, _| {
        let id = ed.active_buffer_id();
        if refresh(ed, id) {
            let dir = listed_dir(&ed.buffers[id]).map(|d| anchor_directory(&d)).unwrap_or_default();
            ed.set_status(format!("Refreshed {}", dir));
        }
    });
    ops::register(ed);

    ed.define_mode(
        Mode::new(MODE)
            .special()
            .revert("dired-refresh")
            .restore("dired")
            .help_group("Visit", &["dired-open", "dired-up", "row-next", "row-previous", "revert-buffer"])
            .help_group("Files", &["dired-copy", "dired-rename", "dired-delete", "dired-create-directory"])
            .help_group("Marks", &["row-mark", "row-unmark", "row-unmark-all"])
            .help_group("View", &["dired-toggle-hidden", "dired-toggle-details"]),
    );
    ed.bind_all(
        "dired",
        &[
            ("RET", "dired-open"),
            ("q", "dired-up"),
            (".", "dired-toggle-hidden"),
            ("(", "dired-toggle-details"),
            ("m", "row-mark"),
            ("u", "row-unmark"),
            ("U", "row-unmark-all"),
            ("D", "dired-delete"),
            ("R", "dired-rename"),
            ("C", "dired-copy"),
            ("+", "dired-create-directory"),
        ],
    );
}

/// Opens a listing of `dir` in the active window, as `show` does, that `q` closes rather
/// than going above `dir`.
pub fn open(ed: &mut Editor, dir: &Path, focus: Option<&OsStr>) -> io::Result<BufferId> {
    let id = show(ed, dir, focus)?;
    ed.buffers[id].local_mut::<Dired>().start = Some(dir.to_path_buf());
    Ok(id)
}

/// Lists `dir` in the active window, reusing a buffer already listing it, with point on
/// the entry called `focus` (else the first entry).
fn show(ed: &mut Editor, dir: &Path, focus: Option<&OsStr>) -> io::Result<BufferId> {
    let existing = ed.buffers.find_path(dir).filter(|&id| ed.buffers[id].local::<Dired>().is_some());
    let listing = read(ed, existing, dir)?;
    let id = existing.unwrap_or_else(|| ed.new_buffer("", MODE));
    ed.show_in_active_view(id);
    install(ed, id, dir, listing);
    focus_entry(ed, focus);
    ed.set_status(format!("Directory {}", anchor_directory(dir)));
    Ok(id)
}

/// Moves the active dired buffer to `dir`. A directory another buffer already lists is
/// shown in that buffer instead, so no two buffers list the same directory.
fn navigate(ed: &mut Editor, dir: &Path, focus: Option<&OsStr>) {
    let id = ed.active_buffer_id();
    let result = match ed.buffers.find_path(dir) {
        Some(other) if other != id => show(ed, dir, focus).map(drop),
        _ => read(ed, Some(id), dir).map(|listing| {
            install(ed, id, dir, listing);
            focus_entry(ed, focus);
            ed.set_status(format!("Directory {}", anchor_directory(dir)));
        }),
    };
    if let Err(e) = result {
        ed.set_status(format!("Error listing {}: {}", collapse_tilde(dir), e));
    }
}

/// Re-reads buffer `id`'s directory, keeping marks and each view's entry at point.
/// Reports and returns false when the directory can't be read.
fn refresh(ed: &mut Editor, id: BufferId) -> bool {
    let Some(dir) = listed_dir(&ed.buffers[id]) else { return false };
    match read(ed, Some(id), &dir) {
        Ok(listing) => {
            install(ed, id, &dir, listing);
            true
        }
        Err(e) => {
            ed.set_status(format!("Error listing {}: {}", collapse_tilde(&dir), e));
            false
        }
    }
}

/// Changes how the active dired buffer lists its directory, re-reads it and reports what
/// `change` returns.
fn relist(ed: &mut Editor, change: impl FnOnce(&mut Dired) -> &'static str) {
    let id = ed.active_buffer_id();
    let buf = &mut ed.buffers[id];
    if buf.local::<Dired>().is_none() {
        return;
    }
    let message = change(buf.local_mut::<Dired>());
    if refresh(ed, id) {
        ed.set_status(message);
    }
}

/// Re-reads every dired buffer, after a file operation; buffers whose directory is gone
/// are killed.
fn refresh_all(ed: &mut Editor) {
    let listings: Vec<BufferId> =
        ed.buffers.iter().filter(|(_, b)| b.local::<Dired>().is_some()).map(|(id, _)| id).collect();
    for id in listings {
        if listed_dir(&ed.buffers[id]).is_some_and(|dir| dir.is_dir()) {
            refresh(ed, id);
        } else {
            ed.kill_buffer(id);
        }
    }
}

/// Reads `dir` as dired buffer `id` (if any) lists it: with or without dotfiles, and with
/// details unless they are hidden or the directory is over the limit. A `(` that forced
/// details applies only to the directory it was pressed in. Directories come first, then
/// files, each in natural order (`natural_cmp`).
fn read(ed: &Editor, id: Option<BufferId>, dir: &Path) -> io::Result<Dired> {
    let config = config(ed);
    let state = id.and_then(|id| Some((ed.buffers[id].local::<Dired>()?, ed.buffers[id].path())));
    let (show_hidden, details) = match state {
        Some((state, listed)) => match state.details {
            DetailView::Shown if listed != Some(dir) => (state.show_hidden, DetailView::Auto),
            details => (state.show_hidden, details),
        },
        None if ed.settings.get(config.details) => (false, DetailView::Auto),
        None => (false, DetailView::Hidden),
    };

    let found: Vec<fs::DirEntry> = fs::read_dir(dir)?
        .flatten()
        .filter(|e| show_hidden || e.file_name().as_encoded_bytes().first() != Some(&b'.'))
        .collect();
    let detailed = match details {
        DetailView::Auto => found.len() as i64 <= ed.settings.get(config.details_limit),
        DetailView::Shown => true,
        DetailView::Hidden => false,
    };
    let mut entries: Vec<Entry> = found
        .into_iter()
        .filter_map(|e| {
            let kind = e.file_type().ok()?;
            let is_link = kind.is_symlink();
            // Only symlinks cost another stat, to learn what they point at.
            let is_dir = kind.is_dir() || (is_link && e.path().is_dir());
            let details = if detailed { Details::read(&e) } else { None };
            Some(Entry { name: e.file_name(), is_dir, is_link, details })
        })
        .collect();
    entries.sort_unstable_by(|a, b| {
        b.is_dir.cmp(&a.is_dir).then_with(|| natural_cmp(a.name.as_encoded_bytes(), b.name.as_encoded_bytes()))
    });
    Ok(Dired { entries, show_hidden, details, detailed, start: None })
}

/// Makes buffer `id` list `listing` of `dir`. Re-reading the same directory keeps marks
/// and each view's point on its entry (or at the same line if the entry is gone); a new
/// directory starts views at its first entry.
fn install(ed: &mut Editor, id: BufferId, dir: &Path, mut listing: Dired) {
    let config = config(ed);
    let buf = &mut ed.buffers[id];
    let same_dir = buf.path() == Some(dir);
    buf.set_path(dir);
    let name = format!("{}/", buf.name().trim_end_matches('/'));
    buf.set_name(name);
    let state = buf.local_mut::<Dired>();
    listing.start = state.start.take();
    *state = listing;

    let text = render(dir, state, config);
    if same_dir {
        text.install(ed, id, DECORATIONS);
    } else {
        text.install_fresh(ed, id, DECORATIONS);
    }
}

fn render(dir: &Path, state: &Dired, config: Config) -> RowText {
    let mut text = RowText::new();
    let heading = anchor_directory(dir);
    let note = match state.details {
        DetailView::Auto if !state.detailed => format!("   {} entries: ( shows details", state.entries.len()),
        _ => String::new(),
    };
    text.line(&[("  ", None), (&heading, Some(FaceId::HEADING)), (&note, Some(FaceId::SHADOW))]);
    text.line(&[]);

    let columns = Columns::new(&state.entries);
    for (i, entry) in state.entries.iter().enumerate() {
        let face = if entry.is_link {
            Some(config.symlink)
        } else if entry.is_dir {
            Some(config.directory)
        } else {
            None
        };
        let target = entry.details.as_ref().and_then(Details::target).map(|t| format!(" -> {}", t.display()));
        let spec = RowSpec::new(entry.name.as_os_str()).point_at(columns.name_col);
        text.row(
            spec,
            &[
                (&columns.line(i), Some(FaceId::SHADOW)),
                (&entry.name.to_string_lossy(), face),
                (if entry.is_dir { "/" } else { "" }, face),
                (target.as_deref().unwrap_or(""), Some(FaceId::SHADOW)),
            ],
        );
    }
    text
}

/// The details columns of a listing, each as wide as its widest cell; a column no entry
/// fills (or a listing without details) takes no space.
struct Columns {
    cells: Vec<Cells>,
    widths: [usize; 4],
    /// Chars before each name.
    name_col: usize,
}

impl Columns {
    const INDENT: &str = "  ";
    const GAP: &str = "  ";

    fn new(entries: &[Entry]) -> Self {
        let mut formatter = Formatter::new();
        let cells: Vec<Cells> = match entries.iter().any(|e| e.details.is_some()) {
            true => entries
                .iter()
                .map(|e| e.details.as_ref().map_or_else(Cells::default, |d| formatter.cells(d, e.is_dir)))
                .collect(),
            false => Vec::new(),
        };
        let mut widths = [0; 4];
        for c in &cells {
            for (width, (cell, _)) in widths.iter_mut().zip(c.columns()) {
                *width = (*width).max(cell.chars().count());
            }
        }
        let name_col = Self::INDENT.len() + widths.iter().filter(|&&w| w > 0).map(|w| w + Self::GAP.len()).sum::<usize>();
        Self { cells, widths, name_col }
    }

    /// Everything before entry `i`'s name.
    fn line(&self, i: usize) -> String {
        let mut line = String::with_capacity(self.name_col);
        line.push_str(Self::INDENT);
        let Some(c) = self.cells.get(i) else { return line };
        for (&width, (cell, right)) in self.widths.iter().zip(c.columns()).filter(|(&w, _)| w > 0) {
            let pad = " ".repeat(width - cell.chars().count());
            let (before, after) = if right { (pad.as_str(), "") } else { ("", pad.as_str()) };
            line.extend([before, cell, after, Self::GAP]);
        }
        line
    }
}

fn listed_dir(buf: &Buffer) -> Option<PathBuf> {
    buf.local::<Dired>()?;
    buf.path().map(Path::to_path_buf)
}

/// The listed directory and the entry at point in the active buffer.
fn entry_at_point(ed: &Editor) -> Option<(PathBuf, Entry)> {
    let buf = ed.active_buffer();
    let entry = buf.local::<Dired>()?.entries.get(rows::at_point(ed)?)?.clone();
    Some((listed_dir(buf)?, entry))
}

/// Puts the active view's point on the entry called `name`, if listed.
fn focus_entry(ed: &mut Editor, name: Option<&OsStr>) {
    if let Some(name) = name {
        rows::focus(ed, name);
    }
}
