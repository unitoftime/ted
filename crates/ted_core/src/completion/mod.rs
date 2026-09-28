//! Completion at point: `completion-at-point` (`C-M-i`) offers what can go at point in a
//! popup under it. Typing goes on into the buffer and narrows the candidates (matched
//! like a picker's); `TAB` or `RET` inserts the selected one, and any other key closes the
//! popup and then does what it normally does.
//!
//! Candidates come from a backend chain (see `chain`): a language server, else the words
//! of the open buffers. `completion.auto` makes the popup open by itself once typing
//! pauses for `completion.delay`: `"trigger"` after a character the buffer's backend asks
//! for (`.` or `:`), `"typing"` also once the word being typed is `completion.min_chars`
//! long. It is `"off"` by default.

mod popup;
mod words;

use std::time::Duration;

use crate::buffer::{Buffer, BufferId, Edit};
use crate::chain::{self, Request};
use crate::command::CommandId;
use crate::editor::Editor;
use crate::settings::Setting;
use crate::text::is_ident_char;

pub use popup::Popup;
pub use words::WordsBackend;

/// Complete the word from `start` to `pos` (point when asked) in `buffer`.
#[derive(Debug, Clone)]
pub struct Query {
    pub buffer: BufferId,
    pub start: usize,
    pub pos: usize,
    pub trigger: Trigger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// Asked for with a key.
    Manual,
    /// Opened by itself as a word was typed.
    Typing,
    /// Opened by itself after typing this trigger character.
    Character(char),
    /// Asked again as the word grew, because the last answer was incomplete.
    Incomplete,
}

/// A candidate.
#[derive(Debug, Clone)]
pub struct Item {
    pub label: String,
    /// What typing matches against, usually the label.
    pub filter: String,
    /// A signature or type, shown dimmed after the label.
    pub detail: String,
    /// What it is (`fn`, `struct`), shown on the right.
    pub kind: &'static str,
    /// Where the inserted text starts: it replaces the text from here to point.
    pub start: usize,
    pub insert: String,
    /// Edits made along with it elsewhere, such as adding an import.
    pub extra: Vec<Edit>,
}

#[derive(Debug, Clone, Default)]
pub struct Answer {
    pub items: Vec<Item>,
    /// There are more candidates than these: ask again as the word grows.
    pub incomplete: bool,
}

/// Characters after which `completion.auto` opens completion in a buffer, set by the
/// backend completing there (a language server).
#[derive(Debug, Clone, Default)]
pub struct TriggerChars(pub Vec<char>);

impl Request for Query {
    type Answer = Answer;

    /// Still typing the same word, in the same buffer, with no other popup up.
    fn is_current(&self, ed: &Editor) -> bool {
        let modal = ed.top_modal().is_none_or(|m| m.id() == popup::ID);
        modal && ed.active_buffer_id() == self.buffer && in_word(ed, self.start)
    }

    fn is_empty(answer: &Answer) -> bool {
        answer.items.is_empty()
    }

    fn answer(self, ed: &mut Editor, answer: Answer) {
        popup::show(ed, &self, answer);
    }

    fn unanswered(self, ed: &mut Editor, errors: Vec<String>) {
        match (self.trigger, errors.first()) {
            (Trigger::Manual, Some(e)) => ed.set_status(format!("No completions ({})", e)),
            (Trigger::Manual, None) => ed.set_status("No completions"),
            // A popup still showing the last answer keeps it.
            (Trigger::Incomplete, _) => {}
            _ => popup::close(ed),
        }
    }
}

struct Settings {
    auto: Setting<String>,
    delay: Setting<i64>,
    min_chars: Setting<i64>,
    self_insert: Option<CommandId>,
}

pub(crate) fn register(ed: &mut Editor) {
    let s = &mut ed.settings;
    let settings = Settings {
        auto: s.define_choice(
            "completion.auto",
            "off",
            &["off", "trigger", "typing"],
            "When completion opens by itself: never, after trigger characters (`.`), or also while typing a word",
        ),
        delay: s.define(
            "completion.delay",
            150_i64,
            "Milliseconds typing must pause before completion opens by itself",
        ),
        min_chars: s.define("completion.min_chars", 3_i64, "How long a word gets before typing opens completion"),
        self_insert: ed.commands.id("self-insert-command"),
    };
    ed.set_ext(settings);
    ed.commands.register("completion-at-point", "Complete the word at point, in a popup", |ed, _| {
        start(ed, Trigger::Manual);
    });
    popup::register(ed);
    chain::register(ed, 0, WordsBackend);
    ed.hooks.on_post_command(auto_complete);
}

/// Asks the backends for completions of the word before point.
fn start(ed: &mut Editor, trigger: Trigger) {
    let buffer = ed.active_buffer_id();
    let pos = ed.active_view().cursor.pos;
    let start = word_start(ed.active_buffer(), pos);
    chain::ask(ed, Query { buffer, start, pos, trigger });
}

/// Whether point is still in the word that starts at `start`.
fn in_word(ed: &Editor, start: usize) -> bool {
    let pos = ed.active_view().cursor.pos;
    pos >= start && ed.active_buffer().text().slice(start..pos).chars().all(is_ident_char)
}

/// Where the identifier ending at `pos` starts.
fn word_start(buf: &Buffer, pos: usize) -> usize {
    pos - buf.text().chars_at(pos).reversed().take_while(|&c| is_ident_char(c)).count()
}

/// Post-command hook: after a typed trigger character (or word, with `"typing"`), opens
/// completion once typing pauses.
fn auto_complete(ed: &mut Editor) {
    let Some(settings) = ed.ext::<Settings>() else {
        return;
    };
    let auto = ed.settings.get(settings.auto);
    if auto == "off" || ed.has_modal() || ed.last_command().is_none_or(|c| Some(c) != settings.self_insert) {
        return;
    }
    let (buffer, pos) = (ed.active_buffer_id(), ed.active_view().cursor.pos);
    let buf = ed.active_buffer();
    let Some(typed) = pos.checked_sub(1).and_then(|p| buf.char_at(p)) else {
        return;
    };
    let is_trigger = buf.local::<TriggerChars>().is_some_and(|t| t.0.contains(&typed));
    let word_len = pos - word_start(buf, pos);
    let trigger = match auto {
        _ if is_trigger => Trigger::Character(typed),
        "typing" if word_len >= ed.settings.get(settings.min_chars).max(1) as usize => Trigger::Typing,
        _ => return,
    };
    let version = buf.version();
    let delay = Duration::from_millis(ed.settings.get(settings.delay).max(0) as u64);
    ed.after(delay, move |ed| {
        let unchanged = ed.active_buffer_id() == buffer
            && ed.active_view().cursor.pos == pos
            && ed.buffers.get(buffer).is_some_and(|b| b.version() == version);
        if unchanged && !ed.has_modal() {
            start(ed, trigger);
        }
    });
}
