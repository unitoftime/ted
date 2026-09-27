//! Incremental search, replace and live project search.

use crate::editor::Editor;
use crate::grep;
use crate::keymap::KeymapId;
use crate::locations::{self, Location};
use crate::text::collapse_tilde;
use crate::ui::{Feed, HistoryCursor, LineInput, Modal, Picker, PickerItem, Prompt};
use crate::xref;

/// Id of past searches in `Editor::input_history`; the newest is what C-s C-s repeats.
const HISTORY: &str = "search";

/// History ids of replace-string's two prompts, and of the replacements it made.
const REPLACE_FROM: &str = "replace-string";
const REPLACE_TO: &str = "replace-with";
const REPLACE_PAIRS: &str = "replace-string-pairs";
/// Separates target and replacement in a `REPLACE_PAIRS` entry (ASCII unit separator).
const PAIR_SEPARATOR: char = '\u{1f}';

/// Most hits a project search lists; a broad query stops there.
const SEARCH_LIMIT: usize = 2000;
/// Shorter queries match nearly every line, so they don't search.
const MIN_QUERY_CHARS: usize = 2;

/// Incremental search session: typing searches from where it started, C-s/C-r step
/// through matches, RET keeps the position, C-g returns to the start. Keys the search
/// doesn't use (C-n, C-v, M-x, ...) keep the position too, then do what they normally do.
/// The query joins the search history once C-s/C-r steps with it or RET ends the search.
pub struct Search {
    input: LineInput,
    history: HistoryCursor,
    start: usize,
    forward: bool,
    failing: bool,
    label: String,
}

impl Search {
    fn new(start: usize, forward: bool) -> Self {
        let mut s = Self {
            input: LineInput::default(),
            history: HistoryCursor::default(),
            start,
            forward,
            failing: false,
            label: String::new(),
        };
        s.update_label();
        s
    }

    fn update_label(&mut self) {
        let failing = if self.failing { "Failing " } else { "" };
        let direction = if self.forward { "" } else { " backward" };
        self.label = format!("{}I-search{}: ", failing, direction);
    }

    /// Searches for the input from `from` and moves point to the match.
    fn search(&mut self, ed: &mut Editor, from: usize, inclusive: bool) {
        let query = self.input.text().to_string();
        let buf = ed.active_buffer();
        let found = if self.forward {
            buf.find_forward(&query, from, inclusive)
        } else {
            buf.find_backward(&query, from, inclusive)
        };
        let mut doc = ed.doc();
        doc.view.highlight = Some(query);
        if let Some(pos) = found {
            doc.set_cursor(pos);
        }
        self.failing = found.is_none();
        self.update_label();
    }

    /// C-s / C-r: next match in `forward` direction, recalling the last search if empty.
    fn repeat(&mut self, ed: &mut Editor, forward: bool) {
        self.forward = forward;
        if self.input.text().is_empty() {
            if let Some(last) = ed.input_history.newest(HISTORY) {
                self.input.set(last);
            }
        }
        if self.input.text().is_empty() {
            self.failing = false;
            self.update_label();
            return;
        }
        let pos = ed.doc().pos();
        self.search(ed, pos, false);
        // Stepping through matches uses the query as much as RET does.
        ed.input_history.record(HISTORY, self.input.text());
    }
}

impl Modal for Search {
    fn id(&self) -> &str {
        "search"
    }

    fn keymap(&self) -> KeymapId {
        KeymapId::SEARCH
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn input(&self) -> Option<&LineInput> {
        Some(&self.input)
    }

    fn input_mut(&mut self) -> Option<&mut LineInput> {
        Some(&mut self.input)
    }

    fn uses_minibuffer(&self) -> bool {
        true
    }

    fn history(&mut self) -> Option<(&str, &mut HistoryCursor, &mut LineInput)> {
        Some((HISTORY, &mut self.history, &mut self.input))
    }

    fn input_changed(&mut self, ed: &mut Editor) {
        if self.input.text().is_empty() {
            let mut doc = ed.doc();
            doc.set_cursor(self.start);
            doc.view.highlight = None;
            self.failing = false;
            self.update_label();
            return;
        }
        self.search(ed, self.start, true);
    }

    fn cancel(self: Box<Self>, ed: &mut Editor) {
        let mut doc = ed.doc();
        doc.set_cursor(self.start);
        doc.view.highlight = None;
    }
}

pub fn register(ed: &mut Editor) {
    let c = &mut ed.commands;
    c.register("search-forward", "Incremental search forward", |ed, _| start(ed, true));
    c.register("search-backward", "Incremental search backward", |ed, _| start(ed, false));
    c.register_hidden("search-repeat-forward", "Jump to the next match", |ed, _| {
        ed.with_modal::<Search, _>(|s, ed| s.repeat(ed, true));
    });
    c.register_hidden("search-repeat-backward", "Jump to the previous match", |ed, _| {
        ed.with_modal::<Search, _>(|s, ed| s.repeat(ed, false));
    });
    c.register_hidden("search-exit", "End the search at the current match", |ed, _| exit(ed));
    c.register_hidden(
        "search-exit-and-replay",
        "End the search at the current match, then handle the key",
        |ed, arg| {
            if let Some(key) = arg.key() {
                exit(ed);
                ed.unread_key(key);
            }
        },
    );

    c.register("replace-string", "Replace every occurrence of a string in the buffer", |ed, _| replace_string(ed));

    c.register("project-search", "Search the project's files as you type (starting with the region)", |ed, _| {
        project_search(ed)
    });
}

/// Ends the search at the current match, recording its query.
fn exit(ed: &mut Editor) {
    let Some(search) = ed.take_modal::<Search>() else {
        return;
    };
    let query = Some(search.input.text().to_string())
        .filter(|q| !q.is_empty())
        .or_else(|| ed.input_history.newest(HISTORY).map(str::to_string));
    ed.active_view_mut().highlight = None;
    if let Some(query) = query {
        ed.set_status(format!("Found '{}'", query));
        ed.input_history.record(HISTORY, &query);
    }
}

fn start(ed: &mut Editor, forward: bool) {
    let search = Search::new(ed.doc().pos(), forward);
    ed.push_modal(search);
}

/// `replace-string`: reads a target, highlighting its matches as it is typed, then its
/// replacement. An empty target repeats the last replacement.
fn replace_string(ed: &mut Editor) {
    let last = ed
        .input_history
        .newest(REPLACE_PAIRS)
        .and_then(|pair| pair.split_once(PAIR_SEPARATOR))
        .map(|(from, to)| (from.to_string(), to.to_string()));
    let label = match &last {
        Some((from, to)) => format!("Replace string (default {} -> {}): ", from, to),
        None => "Replace string: ".to_string(),
    };
    let default_target = last.as_ref().map(|(from, _)| from.clone());
    set_highlight(ed, default_target.as_deref());
    let prompt = Prompt::new(REPLACE_FROM, label, move |ed, target| match last {
        _ if !target.is_empty() => read_replacement(ed, target),
        Some((from, to)) => replace(ed, &from, &to),
        None => set_highlight(ed, None),
    })
    .verbatim()
    .on_change(move |ed, text| set_highlight(ed, Some(text).filter(|t| !t.is_empty()).or(default_target.as_deref())))
    .on_cancel(|ed| set_highlight(ed, None));
    ed.push_modal(prompt);
}

fn read_replacement(ed: &mut Editor, target: String) {
    let label = format!("Replace '{}' with: ", target);
    let prompt = Prompt::new(REPLACE_TO, label, move |ed, replacement| replace(ed, &target, &replacement))
        .verbatim()
        .on_cancel(|ed| set_highlight(ed, None));
    ed.push_modal(prompt);
}

fn replace(ed: &mut Editor, target: &str, replacement: &str) {
    set_highlight(ed, None);
    if ed.active_buffer().is_read_only() {
        ed.set_status("Buffer is read-only");
        return;
    }
    let mut doc = ed.doc();
    let cursor = doc.pos();
    let count = doc.buf.replace_all(target, replacement, None, cursor);
    doc.set_cursor(cursor);
    ed.input_history.record(REPLACE_PAIRS, &format!("{}{}{}", target, PAIR_SEPARATOR, replacement));
    ed.set_status(format!("Replaced {} occurrence(s) of '{}' with '{}'", count, target, replacement));
}

/// Highlights `pattern`'s matches in the active view (`None` or empty clears it).
fn set_highlight(ed: &mut Editor, pattern: Option<&str>) {
    ed.active_view_mut().highlight = pattern.filter(|p| !p.is_empty()).map(str::to_string);
}

/// A live picker of the lines matching the query in the files of the active buffer's
/// project, as they are found. Choosing one jumps there (`M-,` comes back).
fn project_search(ed: &mut Editor) {
    let root = ed.project().root;
    let buf = ed.active_buffer();
    let region = ed.active_view().cursor.region().map(|r| buf.slice_to_string(r)).filter(|text| !text.contains('\n'));
    let title = format!("Search {}", collapse_tilde(&root));
    let search = move |query: &str, feed: &Feed<Location>| {
        if query.chars().count() < MIN_QUERY_CHARS {
            return;
        }
        grep::search(&root, query, SEARCH_LIMIT, &|| feed.is_cancelled(), &|hits| {
            let entries = hits
                .into_iter()
                .map(|hit| (PickerItem::at(hit.location.clone(), hit.text, hit.matches), hit.location))
                .collect();
            feed.send(entries);
        });
    };
    let mut picker = Picker::live("project-search", title, search, |ed, location| {
        xref::push_mark(ed);
        locations::visit(ed, &location);
    });
    if let Some(region) = region {
        picker.set_query(ed, &region);
    }
    ed.push_modal(picker);
}
