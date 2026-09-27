//! Bracket matching: finds the partner of the bracket at point.
//!
//! The syntax tree answers exactly when the bracket is one of its tokens: the partner is a
//! sibling under the same node, so brackets inside strings and comments never pair up
//! with code. Without a grammar, or for brackets the grammar treats as plain text, a
//! nesting-counting scan decides, bounded so a stray bracket in a huge file stays cheap.

use ropey::Rope;
use tree_sitter::Tree;

const PAIRS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];

/// How far the fallback scan looks for a partner, in chars.
const SCAN_LIMIT: usize = 20_000;

/// The bracket at `pos` (or a closing bracket just before it, where point sits after
/// typing one) and its partner, as char positions.
pub fn matching_pair(text: &Rope, tree: Option<&Tree>, pos: usize) -> Option<(usize, usize)> {
    let at = text.get_char(pos).filter(|&c| kind(c).is_some()).map(|_| pos);
    let before = pos.checked_sub(1).filter(|&p| text.get_char(p).and_then(kind).is_some_and(|(_, open)| !open));
    let bracket = at.or(before)?;
    let partner =
        tree.and_then(|tree| partner_in_tree(text, tree, bracket)).or_else(|| partner_by_scan(text, bracket))?;
    Some((bracket, partner))
}

/// The pair `c` belongs to and whether it opens.
fn kind(c: char) -> Option<((char, char), bool)> {
    PAIRS.iter().find_map(|&pair| match c {
        _ if c == pair.0 => Some((pair, true)),
        _ if c == pair.1 => Some((pair, false)),
        _ => None,
    })
}

/// `None` both when the tree pairs nothing with it and when it isn't a token of the tree.
fn partner_in_tree(text: &Rope, tree: &Tree, bracket: usize) -> Option<usize> {
    let ch = text.char(bracket);
    let ((open, close), opens) = kind(ch)?;
    let byte = text.char_to_byte(bracket);
    let node = tree.root_node().descendant_for_byte_range(byte, byte + 1)?;
    if node.start_byte() != byte || node.end_byte() != byte + 1 || node.child_count() > 0 {
        return None;
    }
    let want = if opens { close } else { open }.to_string();
    let mut sibling = node;
    loop {
        sibling = if opens { sibling.next_sibling() } else { sibling.prev_sibling() }?;
        if sibling.kind() == want && sibling.end_byte() - sibling.start_byte() == 1 {
            return Some(text.byte_to_char(sibling.start_byte()));
        }
    }
}

fn partner_by_scan(text: &Rope, bracket: usize) -> Option<usize> {
    let ((open, close), opens) = kind(text.char(bracket))?;
    let mut depth = 0usize;
    let mut step = |i: usize, c: char| {
        if c == open || c == close {
            if (c == open) == opens {
                depth += 1;
            } else {
                depth -= 1;
            }
        }
        (depth == 0).then_some(i)
    };
    if opens {
        let end = (bracket + SCAN_LIMIT).min(text.len_chars());
        text.slice(bracket..end).chars().enumerate().find_map(|(i, c)| step(bracket + i, c))
    } else {
        let start = bracket.saturating_sub(SCAN_LIMIT);
        let mut chars = text.slice(start..=bracket).chars_at(bracket + 1 - start);
        let mut i = bracket + 1;
        std::iter::from_fn(|| chars.prev()).find_map(|c| {
            i -= 1;
            step(i, c)
        })
    }
}
