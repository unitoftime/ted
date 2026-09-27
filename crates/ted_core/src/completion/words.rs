//! The fallback backend: words from the open buffers of the same mode, nearest to point
//! first, like Emacs' dabbrev. The texts are scanned on a job thread (a rope clone is
//! cheap), so a large buffer doesn't hold up typing.

use std::collections::HashMap;

use ropey::Rope;

use super::{Answer, Item, Query};
use crate::chain::{Backend, Reply};
use crate::editor::Editor;
use crate::fuzzy::fuzzy_score;
use crate::text::is_ident_char;

/// Shorter words aren't worth completing.
const MIN_WORD_CHARS: usize = 3;

pub struct WordsBackend;

impl Backend<Query> for WordsBackend {
    fn name(&self) -> &str {
        "words"
    }

    fn start(&self, ed: &mut Editor, query: &Query, reply: Reply<Query>) -> bool {
        let buf = &ed.buffers[query.buffer];
        let prefix = buf.slice_to_string(query.start..query.pos);
        if prefix.is_empty() {
            return false;
        }
        let mode = buf.mode().name.clone();
        let here = buf.text().clone();
        let others: Vec<Rope> = ed
            .buffers
            .iter()
            .filter(|(id, b)| *id != query.buffer && b.path().is_some() && b.mode().name == mode)
            .map(|(_, b)| b.text().clone())
            .collect();
        let (start, pos) = (query.start, query.pos);
        ed.spawn(move |ctx| {
            // Distance from point in this buffer; other buffers' words come after.
            let mut nearest: HashMap<String, usize> = HashMap::new();
            let mut note = |word: &str, distance: usize| match nearest.get_mut(word) {
                Some(best) => *best = (*best).min(distance),
                None => {
                    nearest.insert(word.to_string(), distance);
                }
            };
            for_each_word(&here, |at, word| {
                if at != start {
                    note(word, at.abs_diff(pos));
                }
            });
            for text in &others {
                for_each_word(text, |_, word| note(word, usize::MAX));
            }
            let mut found: Vec<(usize, String)> = nearest
                .into_iter()
                .filter(|(word, _)| *word != prefix && fuzzy_score(&prefix, word).is_some())
                .map(|(word, distance)| (distance, word))
                .collect();
            found.sort_unstable();
            let items = found
                .into_iter()
                .map(|(_, word)| Item {
                    label: word.clone(),
                    filter: word.clone(),
                    detail: String::new(),
                    kind: "",
                    start,
                    insert: word,
                    extra: Vec::new(),
                })
                .collect();
            ctx.send(move |ed| reply.send(ed, Ok(Answer { items, incomplete: false })));
        });
        true
    }
}

/// Calls `f` with each identifier of `text` long enough to complete, and the char offset
/// it starts at.
fn for_each_word(text: &Rope, mut f: impl FnMut(usize, &str)) {
    let mut word = String::new();
    let (mut start, mut len) = (0, 0);
    for (at, c) in text.chars().enumerate().chain([(text.len_chars(), ' ')]) {
        if is_ident_char(c) {
            if len == 0 {
                start = at;
            }
            word.push(c);
            len += 1;
            continue;
        }
        if len >= MIN_WORD_CHARS && !word.starts_with(|c: char| c.is_ascii_digit()) {
            f(start, &word);
        }
        word.clear();
        len = 0;
    }
}
