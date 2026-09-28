//! Jump: type the first letter of a word, then the label drawn over it, to move point
//! there, in any visible window. Targets nearest point get the shortest labels.

use crate::editor::Editor;
use crate::keymap::KeymapId;
use crate::ui::Modal;
use crate::view::{JumpLabel, ViewId};

/// Label keys, easiest to reach first.
const LABEL_KEYS: &[char] = &[
    'a', 's', 'd', 'f', 'g', 'h', 'j', 'k', 'l', 'q', 'w', 'e', 'r', 't', 'y', 'u', 'i', 'o', 'p', 'z', 'x', 'c', 'v',
    'b', 'n', 'm',
];

/// Which occurrences of the typed char are targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    WordStart,
    Anywhere,
}

struct Target {
    view: ViewId,
    pos: usize,
    /// The part of the label still to type.
    label: String,
}

/// The jump in progress: waiting for the head char while `targets` is empty, then for
/// label keys, each narrowing the targets until one is left.
pub struct Jump {
    scope: Scope,
    label: &'static str,
    targets: Vec<Target>,
}

impl Modal for Jump {
    fn id(&self) -> &str {
        "jump"
    }

    fn keymap(&self) -> KeymapId {
        KeymapId::JUMP
    }

    fn label(&self) -> &str {
        self.label
    }

    fn uses_minibuffer(&self) -> bool {
        true
    }

    fn cancel(self: Box<Self>, ed: &mut Editor) {
        clear_labels(ed);
    }
}

pub fn register(ed: &mut Editor) {
    let c = &mut ed.commands;
    c.register("jump", "Jump to a word in any window: type its first letter, then its label", |ed, _| {
        start(ed, Scope::WordStart)
    });
    c.register("jump-char", "Jump to a character in any window: type it, then its label", |ed, _| {
        start(ed, Scope::Anywhere)
    });
    c.register_hidden("jump-input", "Choose the head char or type a label", |ed, arg| {
        if let Some(ch) = arg.char() {
            input(ed, ch);
        }
    });
}

fn start(ed: &mut Editor, scope: Scope) {
    ed.push_modal(Jump { scope, label: "Head char: ", targets: Vec::new() });
}

fn input(ed: &mut Editor, ch: char) {
    let Some(mut jump) = ed.take_modal::<Jump>() else { return };
    if jump.targets.is_empty() {
        jump.targets = find_targets(ed, jump.scope, ch);
        if jump.targets.is_empty() {
            ed.set_status(format!("No matches for '{}'", ch));
            return;
        }
        let labels = labels(jump.targets.len());
        for (target, label) in jump.targets.iter_mut().zip(labels) {
            target.label = label;
        }
        jump.label = "Jump to: ";
    } else {
        let key = ch.to_ascii_lowercase();
        if !jump.targets.iter().any(|t| t.label.starts_with(key)) {
            // Not a label: keep waiting for one.
            ed.push_modal(jump);
            return;
        }
        jump.targets.retain_mut(|t| match t.label.strip_prefix(key) {
            Some(rest) => {
                t.label = rest.to_string();
                true
            }
            None => false,
        });
    }
    if let [target] = jump.targets.as_slice() {
        let (view, pos) = (target.view, target.pos);
        clear_labels(ed);
        jump_to(ed, view, pos);
        return;
    }
    show_labels(ed, &jump.targets);
    ed.push_modal(jump);
}

/// Matches of `ch` (ignoring case) on screen in every window, the active window's first,
/// each window's nearest its point first. Capped at the number of labels available.
fn find_targets(ed: &mut Editor, scope: Scope, ch: char) -> Vec<Target> {
    let matches = |c: char| c == ch || c.to_lowercase().eq(ch.to_lowercase());
    let active = ed.layout.active_id();
    let views: Vec<ViewId> = ed.layout.views().iter().map(|v| v.id()).collect();
    let mut found: Vec<(bool, usize, Target)> = Vec::new();
    for id in views {
        let Some(doc) = ed.doc_for(id) else { continue };
        if doc.buf.has_renderer() {
            continue;
        }
        let point = doc.pos();
        for sl in doc.screen_lines() {
            let (chars, content) = (sl.layout.chars(), doc.buf.line_content(sl.line));
            let mut prev_is_word = chars.start > 0 && content.char(chars.start - 1).is_alphanumeric();
            for (i, c) in chars.clone().zip(content.chars_at(chars.start)) {
                let head = scope == Scope::Anywhere || !prev_is_word;
                prev_is_word = c.is_alphanumeric();
                if head && sl.cols.contains(&sl.layout.col(i)) && matches(c) {
                    let pos = sl.line_start + i;
                    found.push((id != active, pos.abs_diff(point), Target { view: id, pos, label: String::new() }));
                }
            }
        }
    }
    found.sort_by_key(|&(other, distance, _)| (other, distance));
    found.truncate(LABEL_KEYS.len() * LABEL_KEYS.len());
    found.into_iter().map(|(_, _, t)| t).collect()
}

/// `n` distinct labels, prefix-free: as many single keys as still leave enough two-key
/// labels (on the remaining keys as prefixes) for the rest.
fn labels(n: usize) -> Vec<String> {
    let keys = LABEL_KEYS.len();
    let singles = if n <= keys { n } else { ((keys * keys).saturating_sub(n) / (keys - 1)).min(keys) };
    let mut out: Vec<String> = LABEL_KEYS[..singles].iter().map(char::to_string).collect();
    let pairs = LABEL_KEYS[singles..].iter().flat_map(|&a| LABEL_KEYS.iter().map(move |&b| format!("{}{}", a, b)));
    out.extend(pairs.take(n - singles));
    out
}

/// Puts every view in jump display (dimmed) with the labels of its targets.
fn show_labels(ed: &mut Editor, targets: &[Target]) {
    for view in ed.layout.views_mut() {
        let mut labels: Vec<JumpLabel> = targets
            .iter()
            .filter(|t| t.view == view.id())
            .map(|t| JumpLabel { pos: t.pos, text: t.label.clone() })
            .collect();
        labels.sort_by_key(|l| l.pos);
        view.jump = Some(labels);
    }
}

fn clear_labels(ed: &mut Editor) {
    for view in ed.layout.views_mut() {
        view.jump = None;
    }
}

/// Moves point to `pos` in `view`, remembering where it was for `M-,`.
fn jump_to(ed: &mut Editor, view: ViewId, pos: usize) {
    crate::xref::push_mark(ed);
    ed.layout.set_active(view);
    ed.doc().set_cursor(pos);
}
