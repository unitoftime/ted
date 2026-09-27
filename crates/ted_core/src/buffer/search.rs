//! Literal text search over rope slices, returning char offsets.

use ropey::{Rope, RopeSlice};

/// Smart case: a query containing uppercase is matched case-sensitively.
pub fn is_case_sensitive(query: &str) -> bool {
    query.chars().any(char::is_uppercase)
}

/// Lowercases char-by-char, keeping a 1:1 char mapping so offsets stay valid.
fn fold_case(s: impl Iterator<Item = char>) -> String {
    s.map(|c| c.to_lowercase().next().unwrap_or(c)).collect()
}

/// Char offsets of all non-overlapping occurrences of `query` in `hay`. Linear in `hay`.
pub fn find_all(hay: RopeSlice, query: &str, case_sensitive: bool) -> Vec<usize> {
    if query.is_empty() || hay.len_chars() == 0 {
        return Vec::new();
    }
    let (text, query) = if case_sensitive {
        (hay.to_string(), query.to_string())
    } else {
        (fold_case(hay.chars()), fold_case(query.chars()))
    };
    let mut out = Vec::new();
    let (mut last_byte, mut last_char) = (0, 0);
    for (byte, _) in text.match_indices(query.as_str()) {
        last_char += text[last_byte..byte].chars().count();
        last_byte = byte;
        out.push(last_char);
    }
    out
}

/// The nearest match from `from` in one direction, wrapping around the buffer. Scans line
/// by line, so the cost is proportional to the distance to the match.
pub fn find_nearest(rope: &Rope, query: &str, from: usize, forward: bool, inclusive: bool) -> Option<usize> {
    let case_sensitive = is_case_sensitive(query);
    if query.contains('\n') {
        let all = find_all(rope.slice(..), query, case_sensitive);
        return pick(&all, from, forward, inclusive).or(if forward { all.first() } else { all.last() }.copied());
    }
    let lines = rope.len_lines();
    let from_line = rope.char_to_line(from.min(rope.len_chars()));
    let matches_on = |line: usize| {
        let start = rope.line_to_char(line);
        find_all(rope.line(line), query, case_sensitive).into_iter().map(move |m| start + m).collect::<Vec<_>>()
    };

    // The cursor's line (only matches on the right side), then onward, then wrapped.
    if let Some(m) = pick(&matches_on(from_line), from, forward, inclusive) {
        return Some(m);
    }
    let order: Box<dyn Iterator<Item = usize>> = if forward {
        Box::new((from_line + 1..lines).chain(0..=from_line))
    } else {
        Box::new((0..from_line).rev().chain((from_line..lines).rev()))
    };
    order.map(matches_on).find_map(|ms| if forward { ms.first().copied() } else { ms.last().copied() })
}

fn pick(matches: &[usize], from: usize, forward: bool, inclusive: bool) -> Option<usize> {
    if forward {
        matches.iter().find(|&&m| if inclusive { m >= from } else { m > from }).copied()
    } else {
        matches.iter().rev().find(|&&m| if inclusive { m <= from } else { m < from }).copied()
    }
}
