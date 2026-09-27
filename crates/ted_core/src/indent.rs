//! Structural indentation: how deep a line belongs, read off the syntax tree.
//!
//! A line's level is the number of distinct earlier lines that open a node enclosing it.
//! Nodes open when they begin with a bracket (every language), or when the grammar names
//! their kind as indenting (a Go `case`, a Python `def`). A line starting with a closing
//! node (`}`, Python's `else`, bash's `fi`) lines up with the line its parent opened on.
//! Lines inside a multi-line string or comment have no structural level and are left alone.

use std::ops::Range;

use ropey::Rope;
use tree_sitter::{Language, Node, Tree};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rule {
    None,
    /// Lines after the node's first are one level deeper.
    Indent,
    /// An opening bracket: the node it begins indents.
    Open,
    /// A line starting with the node aligns with its parent's first line.
    Close,
    /// Text whose lines are content, not code: strings, comments, heredocs.
    Verbatim,
}

/// A grammar's indentation rules, indexed by node kind id.
pub struct Indents {
    rules: Vec<Rule>,
}

impl Indents {
    /// Rules for `language`: brackets plus the named `indent` and `close` kinds.
    pub fn new(language: &Language, indent: &[&str], close: &[&str]) -> Self {
        let rules = (0..language.node_kind_count() as u16)
            .map(|id| {
                let name = language.node_kind_for_id(id).unwrap_or_default();
                match name {
                    "{" | "(" | "[" => Rule::Open,
                    "}" | ")" | "]" => Rule::Close,
                    _ if indent.contains(&name) => Rule::Indent,
                    _ if close.contains(&name) => Rule::Close,
                    _ if ["comment", "string", "heredoc"].iter().any(|k| name.contains(k)) => Rule::Verbatim,
                    _ => Rule::None,
                }
            })
            .collect();
        Self { rules }
    }

    fn rule(&self, node: Node) -> Rule {
        self.rules.get(node.kind_id() as usize).copied().unwrap_or(Rule::None)
    }

    fn indents(&self, node: Node) -> bool {
        self.rule(node) == Rule::Indent || node.child(0).is_some_and(|first| self.rule(first) == Rule::Open)
    }

    /// The level of each line in `lines`, or `None` where it has none (inside a string or
    /// comment that began on an earlier line).
    pub fn levels(&self, tree: &Tree, text: &Rope, lines: Range<usize>) -> Vec<Option<usize>> {
        let mut cursor = tree.walk();
        let mut chain: Vec<Node> = Vec::new();
        let lines = lines.start..lines.end.min(text.len_lines());
        lines
            .map(|line| {
                let indentation = text.line(line).chars().take_while(|&c| c == ' ' || c == '\t').count();
                let byte = text.line_to_byte(line) + indentation;

                // The nodes enclosing the line's first character, outermost first.
                cursor.reset(tree.root_node());
                chain.clear();
                chain.push(tree.root_node());
                while cursor.goto_first_child_for_byte(byte).is_some() && cursor.node().start_byte() <= byte {
                    chain.push(cursor.node());
                }
                self.level(&chain, line, byte)
            })
            .collect()
    }

    fn level(&self, chain: &[Node], line: usize, byte: usize) -> Option<usize> {
        let before = |node: &Node| node.start_position().row < line;
        if chain.iter().any(|n| before(n) && self.rule(*n) == Rule::Verbatim) {
            return None;
        }
        let closed_row = (1..chain.len())
            .find(|&i| chain[i].start_byte() == byte && self.rule(chain[i]) == Rule::Close)
            .map(|i| chain[i - 1].start_position().row);

        // Start rows only grow going inwards, so distinct rows are changes from the last.
        let (mut level, mut last_row) = (0, None);
        for node in chain.iter().filter(|n| before(n) && self.indents(**n)) {
            let row = node.start_position().row;
            if Some(row) != closed_row && Some(row) != last_row {
                level += 1;
                last_row = Some(row);
            }
        }
        Some(level)
    }
}
