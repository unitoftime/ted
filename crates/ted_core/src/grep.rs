//! Project-wide text search on ripgrep's own engine, linked in rather than run as `rg`:
//! `ignore` walks the tree on every core, skipping what git and `.ignore` files exclude
//! (and hidden files), and `grep-searcher` scans each file with a `grep-regex` matcher.
//!
//! Queries are regexes, taken literally when they don't parse (`foo(` while typing), and
//! smart-case: all lowercase ignores case. Binary files are skipped.

use std::ops::Range;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::sinks::Lossy;
use grep_searcher::{BinaryDetection, SearcherBuilder};
use ignore::{WalkBuilder, WalkState};

use crate::locations::Location;
use crate::text::trim_highlighted;

/// Longest line text kept for a hit; minified files have lines of megabytes.
const MAX_TEXT_CHARS: usize = 300;

/// A matching line.
#[derive(Debug, Clone)]
pub struct Hit {
    /// Where the first match on the line starts.
    pub location: Location,
    /// The line, trimmed and cut to `MAX_TEXT_CHARS`.
    pub text: String,
    /// Char ranges of `text` the query matched.
    pub matches: Vec<Range<usize>>,
}

/// Searches the files under `root` for `query`, handing each file's hits to `found` from
/// the walk's threads as soon as the file is done. Stops after `limit` hits, or once
/// `stop` returns true.
pub fn search(
    root: &Path,
    query: &str,
    limit: usize,
    stop: &(dyn Fn() -> bool + Sync),
    found: &(dyn Fn(Vec<Hit>) + Sync),
) {
    let Some(matcher) = matcher(query) else { return };
    let remaining = AtomicUsize::new(limit);
    let take_one = || remaining.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1)).is_ok();

    WalkBuilder::new(root).build_parallel().run(|| {
        let matcher = matcher.clone();
        let mut searcher = SearcherBuilder::new().line_number(true).binary_detection(BinaryDetection::quit(0)).build();
        let take_one = &take_one;
        Box::new(move |entry| {
            if stop() {
                return WalkState::Quit;
            }
            let Some(entry) = entry.ok().filter(|e| e.file_type().is_some_and(|t| t.is_file())) else {
                return WalkState::Continue;
            };
            let mut hits = Vec::new();
            let mut exhausted = false;
            let sink = Lossy(|line_number, line: &str| {
                if !take_one() {
                    exhausted = true;
                    return Ok(false);
                }
                let indent = line.chars().take_while(|c| c.is_whitespace()).count();
                let ranges = match_ranges(&matcher, line, indent + MAX_TEXT_CHARS);
                let location = Location {
                    path: entry.path().to_path_buf(),
                    line: line_number.saturating_sub(1) as usize,
                    col: ranges.first().map_or(0, |r| r.start),
                };
                let (mut text, mut matches) = trim_highlighted(line, &ranges);
                if let Some((cut, _)) = text.char_indices().nth(MAX_TEXT_CHARS) {
                    text.truncate(cut);
                    matches.retain_mut(|r| {
                        r.end = r.end.min(MAX_TEXT_CHARS);
                        r.start < r.end
                    });
                }
                hits.push(Hit { location, text, matches });
                Ok(true)
            });
            // Unreadable files are skipped, as ripgrep does.
            let _ = searcher.search_path(&matcher, entry.path(), sink);
            if !hits.is_empty() {
                found(hits);
            }
            if exhausted {
                WalkState::Quit
            } else {
                WalkState::Continue
            }
        })
    });
}

/// Char ranges of `matcher`'s matches in `line`, up to those starting past char `limit`.
fn match_ranges(matcher: &RegexMatcher, line: &str, limit: usize) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let (mut byte, mut chars) = (0, 0);
    let mut char_at = |b: usize| {
        chars += line.get(byte..b)?.chars().count();
        byte = b;
        Some(chars)
    };
    let _ = matcher.find_iter(line.as_bytes(), |m| {
        let (Some(start), Some(end)) = (char_at(m.start()), char_at(m.end())) else { return false };
        if start > limit {
            return false;
        }
        ranges.push(start..end);
        true
    });
    ranges
}

/// A smart-case line matcher for `query`: as a regex, else as a literal.
fn matcher(query: &str) -> Option<RegexMatcher> {
    let mut builder = RegexMatcherBuilder::new();
    builder.case_smart(true).line_terminator(Some(b'\n'));
    builder.build(query).or_else(|_| builder.build_literals(&[query])).ok()
}
