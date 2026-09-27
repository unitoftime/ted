//! Emacs-style undo tree over rope snapshots.
//!
//! Ropes share structure, so a snapshot per node is cheap. Each node carries a `content_id`:
//! nodes committed as copies of older states ("undoing an undo") inherit that state's id,
//! which lets the buffer answer "is this the saved content?" in O(1).

use std::time::Instant;

use ropey::Rope;

#[derive(Clone, Debug)]
pub struct UndoNode {
    pub id: usize,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub active_child_idx: usize,
    pub rope: Rope,
    pub cursor: usize,
    pub content_id: u64,
    pub timestamp: Instant,
}

#[derive(Clone, Debug)]
pub struct TreeDisplayLine {
    pub node_id: usize,
    pub text: String,
    pub is_current: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum EditKind {
    #[default]
    None,
    Typing,
    Backspace,
    Delete,
}

/// Consecutive single-char edits grouped into one undo step (a typed word, a run of DELs).
#[derive(Clone, Copy, Debug, Default)]
pub struct EditGroup {
    pub kind: EditKind,
    pub count: usize,
    pub expected_cursor: usize,
    pub ended_on_space: bool,
}

const MAX_GROUP_LEN: usize = 20;

#[derive(Clone, Debug)]
pub struct UndoTree {
    pub nodes: Vec<UndoNode>,
    pub current_id: usize,
    in_undo_chain: bool,
    undo_chain_origin: Option<usize>,
    undo_chain_visited: Vec<(usize, usize)>,
    next_content_id: u64,
}

impl UndoTree {
    pub fn new(rope: Rope, cursor: usize) -> Self {
        let root = UndoNode {
            id: 0,
            parent: None,
            children: Vec::new(),
            active_child_idx: 0,
            rope,
            cursor,
            content_id: 0,
            timestamp: Instant::now(),
        };
        Self {
            nodes: vec![root],
            current_id: 0,
            in_undo_chain: false,
            undo_chain_origin: None,
            undo_chain_visited: Vec::new(),
            next_content_id: 1,
        }
    }

    pub fn current(&self) -> &UndoNode {
        &self.nodes[self.current_id]
    }

    pub fn current_content_id(&self) -> u64 {
        self.current().content_id
    }

    pub fn set_current_cursor(&mut self, cursor: usize) {
        self.nodes[self.current_id].cursor = cursor;
    }

    /// Commits a new state as a child of the current node.
    pub fn commit(&mut self, rope: Rope, cursor: usize) {
        let content_id = self.next_content_id;
        self.next_content_id += 1;
        self.push_node(rope, cursor, content_id);
    }

    fn push_node(&mut self, rope: Rope, cursor: usize, content_id: u64) {
        let new_id = self.nodes.len();
        self.nodes.push(UndoNode {
            id: new_id,
            parent: Some(self.current_id),
            children: Vec::new(),
            active_child_idx: 0,
            rope,
            cursor,
            content_id,
            timestamp: Instant::now(),
        });
        let parent = &mut self.nodes[self.current_id];
        parent.children.push(new_id);
        parent.active_child_idx = parent.children.len() - 1;
        self.current_id = new_id;
    }

    pub fn can_undo(&self) -> bool {
        self.current().parent.is_some()
    }

    pub fn can_redo(&self) -> bool {
        (self.in_undo_chain && !self.undo_chain_visited.is_empty()) || !self.current().children.is_empty()
    }

    /// Ends a run of consecutive undos. As in Emacs, the undos themselves become history:
    /// the visited states are re-committed on top of where the chain started.
    pub fn break_undo_chain(&mut self) {
        if !self.in_undo_chain {
            return;
        }
        self.in_undo_chain = false;
        let visited = std::mem::take(&mut self.undo_chain_visited);
        if let Some(origin) = self.undo_chain_origin.take() {
            if self.current_id != origin && !visited.is_empty() {
                self.current_id = origin;
                for (node_id, cursor) in visited {
                    let node = &self.nodes[node_id];
                    let (rope, content_id) = (node.rope.clone(), node.content_id);
                    self.push_node(rope, cursor, content_id);
                }
            }
        }
    }

    pub fn undo(&mut self) -> Option<(Rope, usize)> {
        if !self.in_undo_chain {
            self.in_undo_chain = true;
            self.undo_chain_origin = Some(self.current_id);
            self.undo_chain_visited.clear();
        }
        let parent_id = self.current().parent?;
        if let Some(pos) = self.nodes[parent_id].children.iter().position(|&c| c == self.current_id) {
            self.nodes[parent_id].active_child_idx = pos;
        }
        self.current_id = parent_id;
        let node = self.current();
        let cursor = node.cursor;
        let rope = node.rope.clone();
        self.undo_chain_visited.push((parent_id, cursor));
        Some((rope, cursor))
    }

    pub fn redo(&mut self) -> Option<(Rope, usize)> {
        if self.in_undo_chain && self.undo_chain_visited.pop().is_some() {
            let target = self.undo_chain_visited.last().map(|(id, _)| *id).or(self.undo_chain_origin)?;
            self.current_id = target;
            return Some(self.current_state());
        }
        let curr = self.current();
        let child_id = *curr.children.get(curr.active_child_idx.min(curr.children.len().checked_sub(1)?))?;
        self.current_id = child_id;
        Some(self.current_state())
    }

    pub fn switch_branch(&mut self, delta: isize) -> Option<(Rope, usize)> {
        self.break_undo_chain();
        if let Some(parent_id) = self.current().parent {
            let siblings = &self.nodes[parent_id].children;
            if siblings.len() > 1 {
                let pos = siblings.iter().position(|&c| c == self.current_id).unwrap_or(0);
                let new_pos = (pos as isize + delta).rem_euclid(siblings.len() as isize) as usize;
                let sibling = siblings[new_pos];
                self.nodes[parent_id].active_child_idx = new_pos;
                self.current_id = sibling;
                return Some(self.current_state());
            }
        }
        let node = &mut self.nodes[self.current_id];
        if node.children.len() > 1 {
            let n = node.children.len() as isize;
            node.active_child_idx = (node.active_child_idx as isize + delta).rem_euclid(n) as usize;
        }
        Some(self.current_state())
    }

    fn current_state(&self) -> (Rope, usize) {
        let node = self.current();
        (node.rope.clone(), node.cursor)
    }

    pub fn format_tree(&self) -> Vec<TreeDisplayLine> {
        let mut lines = Vec::new();
        self.build_tree_display(0, "", true, &mut lines);
        lines
    }

    fn build_tree_display(&self, node_id: usize, prefix: &str, is_root: bool, out: &mut Vec<TreeDisplayLine>) {
        let node = &self.nodes[node_id];
        let is_current = node_id == self.current_id;
        let marker = if is_current { "●" } else { "○" };
        let char_count = node.rope.len_chars();
        let first_line = node.rope.line(0).to_string();
        let trimmed = first_line.trim_end_matches(['\r', '\n']);
        let preview = if trimmed.is_empty() {
            format!("({} chars)", char_count)
        } else if trimmed.chars().count() > 20 {
            let s: String = trimmed.chars().take(20).collect();
            format!("\"{}...\" ({} chars)", s, char_count)
        } else {
            format!("\"{}\" ({} chars)", trimmed, char_count)
        };

        let text = if is_root {
            format!("{} [r{}] {}", marker, node_id, preview)
        } else {
            format!("{}{} [r{}] {}", prefix, marker, node_id, preview)
        };
        out.push(TreeDisplayLine { node_id, text, is_current });

        let n = node.children.len();
        for (i, &child_id) in node.children.iter().enumerate() {
            let continuation = if i + 1 == n { "    " } else { "│   " };
            let child_prefix = if is_root {
                continuation.to_string()
            } else {
                format!("{}{}", prefix.trim_end_matches(['─', ' ', '└', '├']), continuation)
            };
            self.build_tree_display(child_id, &child_prefix, false, out);
        }
    }
}

impl EditGroup {
    /// Whether an edit of `kind` at `cursor` continues this group.
    pub fn continues(&self, kind: EditKind, cursor: usize, is_space: bool) -> bool {
        self.kind == kind
            && kind != EditKind::None
            && cursor == self.expected_cursor
            && self.count < MAX_GROUP_LEN
            && (kind != EditKind::Typing || !self.ended_on_space || is_space)
    }
}
