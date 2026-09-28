//! Formatting: `format-buffer` rewrites the buffer (or the region) the way its language's
//! formatter lays it out, as one undo step. With `format_on_save`, saving formats first.
//!
//! Formatters are backends of a `Query` chain (see `chain`); a language server registers
//! one. A formatter answers with the edits that format the text, and none when it is
//! already formatted, which doesn't pass the query on.

use std::ops::Range;

use crate::buffer::{BufferId, Edit};
use crate::chain::{self, Request};
use crate::editor::Editor;
use crate::plugin::SaveToken;
use crate::settings::Setting;

/// Format the chars in `range` of `buffer`, or all of it.
#[derive(Debug, Clone)]
pub struct Query {
    pub buffer: BufferId,
    pub range: Option<Range<usize>>,
    /// The buffer version the answer's edits are for.
    version: u64,
    /// The save waiting for this, which reports nothing itself.
    save: Option<SaveToken>,
}

impl Request for Query {
    type Answer = Vec<Edit>;

    fn is_current(&self, ed: &Editor) -> bool {
        ed.buffers.get(self.buffer).is_some_and(|b| b.version() == self.version)
    }

    fn is_empty(_: &Vec<Edit>) -> bool {
        false
    }

    fn answer(self, ed: &mut Editor, edits: Vec<Edit>) {
        let unchanged = edits.is_empty();
        let result = ed.apply_edits(self.buffer, edits);
        match self.save {
            Some(save) => save.done(ed),
            None => ed.set_status(match result {
                Err(e) => format!("Could not format: {}", e),
                Ok(()) if unchanged => "Already formatted".to_string(),
                Ok(()) => "Formatted".to_string(),
            }),
        }
    }

    fn unanswered(self, ed: &mut Editor, errors: Vec<String>) {
        if let Some(save) = self.save {
            save.done(ed);
            return;
        }
        let mode = ed.buffers.get(self.buffer).map(|b| b.mode().name.clone()).unwrap_or_default();
        ed.set_status(match errors.first() {
            Some(e) => format!("Could not format: {}", e),
            None => format!("No formatter for {} buffers", mode),
        });
    }

    fn dropped(self, ed: &mut Editor) {
        if let Some(save) = self.save {
            save.done(ed);
        }
    }
}

struct Settings {
    on_save: Setting<bool>,
}

pub(crate) fn register(ed: &mut Editor) {
    let on_save = ed.settings.define("format_on_save", false, "Format buffers that have a formatter when saving");
    ed.set_ext(Settings { on_save });
    ed.commands.register(
        "format-buffer",
        "Format the buffer, or the region, with its language's formatter",
        |ed, _| {
            let buffer = ed.active_buffer_id();
            let range = ed.active_view().cursor.region();
            ask(ed, buffer, range, None);
        },
    );
    ed.hooks.on_before_save(|ed, buffer, save| {
        let on_save = ed.ext::<Settings>().is_some_and(|s| ed.settings.get_in(s.on_save, ed.buffers[buffer].mode()));
        if on_save {
            ask(ed, buffer, None, Some(save));
        } else {
            save.done(ed);
        }
    });
}

fn ask(ed: &mut Editor, buffer: BufferId, range: Option<Range<usize>>, save: Option<SaveToken>) {
    let version = ed.buffers[buffer].version();
    chain::ask(ed, Query { buffer, range, version, save });
}
