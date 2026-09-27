//! The completion popup: the candidates matching the word before point, under it.

use super::{in_word, Answer, Item, Query, Trigger};
use crate::buffer::{BufferId, Edit};
use crate::chain;
use crate::commands::edit;
use crate::editor::Editor;
use crate::face::FaceId;
use crate::frame::{Frame, Rect};
use crate::fuzzy::{MatchKeys, Narrowing};
use crate::keymap::KeymapId;
use crate::text::{is_ident_char, truncate_with_ellipsis};
use crate::ui::{popup_rect, render_box, Modal};
use crate::view::RenderCtx;

pub(super) const ID: &str = "completion";
const MAX_ROWS: usize = 10;
const MAX_LABEL: usize = 40;
const MAX_DETAIL: usize = 40;
const MAX_KIND: usize = 10;

pub struct Popup {
    buffer: BufferId,
    /// Where the word being completed starts.
    start: usize,
    items: Vec<Item>,
    matches: Narrowing,
    selected: usize,
    /// The first match shown.
    scroll: usize,
    /// The word typed so far, which the matches are for.
    prefix: String,
    /// The backend has more candidates, so typing asks again.
    incomplete: bool,
}

impl Popup {
    fn new(query: &Query, answer: Answer) -> Self {
        let keys = answer.items.iter().map(|item| MatchKeys::new(&item.filter, "")).collect();
        Self {
            buffer: query.buffer,
            start: query.start,
            items: answer.items,
            matches: Narrowing::new(keys),
            selected: 0,
            scroll: 0,
            prefix: String::new(),
            incomplete: answer.incomplete,
        }
    }

    /// The candidates matching the word typed so far, best first.
    pub fn matches(&self) -> impl Iterator<Item = &Item> {
        self.matches.matches().iter().map(|&i| &self.items[i])
    }

    pub fn selected(&self) -> Option<&Item> {
        self.matches.matches().get(self.selected).map(|&i| &self.items[i])
    }

    fn narrow(&mut self, prefix: String) {
        self.matches.narrow(&prefix);
        self.prefix = prefix;
        self.selected = 0;
        self.scroll = 0;
    }

    fn move_selection(&mut self, delta: isize) {
        let n = self.matches.matches().len();
        if n == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(n as isize) as usize;
        self.scroll = self.scroll.min(self.selected).max((self.selected + 1).saturating_sub(MAX_ROWS));
    }
}

impl Modal for Popup {
    fn id(&self) -> &str {
        ID
    }

    fn keymap(&self) -> KeymapId {
        KeymapId::COMPLETION
    }

    fn render_overlay(&mut self, ed: &Editor, frame: &mut Frame, cx: &RenderCtx) {
        let Some(caret) = ed.active_view().caret() else {
            return;
        };
        let (faces, m) = (cx.faces, cx.metrics);
        let shown: Vec<&Item> = self.matches().skip(self.scroll).take(MAX_ROWS).collect();
        let width = |f: fn(&Item) -> &str, max| shown.iter().map(|i| f(i).chars().count().min(max)).max().unwrap_or(0);
        let label_w = width(|i| &i.label, MAX_LABEL);
        let detail_w = width(|i| &i.detail, MAX_DETAIL);
        let kind_w = width(|i| i.kind, MAX_KIND);
        let gap = |w: usize| if w > 0 { w + 2 } else { 0 };
        let cols = 1 + label_w + gap(detail_w) + gap(kind_w) + 1;

        // The labels line up with the word being completed, past the border and a column.
        let word_x = caret.x - self.prefix.chars().count() as f32 * m.char_w;
        let anchor = Rect { x: word_x - m.char_w - 1.0, ..caret };
        let size = (cols as f32 * m.char_w + 2.0, shown.len() as f32 * m.line_h + 2.0);
        let rect = popup_rect(frame, anchor, size);
        let inner = render_box(frame, cx, rect, 1.0);
        let dim = faces.get(FaceId::SHADOW);
        for (row, item) in shown.iter().enumerate() {
            let y = inner.y + row as f32 * m.line_h;
            let selected = self.scroll + row == self.selected;
            let face = if selected { FaceId::POPUP_SELECTION } else { FaceId::POPUP };
            if selected {
                frame.fill_rect(Rect::new(inner.x, y, inner.w, m.line_h), faces.bg(face));
            }
            let base = faces.get(face);
            let clip = Rect::new(inner.x, y, inner.w, m.line_h);
            let x = |col: usize| inner.x + col as f32 * m.char_w;
            let label = truncate_with_ellipsis(&item.label, MAX_LABEL);
            frame.draw_text_clipped(x(1), y, label, faces.style(base), clip);
            let detail = truncate_with_ellipsis(&item.detail, MAX_DETAIL);
            frame.draw_text_clipped(x(1 + gap(label_w)), y, detail, faces.style(base.merge(dim)), clip);
            let kind = truncate_with_ellipsis(item.kind, MAX_KIND);
            let kind_x = x(cols - 1 - kind.chars().count());
            frame.draw_text_clipped(kind_x, y, kind, faces.style(base.merge(dim)), clip);
        }
    }
}

pub(super) fn register(ed: &mut Editor) {
    let c = &mut ed.commands;
    c.register_hidden("completion-accept", "Insert the selected completion", |ed, _| {
        if let Some(popup) = ed.take_modal::<Popup>() {
            if let Some(item) = popup.selected().cloned() {
                insert(ed, popup.buffer, item);
            }
        }
    });
    c.register_hidden("completion-next", "Select the next completion", |ed, _| {
        ed.with_modal::<Popup, _>(|popup, _| popup.move_selection(1));
    });
    c.register_hidden("completion-previous", "Select the previous completion", |ed, _| {
        ed.with_modal::<Popup, _>(|popup, _| popup.move_selection(-1));
    });
    c.register_hidden(
        "completion-delete-backward-char",
        "Delete the character before point and complete again",
        |ed, _| {
            edit(ed, |d| d.backspace());
            refresh(ed);
        },
    );
    c.register_hidden("completion-key", "Type a word character, or close completion and handle the key", |ed, arg| {
        let Some(key) = arg.key() else {
            return;
        };
        match key.printable().filter(|&c| is_ident_char(c)) {
            Some(ch) => {
                edit(ed, |d| d.insert_char(ch));
                refresh(ed);
            }
            None => {
                close(ed);
                ed.unread_key(key);
            }
        }
    });
}

/// Shows `answer` to `query`, in place of the candidates of a popup already open. A manual
/// query with one match inserts it right away.
pub(super) fn show(ed: &mut Editor, query: &Query, answer: Answer) {
    let pos = ed.active_view().cursor.pos;
    let prefix = ed.buffers[query.buffer].slice_to_string(query.start..pos);
    let mut popup = Popup::new(query, answer);
    popup.narrow(prefix);
    let found = popup.matches.matches().len();
    if found == 0 {
        close(ed);
        if query.trigger == Trigger::Manual {
            ed.set_status("No completions");
        }
        return;
    }
    if found == 1 && query.trigger == Trigger::Manual {
        let item = popup.selected().cloned().expect("one match");
        insert(ed, query.buffer, item);
        return;
    }
    match ed.modal_mut::<Popup>() {
        Some(open) => *open = popup,
        None => ed.push_modal(popup),
    }
}

pub(super) fn close(ed: &mut Editor) {
    ed.take_modal::<Popup>();
}

/// After the word changed: narrows the candidates, asking again for more when the answer
/// was incomplete. Closes when point left the word (a click moved it) or nothing matches.
fn refresh(ed: &mut Editor) {
    let (buffer, pos) = (ed.active_buffer_id(), ed.active_view().cursor.pos);
    let Some(popup) = ed.modal::<Popup>() else {
        return;
    };
    let (start, incomplete) = (popup.start, popup.incomplete);
    if buffer != popup.buffer || !in_word(ed, start) {
        close(ed);
        return;
    }
    let prefix = ed.buffers[buffer].slice_to_string(start..pos);
    let empty = ed.with_modal::<Popup, _>(|popup, _| {
        popup.narrow(prefix);
        popup.matches.matches().is_empty()
    });
    if incomplete {
        chain::ask(ed, Query { buffer, start, pos, trigger: Trigger::Incomplete });
    } else if empty == Some(true) {
        close(ed);
    }
}

/// Replaces the word being completed with `item`, along with its other edits, as one undo
/// step.
fn insert(ed: &mut Editor, buffer: BufferId, item: Item) {
    let pos = ed.active_view().cursor.pos;
    let mut edits = item.extra;
    edits.push(Edit::new(item.start.min(pos)..pos, item.insert));
    if let Err(e) = ed.apply_edits(buffer, edits) {
        ed.set_status(format!("Could not complete: {}", e));
    }
}
