//! Fuzzy picker popup (helm-style): filters items as you type and hands the value the
//! chosen item stands for back to the caller. Items can keep arriving from a background
//! job while it is open.
//!
//! A live picker (`Picker::live`) doesn't filter: each query reruns a search on a job
//! thread, whose results stream in as they are found (project search).
//!
//! Items for places in files (`PickerItem::at`) show the line with what was found
//! highlighted, and where it is as a short `file:line` on the right: the file's name,
//! grown by directories only as far as needed to tell it from the other items' files.

use std::any::Any;
use std::marker::PhantomData;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::editor::Editor;
use crate::face::{Face, FaceId};
use crate::frame::{CursorVisual, Frame, Rect};
use crate::fuzzy::{MatchKeys, Narrowing};
use crate::jobs::{JobContext, JobHandle};
use crate::keymap::KeymapId;
use crate::locations::Location;
use crate::text::{highlight_runs, short_paths, truncate_with_ellipsis};
use crate::ui::{render_input_region, render_panel, LineInput, Modal};
use crate::view::RenderCtx;

#[derive(Debug, Clone)]
pub struct PickerItem {
    pub title: String,
    pub subtitle: String,
    /// Char ranges of `title` drawn in the `match` face, such as what a search found.
    pub highlights: Vec<Range<usize>>,
    /// The place in a file the item stands for, shown as a short `file:line` on the right
    /// instead of the subtitle.
    pub location: Option<Location>,
}

impl PickerItem {
    pub fn new(title: impl Into<String>, subtitle: impl Into<String>) -> Self {
        Self { title: title.into(), subtitle: subtitle.into(), highlights: Vec::new(), location: None }
    }

    /// An item for a line of a file: its text, with `highlights` (char ranges) over it.
    pub fn at(location: Location, line: impl Into<String>, highlights: Vec<Range<usize>>) -> Self {
        Self { title: line.into(), subtitle: String::new(), highlights, location: Some(location) }
    }

    /// What typing matches against: the title, then the subtitle or the file's name.
    fn match_keys(&self) -> MatchKeys {
        let file = self.location.as_ref().and_then(|l| l.path.file_name()).map(|n| n.to_string_lossy());
        MatchKeys::new(&self.title, file.as_deref().unwrap_or(&self.subtitle))
    }
}

type SelectFn = Box<dyn FnOnce(&mut Editor, Box<dyn Any>, usize)>;

/// How long a live picker waits after a keystroke before searching, so typing a word
/// starts one search rather than one per key.
const LIVE_PAUSE: Duration = Duration::from_millis(60);

/// Spawns a live picker's search for a query.
type StartFn = Box<dyn Fn(&Editor, String) -> JobHandle>;

/// What makes a picker live: its items come from a search rerun whenever the query changes.
struct Live {
    start: StartFn,
    /// Empties `values` (a `Vec` of the picker's value type).
    clear_values: fn(&mut dyn Any),
}

/// A picker's background search or load reports its results through this, from any thread.
pub struct Feed<T> {
    ctx: JobContext,
    id: String,
    _values: PhantomData<fn() -> T>,
}

impl<T: Send + 'static> Feed<T> {
    /// Whether the query changed or the picker closed, so the search should stop.
    pub fn is_cancelled(&self) -> bool {
        self.ctx.is_cancelled()
    }

    /// Adds entries to the picker. Returns false once the search should stop.
    pub fn send(&self, entries: Vec<(PickerItem, T)>) -> bool {
        let id = self.id.clone();
        self.ctx.send(move |ed| {
            ed.with_modal::<Picker, _>(|picker, _| {
                if picker.id == id {
                    picker.extend(entries);
                }
            });
        })
    }

    fn finish(&self) {
        let id = self.id.clone();
        self.ctx.send(move |ed| {
            ed.with_modal::<Picker, _>(|picker, _| {
                if picker.id == id {
                    picker.loading = None;
                }
            });
        });
    }
}

fn clear_values<T: 'static>(values: &mut dyn Any) {
    if let Some(values) = values.downcast_mut::<Vec<T>>() {
        values.clear();
    }
}

pub struct Picker {
    id: String,
    pub title: String,
    pub input: LineInput,
    pub items: Vec<PickerItem>,
    /// What each item stands for (a `Vec<T>` parallel to `items`), handed to `on_select`.
    values: Box<dyn Any>,
    /// The items matching the query.
    matches: Narrowing,
    pub selected: usize,
    keymap: KeymapId,
    on_select: Option<SelectFn>,
    /// The short `file:line` of each item with a location (empty for the others).
    labels: Vec<String>,
    /// The job still adding items; cancelled when the picker closes.
    loading: Option<JobHandle>,
    live: Option<Live>,
}

pub const PAGE_STEP: usize = 10;

impl Picker {
    /// `on_select` receives the index of the chosen item in `items`.
    pub fn new(
        id: &str,
        title: impl Into<String>,
        items: Vec<PickerItem>,
        on_select: impl FnOnce(&mut Editor, usize) + 'static,
    ) -> Self {
        let indices = 0..items.len();
        Self::with_values(id, title, items.into_iter().zip(indices), on_select)
    }

    /// A picker whose items each stand for a value; `on_select` receives the chosen one.
    pub fn with_values<T: 'static>(
        id: &str,
        title: impl Into<String>,
        entries: impl IntoIterator<Item = (PickerItem, T)>,
        on_select: impl FnOnce(&mut Editor, T) + 'static,
    ) -> Self {
        let (items, values): (Vec<PickerItem>, Vec<T>) = entries.into_iter().unzip();
        let matches = Narrowing::new(items.iter().map(PickerItem::match_keys).collect());
        let on_select: SelectFn = Box::new(move |ed, values, index| {
            let mut values = values.downcast::<Vec<T>>().expect("values have the picker's type");
            on_select(ed, values.swap_remove(index));
        });
        let mut picker = Self {
            id: id.to_string(),
            title: title.into(),
            input: LineInput::default(),
            items,
            values: Box::new(values),
            matches,
            selected: 0,
            keymap: KeymapId::PICKER,
            on_select: Some(on_select),
            labels: Vec::new(),
            loading: None,
            live: None,
        };
        picker.relabel();
        picker.rescan();
        picker
    }

    /// A picker whose items come from `search`, run on a job thread for each query (after
    /// a short pause) and sending results through its `Feed` as it finds them. Items show
    /// in the order they arrive, unfiltered; a new query cancels the search before it.
    pub fn live<T: Send + 'static>(
        id: &str,
        title: impl Into<String>,
        search: impl Fn(&str, &Feed<T>) + Send + Sync + 'static,
        on_select: impl FnOnce(&mut Editor, T) + 'static,
    ) -> Self {
        let search = Arc::new(search);
        let picker_id = id.to_string();
        let start: StartFn = Box::new(move |ed: &Editor, query: String| {
            let (search, id) = (search.clone(), picker_id.clone());
            ed.spawn(move |ctx| {
                std::thread::sleep(LIVE_PAUSE);
                if ctx.is_cancelled() {
                    return;
                }
                let feed = Feed { ctx, id, _values: PhantomData };
                search(&query, &feed);
                feed.finish();
            })
        });
        let mut picker = Self::with_values(id, title, std::iter::empty::<(PickerItem, T)>(), on_select);
        picker.live = Some(Live { start, clear_values: clear_values::<T> });
        picker
    }

    /// Replaces the query, as if typed.
    pub fn set_query(&mut self, ed: &Editor, query: &str) {
        self.input.set(query);
        self.query_changed(ed);
    }

    /// Filters for the new query, or restarts a live picker's search.
    fn query_changed(&mut self, ed: &Editor) {
        let Some(live) = &self.live else {
            self.refilter();
            return;
        };
        if let Some(job) = self.loading.take() {
            job.cancel();
        }
        (live.clear_values)(&mut *self.values);
        self.items.clear();
        self.matches.clear();
        self.labels.clear();
        self.selected = 0;
        let query = self.input.text();
        if !query.is_empty() {
            self.loading = Some((live.start)(ed, query.to_string()));
        }
    }

    /// Reads keys through `keymap` instead of `picker`.
    pub fn keymap(mut self, keymap: KeymapId) -> Self {
        self.keymap = keymap;
        self
    }

    /// Runs `load` on a job thread; it adds entries (values of the type the picker was
    /// made with) through its `Feed`, in as many batches as it likes. The title shows the
    /// picker is loading until `load` returns; closing the picker first cancels the job.
    pub fn load_in_background<T: Send + 'static>(
        mut self,
        ed: &Editor,
        load: impl FnOnce(&Feed<T>) + Send + 'static,
    ) -> Self {
        let id = self.id.clone();
        self.loading = Some(ed.spawn(move |ctx| {
            let feed = Feed { ctx, id, _values: PhantomData };
            load(&feed);
            feed.finish();
        }));
        self
    }

    /// Adds entries, keeping the selected item selected. Returns false, adding nothing, if
    /// their values are not of the type the picker was made with.
    pub fn extend<T: 'static>(&mut self, entries: impl IntoIterator<Item = (PickerItem, T)>) -> bool {
        let Some(values) = self.values.downcast_mut::<Vec<T>>() else {
            return false;
        };
        for (item, value) in entries {
            self.matches.push(item.match_keys());
            self.items.push(item);
            values.push(value);
        }
        self.relabel();
        let selected = self.filtered().get(self.selected).copied();
        self.rescan();
        self.selected = selected.and_then(|s| self.filtered().iter().position(|&i| i == s)).unwrap_or(0);
        true
    }

    /// Recomputes the short `file:line` of the items with locations, since a new file can
    /// make another's name ambiguous.
    fn relabel(&mut self) {
        let paths: Vec<&Path> = self.items.iter().filter_map(|it| Some(it.location.as_ref()?.path.as_path())).collect();
        if paths.is_empty() {
            self.labels.clear();
            return;
        }
        let mut short = short_paths(&paths).into_iter();
        self.labels = self
            .items
            .iter()
            .map(|it| match (&it.location, short.next()) {
                (Some(location), Some(path)) => format!("{}:{}", path, location.line + 1),
                _ => String::new(),
            })
            .collect();
    }

    /// Filters for the current query.
    fn refilter(&mut self) {
        self.matches.narrow(self.input.text());
        if self.selected >= self.filtered().len() {
            self.selected = 0;
        }
    }

    /// Filters every item for the current query (a live picker lists them all).
    fn rescan(&mut self) {
        match self.live {
            Some(_) => self.matches.show_all(self.input.text()),
            None => self.matches.rescan(self.input.text()),
        }
        if self.selected >= self.filtered().len() {
            self.selected = 0;
        }
    }

    /// Indices into `items` matching the query, best first.
    pub fn filtered(&self) -> &[usize] {
        self.matches.matches()
    }

    pub fn selected_item(&self) -> Option<&PickerItem> {
        self.filtered().get(self.selected).map(|&i| &self.items[i])
    }

    /// Moves the selection by `delta`, wrapping when `wrap` is set, clamping otherwise.
    pub fn move_selection(&mut self, delta: isize, wrap: bool) {
        let n = self.filtered().len();
        if n == 0 {
            return;
        }
        self.selected = if wrap {
            (self.selected as isize + delta).rem_euclid(n as isize) as usize
        } else {
            self.selected.saturating_add_signed(delta).min(n - 1)
        };
    }

    /// Consumes the picker and runs its callback with the selected item, if any.
    pub fn select(mut self, ed: &mut Editor) {
        let Some(&index) = self.filtered().get(self.selected) else {
            return;
        };
        if let Some(f) = self.on_select.take() {
            let values = std::mem::replace(&mut self.values, Box::new(()));
            f(ed, values, index);
        }
    }

    pub fn render(&self, frame: &mut Frame, cx: &RenderCtx) {
        let (faces, m) = (cx.faces, cx.metrics);
        let w = (frame.width - 60.0).clamp(320.0, 800.0);
        let h = (frame.height - 80.0).clamp(200.0, 500.0);
        let loading = match (&self.loading, &self.live) {
            (Some(_), Some(_)) => ", searching…",
            (Some(_), None) => ", loading…",
            (None, _) => "",
        };
        let title = format!("{} ({} items{})", self.title, self.filtered().len(), loading);
        let body = render_panel(frame, cx, (w, h), &title, false);
        let left = body.x + 10.0;

        let input_y = body.y + 6.0;
        let prompt = "> ";
        let input_clip = Rect::new(left, input_y, (body.w - 20.0).max(0.0), m.line_h + 4.0);
        frame.draw_text_clipped(left, input_y, prompt, faces.text(FaceId::POPUP_PROMPT), input_clip);
        let query_x = left + prompt.len() as f32 * m.char_w;
        render_input_region(frame, query_x, input_y, &self.input, cx);
        frame.draw_text_clipped(query_x, input_y, self.input.text(), faces.text(FaceId::POPUP_INPUT), input_clip);
        let cursor_x = query_x + self.input.cursor() as f32 * m.char_w;
        if cursor_x < body.x + body.w - 12.0 {
            let color = faces.fg(FaceId::POPUP_INPUT);
            frame.set_cursor(CursorVisual { x: cursor_x, y: input_y, w: 2.0, h: m.line_h, color });
        }

        let list_y = input_y + m.line_h + 8.0;
        let visible_rows = ((body.y + body.h - list_y - 4.0) / m.line_h).floor().max(0.0) as usize;
        let scroll = (self.selected + 1).saturating_sub(visible_rows);
        let max_chars = (((body.w - 20.0) / m.char_w.max(1.0)).floor() as usize).max(10);
        let title_width = (((body.w - 36.0) / (m.char_w.max(1.0) * 2.0)).floor() as usize).clamp(20, 45);

        for (row, &item_idx) in self.filtered().iter().skip(scroll).take(visible_rows).enumerate() {
            let item = &self.items[item_idx];
            let is_selected = scroll + row == self.selected;
            let row_y = list_y + row as f32 * m.line_h;
            let face = if is_selected { FaceId::POPUP_SELECTION } else { FaceId::POPUP };
            if is_selected {
                frame.fill_rect(Rect::new(body.x + 2.0, row_y, body.w - 4.0, m.line_h), faces.bg(face));
            }
            let clip = Rect::new(body.x + 6.0, row_y, (body.w - 12.0).max(0.0), m.line_h);
            let cell = |col: usize| body.x + 6.0 + col as f32 * m.char_w;
            let base = faces.get(face);
            frame.draw_text_clipped(cell(0), row_y, if is_selected { "▸ " } else { "  " }, faces.style(base), clip);

            let label = self.labels.get(item_idx).filter(|l| !l.is_empty());
            let label_chars = label.map_or(0, |l| l.chars().count() + 2);
            let title = truncate_with_ellipsis(&item.title, max_chars.saturating_sub(2 + label_chars));
            let title_chars = title.chars().count();
            draw_highlighted(frame, (cell(2), row_y), &title, &item.highlights, base, cx, clip);
            if let Some(label) = label {
                let dim = faces.style(base.merge(faces.get(FaceId::SHADOW)));
                frame.draw_text_clipped(
                    cell(max_chars.saturating_sub(label_chars - 2)),
                    row_y,
                    label.as_str(),
                    dim,
                    clip,
                );
            } else if !item.subtitle.is_empty() {
                let pad = title_width.saturating_sub(title_chars);
                let rest = format!("{} · {}", " ".repeat(pad), item.subtitle);
                let rest = truncate_with_ellipsis(&rest, max_chars.saturating_sub(2 + title_chars));
                frame.draw_text_clipped(cell(2 + title_chars), row_y, rest, faces.style(base), clip);
            }
        }
    }
}

/// Draws `text` at `(x, y)` in `base`, with its char ranges `highlights` in the `match` face.
fn draw_highlighted(
    frame: &mut Frame,
    (x, y): (f32, f32),
    text: &str,
    highlights: &[Range<usize>],
    base: Face,
    cx: &RenderCtx,
    clip: Rect,
) {
    let highlight = base.merge(cx.faces.get(FaceId::MATCH));
    let mut col = 0;
    for (run, highlighted) in highlight_runs(text, highlights) {
        let face = if highlighted { highlight } else { base };
        frame.draw_text_clipped(x + col as f32 * cx.metrics.char_w, y, run, cx.faces.style(face), clip);
        col += run.chars().count();
    }
}

impl Drop for Picker {
    fn drop(&mut self) {
        if let Some(job) = &self.loading {
            job.cancel();
        }
    }
}

impl Modal for Picker {
    fn id(&self) -> &str {
        &self.id
    }

    fn keymap(&self) -> KeymapId {
        self.keymap
    }

    fn input(&self) -> Option<&LineInput> {
        Some(&self.input)
    }

    fn input_mut(&mut self) -> Option<&mut LineInput> {
        Some(&mut self.input)
    }

    fn input_changed(&mut self, ed: &mut Editor) {
        self.query_changed(ed);
    }

    fn render_overlay(&mut self, _ed: &Editor, frame: &mut Frame, cx: &RenderCtx) {
        self.render(frame, cx);
    }
}
