//! Buffers: text, file association, mode, undo history and syntax state.
//!
//! Buffers are plain owned values living in the editor's `Buffers` store and are addressed
//! by `BufferId`; views refer to them by id. Nothing here locks or allocates per access.

mod decorations;
mod edits;
mod history;
mod journal;
mod search;
mod styled;

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::ops::{Index, IndexMut, Range};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use ropey::{Rope, RopeSlice};

pub use decorations::{Decoration, Decorations};
pub use edits::{map_pos, Edit};
pub use history::{EditGroup, EditKind, TreeDisplayLine, UndoNode, UndoTree};
pub use styled::StyledText;

use journal::Journal;

use crate::ext::Extensions;
use crate::keymap::KeymapId;
use crate::mode::{IndentStyle, Indentation, Mode};
use crate::project::Project;
use crate::settings;
use crate::syntax::{Parsed, Syntax, SyntaxToken};
use crate::text::directory_of;
use crate::view::BufferRenderer;

pub const SCRATCH_NAME: &str = "*scratch*";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BufferId(u32);

impl BufferId {
    /// Refers to no buffer; only used for transient placeholders.
    pub(crate) const DETACHED: BufferId = BufferId(u32::MAX);
}

pub struct Buffer {
    text: Rope,
    name: String,
    path: Option<PathBuf>,
    /// Working directory of a buffer without a file, set by whatever generated it.
    directory: Option<PathBuf>,
    /// What the mode's `restore` command needs besides the directory to bring the buffer
    /// back (which terminal it shows).
    restore_argument: Option<String>,
    mode: Arc<Mode>,
    /// The editor's indentation settings, used where the mode has none of its own (kept
    /// current by `Buffers`).
    default_indentation: Indentation,
    read_only: bool,
    history: UndoTree,
    group: EditGroup,
    /// Edits made since the last history commit.
    pending: bool,
    /// Content id of the undo node written to disk; `None` if never saved.
    saved_content: Option<u64>,
    disk_mtime: Option<SystemTime>,
    syntax: Option<Syntax>,
    /// Bumped by every text change, so observers (language servers) can tell what they saw.
    version: u64,
    /// The recent changes, to catch observers up from the version they saw.
    journal: Journal,
    decorations: Decorations,
    locals: Extensions,
    renderer: Option<Box<dyn BufferRenderer>>,
    /// Minor keymaps, consulted above the mode's keymap; the last enabled wins.
    keymaps: Vec<KeymapId>,
    margin: Option<Margin>,
    /// Where the last window to leave the buffer was, for the next one to show it.
    place: Place,
}

/// A position in a buffer that outlives the windows showing it: the cursor, mark and
/// scroll a window returns to. Edits move it along with the text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Place {
    pub pos: usize,
    pub mark: Option<usize>,
    pub top_line: usize,
}

/// Per-line annotations drawn left of the line numbers (e.g. git blame). A margin
/// describes one version of the text, so any change to the text hides it.
pub struct Margin {
    /// Width in columns.
    pub width: usize,
    /// One entry per line; lines past the end show nothing.
    pub lines: Vec<StyledText>,
    version: u64,
}

impl Buffer {
    /// A clean buffer holding `text`, in the fallback mode until `set_mode` is called.
    pub fn new(name: impl Into<String>, text: &str) -> Self {
        let rope = Rope::from_str(text);
        Self {
            history: UndoTree::new(rope.clone(), 0),
            text: rope,
            name: name.into(),
            path: None,
            directory: None,
            restore_argument: None,
            mode: Mode::fundamental(),
            default_indentation: Indentation::default(),
            read_only: false,
            group: EditGroup::default(),
            pending: false,
            saved_content: Some(0),
            disk_mtime: None,
            syntax: None,
            version: 0,
            journal: Journal::default(),
            decorations: Decorations::default(),
            locals: Extensions::default(),
            renderer: None,
            keymaps: Vec::new(),
            margin: None,
            place: Place::default(),
        }
    }

    pub fn scratch() -> Self {
        Self::new(SCRATCH_NAME, "")
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(text: &str) -> Self {
        Self::new("*unnamed*", text)
    }

    /// Loads `path`, or starts an empty buffer visiting it if the file doesn't exist yet.
    pub fn from_file(path: &Path) -> io::Result<Self> {
        let mut buf = Self::new(String::new(), "");
        if path.exists() {
            let rope = Rope::from_reader(fs::File::open(path)?)?;
            buf.history = UndoTree::new(rope.clone(), 0);
            buf.text = rope;
            buf.disk_mtime = mtime(path);
        }
        buf.set_path(path);
        Ok(buf)
    }

    // ---------------------------------------------------------------------------------
    // Identity
    // ---------------------------------------------------------------------------------

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn set_name(&mut self, name: impl Into<String>) {
        self.name = name.into();
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Associates the buffer with `path` and renames it after the file.
    pub fn set_path(&mut self, path: impl AsRef<Path>) {
        let path = path.as_ref();
        self.name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string_lossy().to_string());
        self.path = Some(path.to_path_buf());
    }

    /// The directory commands run in for this buffer: the visited directory or the visited
    /// file's, else the one its creator set (a build's, a repository's), else ted's own.
    pub fn directory(&self) -> PathBuf {
        match (&self.path, &self.directory) {
            (Some(path), _) => directory_of(Some(path)),
            (None, Some(dir)) => dir.clone(),
            (None, None) => directory_of(None),
        }
    }

    /// The project the buffer's working directory belongs to.
    pub fn project(&self) -> Project {
        Project::containing(&self.directory())
    }

    /// Sets the working directory used while the buffer visits no file.
    pub fn set_directory(&mut self, dir: impl Into<PathBuf>) {
        self.directory = Some(dir.into());
    }

    /// What the mode's `restore` command is given to bring the buffer back.
    pub fn restore_argument(&self) -> Option<&str> {
        self.restore_argument.as_deref()
    }

    pub fn set_restore_argument(&mut self, argument: Option<String>) {
        self.restore_argument = argument;
    }

    /// Where a window showing the buffer should start (see `Place`).
    pub fn place(&self) -> Place {
        self.place
    }

    pub fn set_place(&mut self, place: Place) {
        self.place = place;
    }

    pub fn is_scratch(&self) -> bool {
        self.path.is_none() && self.name == SCRATCH_NAME
    }

    pub fn mode(&self) -> &Arc<Mode> {
        &self.mode
    }

    pub fn set_mode(&mut self, mode: Arc<Mode>) {
        self.syntax = mode.grammar.and_then(|load| load()).map(Syntax::new);
        self.read_only |= mode.read_only;
        self.mode = mode;
    }

    /// Swaps in a changed definition of the current mode (same grammar), keeping the
    /// syntax state.
    pub(crate) fn update_mode(&mut self, mode: Arc<Mode>) {
        debug_assert_eq!(mode.name, self.mode.name);
        self.mode = mode;
    }

    pub fn tab_width(&self) -> usize {
        match self.mode.get(settings::TAB_WIDTH) {
            Some(width) => width.clamp(1, 16) as usize,
            None => self.default_indentation.width,
        }
    }

    pub fn indent_style(&self) -> IndentStyle {
        match self.mode.get(settings::INDENT) {
            Some(style) => IndentStyle::from_setting(style),
            None => self.default_indentation.style,
        }
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    pub fn set_read_only(&mut self, read_only: bool) {
        self.read_only = read_only;
    }

    /// Buffer-local state of type `T`, if set.
    pub fn local<T: 'static>(&self) -> Option<&T> {
        self.locals.get()
    }

    /// Buffer-local state of type `T`, created with `Default` on first use.
    pub fn local_mut<T: Default + 'static>(&mut self) -> &mut T {
        self.locals.get_mut()
    }

    /// Layers `keymap` over the mode's keymap in this buffer (a minor mode's keys).
    pub fn enable_keymap(&mut self, keymap: KeymapId) {
        if !self.keymaps.contains(&keymap) {
            self.keymaps.push(keymap);
        }
    }

    pub fn disable_keymap(&mut self, keymap: KeymapId) {
        self.keymaps.retain(|&k| k != keymap);
    }

    /// Minor keymaps, most recently enabled last.
    pub fn minor_keymaps(&self) -> &[KeymapId] {
        &self.keymaps
    }

    /// Draws this buffer with `renderer` instead of its text (or with `None`, normally).
    pub fn set_renderer(&mut self, renderer: Option<Box<dyn BufferRenderer>>) {
        self.renderer = renderer;
    }

    pub fn renderer_mut(&mut self) -> Option<&mut (dyn BufferRenderer + 'static)> {
        self.renderer.as_deref_mut()
    }

    /// Shows `lines` in a margin `width` columns wide, until the text next changes.
    pub fn set_margin(&mut self, width: usize, lines: Vec<StyledText>) {
        self.margin = Some(Margin { width, lines, version: self.version });
    }

    pub fn clear_margin(&mut self) {
        self.margin = None;
    }

    /// The margin, unless the text changed since it was set.
    pub fn margin(&self) -> Option<&Margin> {
        self.margin.as_ref().filter(|m| m.version == self.version)
    }

    pub fn has_renderer(&self) -> bool {
        self.renderer.is_some()
    }

    pub fn decorations(&self) -> &Decorations {
        &self.decorations
    }

    pub fn decorations_mut(&mut self) -> &mut Decorations {
        &mut self.decorations
    }

    /// Increases with every change to the text (edits, undo, reloads).
    pub fn version(&self) -> u64 {
        self.version
    }

    /// The changes since `version`, in order, each in the text as it was just before it:
    /// replayed on the text of `version`, they give the current text. `None` once they
    /// are no longer all remembered.
    pub fn changes_since(&self, version: u64) -> Option<impl ExactSizeIterator<Item = &Edit>> {
        self.journal.since(version, self.version)
    }

    /// Records the change `edit` made to the text.
    fn changed(&mut self, edit: Edit) {
        let edits = std::slice::from_ref(&edit);
        self.place.pos = map_pos(edits, self.place.pos);
        self.place.mark = self.place.mark.map(|mark| map_pos(edits, mark));
        self.journal.record(self.version, edit);
        self.version += 1;
    }

    pub fn is_dirty(&self) -> bool {
        self.pending || self.saved_content != Some(self.history.current_content_id())
    }

    // ---------------------------------------------------------------------------------
    // Text access
    // ---------------------------------------------------------------------------------

    pub fn text(&self) -> &Rope {
        &self.text
    }

    pub fn len_chars(&self) -> usize {
        self.text.len_chars()
    }

    pub fn len_lines(&self) -> usize {
        self.text.len_lines()
    }

    /// Line `idx` including its line terminator.
    pub fn line(&self, idx: usize) -> RopeSlice<'_> {
        self.text.line(idx.min(self.text.len_lines().saturating_sub(1)))
    }

    /// Line `idx` without its line terminator.
    pub fn line_content(&self, idx: usize) -> RopeSlice<'_> {
        let line = self.line(idx);
        line.slice(..line_content_len(line))
    }

    pub fn char_at(&self, idx: usize) -> Option<char> {
        (idx < self.text.len_chars()).then(|| self.text.char(idx))
    }

    pub fn char_to_line(&self, idx: usize) -> usize {
        self.text.char_to_line(idx.min(self.text.len_chars()))
    }

    pub fn line_to_char(&self, line: usize) -> usize {
        self.text.line_to_char(line.min(self.text.len_lines()))
    }

    pub fn char_to_point(&self, idx: usize) -> (usize, usize) {
        let line = self.char_to_line(idx);
        (line, idx.min(self.len_chars()) - self.text.line_to_char(line))
    }

    /// Char index of (`line`, `col`), with `col` clamped to the line's content.
    pub fn point_to_char(&self, line: usize, col: usize) -> usize {
        if line >= self.text.len_lines() {
            return self.text.len_chars();
        }
        self.text.line_to_char(line) + col.min(line_content_len(self.text.line(line)))
    }

    /// Char index of the end of `line`'s content (before its terminator).
    pub fn line_end(&self, line: usize) -> usize {
        self.point_to_char(line, usize::MAX)
    }

    pub fn slice_to_string(&self, range: Range<usize>) -> String {
        let len = self.len_chars();
        let (start, end) = (range.start.min(len), range.end.min(len));
        if start >= end {
            return String::new();
        }
        self.text.slice(start..end).to_string()
    }

    // ---------------------------------------------------------------------------------
    // Editing
    // ---------------------------------------------------------------------------------

    pub fn insert(&mut self, idx: usize, text: &str) {
        if text.is_empty() || self.read_only {
            return;
        }
        self.history.break_undo_chain();
        let idx = idx.min(self.text.len_chars());
        if let Some(syntax) = &mut self.syntax {
            syntax.edit_insert(&self.text, idx, text);
        }
        self.text.insert(idx, text);
        self.decorations.on_insert(idx, text.chars().count());
        self.pending = true;
        self.changed(Edit::new(idx..idx, text));
    }

    pub fn remove(&mut self, range: Range<usize>) {
        if self.read_only {
            return;
        }
        let len = self.text.len_chars();
        let (start, end) = (range.start.min(len), range.end.min(len));
        if start >= end {
            return;
        }
        self.history.break_undo_chain();
        if let Some(syntax) = &mut self.syntax {
            syntax.edit_remove(&self.text, start..end);
        }
        self.text.remove(start..end);
        self.decorations.on_remove(start..end);
        self.pending = true;
        self.changed(Edit::new(start..end, ""));
    }

    /// Replaces the whole text and starts a fresh, clean history (used by generated
    /// buffers such as dired and compilation output). Ignores read-only. New content is
    /// shown from its start.
    pub fn set_text(&mut self, text: &str) {
        self.replace_text(Rope::from_str(text));
        self.place = Place::default();
    }

    fn replace_text(&mut self, text: Rope) {
        self.changed(journal::diff(&self.text, &text));
        self.text = text;
        self.decorations.clear_all();
        if let Some(syntax) = &mut self.syntax {
            syntax.invalidate();
        }
        self.reset_history();
    }

    /// Appends program output to a generated buffer: ignores read-only and records no
    /// undo history, so the buffer stays clean. Returns the appended char range.
    pub fn append_generated(&mut self, text: &str) -> Range<usize> {
        let start = self.text.len_chars();
        if let Some(syntax) = &mut self.syntax {
            syntax.edit_insert(&self.text, start, text);
        }
        self.text.insert(start, text);
        self.changed(Edit::new(start..start, text));
        self.reset_history();
        start..self.text.len_chars()
    }

    /// Replaces the whole text with `styled`, whose faces become decoration layer `owner`
    /// (other layers are cleared). Like `set_text`, starts a clean history.
    pub fn set_styled(&mut self, owner: &str, styled: StyledText) {
        let (text, decorations) = styled.into_parts();
        self.set_text(&text);
        self.decorations.set(owner, decorations);
    }

    /// Appends `styled` like `append_generated`, adding its faces to layer `owner`.
    pub fn append_styled(&mut self, owner: &str, styled: StyledText) -> Range<usize> {
        let (text, decorations) = styled.into_parts();
        let range = self.append_generated(&text);
        let shifted = decorations
            .into_iter()
            .map(|d| Decoration::new(d.range.start + range.start..d.range.end + range.start, d.face));
        self.decorations.extend(owner, shifted);
        range
    }

    fn reset_history(&mut self) {
        self.history = UndoTree::new(self.text.clone(), 0);
        self.group = EditGroup::default();
        self.pending = false;
        self.saved_content = Some(0);
    }

    /// Replaces every smart-case occurrence of `target` (optionally within `range`) as one
    /// undo step; undoing it returns the cursor to `cursor`.
    pub fn replace_all(
        &mut self,
        target: &str,
        replacement: &str,
        range: Option<Range<usize>>,
        cursor: usize,
    ) -> usize {
        if target.is_empty() || self.read_only {
            return 0;
        }
        let len = self.len_chars();
        let range = range.map_or(0..len, |r| r.start.min(len)..r.end.min(len));
        let matches = find_in_slice(self.text.slice(range.clone()), target);
        if matches.is_empty() {
            return 0;
        }
        self.end_edit_group();
        self.snapshot(cursor);
        let target_len = target.chars().count();
        for &m in matches.iter().rev() {
            let at = range.start + m;
            self.remove(at..at + target_len);
            self.insert(at, replacement);
        }
        matches.len()
    }

    // ---------------------------------------------------------------------------------
    // Undo history
    // ---------------------------------------------------------------------------------

    fn flush_pending(&mut self, cursor: usize) {
        if self.pending {
            self.history.commit(self.text.clone(), cursor);
            self.pending = false;
        }
    }

    /// Closes the current undo step: later edits undo separately from earlier ones.
    pub fn snapshot(&mut self, cursor: usize) {
        self.history.break_undo_chain();
        if self.pending {
            self.flush_pending(cursor);
        } else {
            self.history.set_current_cursor(cursor);
        }
    }

    /// Prepares a single-char edit that may be grouped with the previous one.
    pub fn begin_grouped_edit(&mut self, kind: EditKind, cursor: usize, is_space: bool) {
        self.history.break_undo_chain();
        if !self.group.continues(kind, cursor, is_space) {
            self.end_edit_group();
            self.snapshot(cursor);
            self.group = EditGroup { kind, count: 0, expected_cursor: cursor, ended_on_space: false };
        }
    }

    pub fn advance_grouped_edit(&mut self, cursor: usize, is_space: bool) {
        self.group.count += 1;
        self.group.expected_cursor = cursor;
        self.group.ended_on_space |= is_space;
    }

    /// Commits the active edit group, if any, as its own undo step.
    pub fn end_edit_group(&mut self) {
        if self.group.kind != EditKind::None {
            let cursor = self.group.expected_cursor;
            self.flush_pending(cursor);
            self.group = EditGroup::default();
        }
    }

    pub fn break_undo_chain(&mut self) {
        self.history.break_undo_chain();
    }

    pub fn can_undo(&self) -> bool {
        self.pending || self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    pub fn undo(&mut self, cursor: usize) -> Option<usize> {
        self.end_edit_group();
        self.flush_pending(cursor);
        let state = self.history.undo();
        self.apply_history_state(state)
    }

    pub fn redo(&mut self, cursor: usize) -> Option<usize> {
        self.end_edit_group();
        self.flush_pending(cursor);
        let state = self.history.redo();
        self.apply_history_state(state)
    }

    pub fn switch_undo_branch(&mut self, delta: isize) -> Option<usize> {
        let cursor = self.history.current().cursor;
        self.flush_pending(cursor);
        let state = self.history.switch_branch(delta);
        self.apply_history_state(state)
    }

    fn apply_history_state(&mut self, state: Option<(Rope, usize)>) -> Option<usize> {
        let (rope, cursor) = state?;
        self.changed(journal::diff(&self.text, &rope));
        self.text = rope;
        self.decorations.clamp(self.text.len_chars());
        if let Some(syntax) = &mut self.syntax {
            syntax.invalidate();
        }
        Some(cursor)
    }

    pub fn history(&self) -> &UndoTree {
        &self.history
    }

    // ---------------------------------------------------------------------------------
    // Search
    // ---------------------------------------------------------------------------------

    /// First match at or after `from` (`inclusive`) or strictly after it, wrapping around.
    pub fn find_forward(&self, query: &str, from: usize, inclusive: bool) -> Option<usize> {
        search::find_nearest(&self.text, query, from, true, inclusive)
    }

    /// Last match at or before `from` (`inclusive`) or strictly before it, wrapping around.
    pub fn find_backward(&self, query: &str, from: usize, inclusive: bool) -> Option<usize> {
        search::find_nearest(&self.text, query, from, false, inclusive)
    }

    // ---------------------------------------------------------------------------------
    // Syntax
    // ---------------------------------------------------------------------------------

    /// Highlight tokens within each char range of `ranges` (each inside one line), one list
    /// per range, with columns counted from the start of its line.
    pub fn highlight(&mut self, ranges: &[Range<usize>]) -> Vec<Vec<SyntaxToken>> {
        match &mut self.syntax {
            Some(syntax) => syntax.highlight(&self.text, ranges),
            None => vec![Vec::new(); ranges.len()],
        }
    }

    /// The syntax tree as of the last parse, without reparsing.
    pub fn syntax_tree(&self) -> Option<&tree_sitter::Tree> {
        self.syntax.as_ref()?.tree()
    }

    /// The up-to-date syntax tree with its grammar and text, if the mode has a grammar.
    pub fn parsed(&mut self) -> Option<Parsed<'_>> {
        self.syntax.as_mut()?.parsed(&self.text)
    }

    // ---------------------------------------------------------------------------------
    // Disk
    // ---------------------------------------------------------------------------------

    pub fn save(&mut self) -> io::Result<()> {
        let path = self.path.clone().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "No file path set"))?;
        self.save_as(&path)
    }

    pub fn save_as(&mut self, path: &Path) -> io::Result<()> {
        self.end_edit_group();
        self.snapshot(self.history.current().cursor);
        self.text.write_to(io::BufWriter::new(fs::File::create(path)?))?;
        self.disk_mtime = mtime(path);
        self.saved_content = Some(self.history.current_content_id());
        if self.path.as_deref() != Some(path) {
            self.set_path(path);
        }
        Ok(())
    }

    pub fn reload_from_disk(&mut self) -> io::Result<()> {
        let path = self.path.clone().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "No file path set"))?;
        self.replace_text(Rope::from_reader(fs::File::open(&path)?)?);
        self.disk_mtime = mtime(&path);
        Ok(())
    }

    /// True when the visited file changed on disk since we last read or wrote it.
    pub fn is_modified_on_disk(&self) -> bool {
        let Some(path) = &self.path else {
            return false;
        };
        match (mtime(path), self.disk_mtime) {
            (Some(disk), Some(seen)) => disk != seen,
            // Created on disk after we started visiting it; directories (dired) never count.
            (Some(_), None) => !path.is_dir(),
            (None, _) => false,
        }
    }

    /// Accepts the current on-disk version as seen, without reloading it.
    pub fn acknowledge_disk_version(&mut self) {
        if let Some(path) = &self.path {
            self.disk_mtime = mtime(path);
        }
    }
}

impl std::fmt::Display for Buffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for chunk in self.text.chunks() {
            f.write_str(chunk)?;
        }
        Ok(())
    }
}

/// Smart-case matches of `query` in `hay`, as char offsets into `hay`.
pub fn find_in_slice(hay: RopeSlice, query: &str) -> Vec<usize> {
    search::find_all(hay, query, search::is_case_sensitive(query))
}

fn mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Length of `line` in chars, excluding a trailing `\n` or `\r\n`.
pub fn line_content_len(line: RopeSlice) -> usize {
    let mut len = line.len_chars();
    if len > 0 && line.char(len - 1) == '\n' {
        len -= 1;
        if len > 0 && line.char(len - 1) == '\r' {
            len -= 1;
        }
    }
    len
}

/// Owns every open buffer. Ids are never reused and iteration follows creation order.
#[derive(Default)]
pub struct Buffers {
    map: BTreeMap<BufferId, Buffer>,
    next: u32,
    indentation: Indentation,
}

impl Buffers {
    pub fn insert(&mut self, mut buffer: Buffer) -> BufferId {
        let id = BufferId(self.next);
        self.next += 1;
        buffer.default_indentation = self.indentation;
        self.map.insert(id, buffer);
        id
    }

    /// Changes the indentation of every buffer whose mode has none of its own.
    pub(crate) fn set_indentation(&mut self, indentation: Indentation) {
        self.indentation = indentation;
        for buffer in self.map.values_mut() {
            buffer.default_indentation = indentation;
        }
    }

    pub fn remove(&mut self, id: BufferId) -> Option<Buffer> {
        self.map.remove(&id)
    }

    pub fn get(&self, id: BufferId) -> Option<&Buffer> {
        self.map.get(&id)
    }

    pub fn get_mut(&mut self, id: BufferId) -> Option<&mut Buffer> {
        self.map.get_mut(&id)
    }

    pub fn contains(&self, id: BufferId) -> bool {
        self.map.contains_key(&id)
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (BufferId, &Buffer)> {
        self.map.iter().map(|(id, b)| (*id, b))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (BufferId, &mut Buffer)> {
        self.map.iter_mut().map(|(id, b)| (*id, b))
    }

    pub fn ids(&self) -> Vec<BufferId> {
        self.map.keys().copied().collect()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn find(&self, pred: impl Fn(&Buffer) -> bool) -> Option<BufferId> {
        self.iter().find(|(_, b)| pred(b)).map(|(id, _)| id)
    }

    pub fn find_path(&self, path: &Path) -> Option<BufferId> {
        self.find(|b| b.path() == Some(path))
    }
}

impl Index<BufferId> for Buffers {
    type Output = Buffer;
    fn index(&self, id: BufferId) -> &Buffer {
        &self.map[&id]
    }
}

impl IndexMut<BufferId> for Buffers {
    fn index_mut(&mut self, id: BufferId) -> &mut Buffer {
        self.map.get_mut(&id).expect("buffer id refers to a live buffer")
    }
}
