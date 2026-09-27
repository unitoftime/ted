//! Default key bindings, as data. Users override any of these from `init.rhai` with
//! `bind`, `bind_mode`, `unbind` and `unbind_mode`, using the same notation.
//!
//! Defaults are what any user of the editor should get. Personal choices belong in
//! `init.rhai`, and `C-c <key>` in the global map is reserved for them: defaults (built-in
//! and plugin) never bind it, except the help listing what the user put there (`C-c ?`). Modes bind
//! `C-c C-<key>`; minor keymaps `C-c <punctuation>` (`C-c !`).

use crate::editor::Editor;
use crate::keymap::KeymapId;

/// Motion and editing within a line. Bound globally and in every modal text input (a
/// one-line buffer), so prompts edit like any other buffer.
const LINE_EDITING: &[(&str, &str)] = &[
    // Motion
    ("<left>", "backward-char"),
    ("<right>", "forward-char"),
    ("<home>", "beginning-of-line"),
    ("<end>", "end-of-line"),
    ("C-a", "beginning-of-line"),
    ("C-e", "end-of-line"),
    ("C-b", "backward-char"),
    ("C-f", "forward-char"),
    ("M-f", "forward-word"),
    ("M-b", "backward-word"),
    ("M-<", "beginning-of-buffer"),
    ("M->", "end-of-buffer"),
    // Editing
    ("DEL", "delete-backward-char"),
    ("<delete>", "delete-char"),
    ("C-d", "delete-char"),
    ("C-DEL", "backward-kill-word"),
    ("M-DEL", "backward-kill-word"),
    ("C-M-DEL", "backward-kill-word"),
    ("M-d", "kill-word"),
    ("C-k", "kill-line"),
    ("C-w", "kill-region"),
    ("M-w", "copy-region"),
    ("C-y", "yank"),
    ("M-y", "yank-pop"),
    ("C-/", "undo"),
    ("C-_", "undo"),
    ("C-?", "redo"),
    ("M-_", "redo"),
    ("C-Z", "redo"),
    // Mark
    ("C-SPC", "set-mark-command"),
    ("C-@", "set-mark-command"),
];

const GLOBAL: &[(&str, &str)] = &[
    // Motion
    ("<up>", "previous-line"),
    ("<down>", "next-line"),
    ("<prior>", "scroll-up"),
    ("<next>", "scroll-down"),
    ("C-p", "previous-line"),
    ("C-n", "next-line"),
    ("C-v", "scroll-down"),
    ("M-v", "scroll-up"),
    ("C-l", "recenter-top-bottom"),
    ("M-g g", "goto-line"),
    ("<mouse-1>", "mouse-set-point"),
    ("<double-mouse-1>", "mouse-select-line"),
    ("<drag-mouse-1>", "mouse-drag-region"),
    ("M-g M-g", "goto-line"),
    // Editing
    ("RET", "newline"),
    ("TAB", "indent-for-tab-command"),
    ("<backtab>", "outdent"),
    ("C-o", "open-line"),
    ("C-g", "keyboard-quit"),
    // Search
    ("C-s", "search-forward"),
    ("C-r", "search-backward"),
    // Cross-references
    ("M-.", "find-definition"),
    ("M-,", "xref-go-back"),
    ("C-M-,", "xref-go-forward"),
    // Errors
    ("M-g n", "next-error"),
    ("M-g M-n", "next-error"),
    ("M-g p", "previous-error"),
    ("M-g M-p", "previous-error"),
    // Display
    ("C-=", "text-scale-increase"),
    ("C-+", "text-scale-increase"),
    ("C--", "text-scale-decrease"),
    ("C-0", "text-scale-reset"),
    // Buffers
    ("C-x *", "switch-to-scratch"),
    // Help
    ("M-x", "execute-extended-command"),
    ("<f1>", "execute-extended-command"),
];

/// `C-x <key>` commands, also reachable with Ctrl held on the second key.
const CTRL_X: &[(&str, &str)] = &[
    ("2", "split-window-below"),
    ("3", "split-window-right"),
    ("1", "delete-other-windows"),
    ("0", "delete-window"),
    ("o", "other-window"),
    ("O", "previous-window"),
    ("s", "save-buffer"),
    ("f", "find-file"),
    ("d", "dired"),
    ("w", "save-buffer-as"),
    ("b", "switch-to-buffer"),
    ("c", "exit-ted"),
    ("k", "kill-buffer"),
    ("u", "undo-tree-visualize"),
    ("x", "exchange-point-and-mark"),
    ("h", "mark-whole-buffer"),
];

/// Prefix help, bound after `CTRL_X` so `C-x C-h` shows help rather than marking.
const PREFIX_HELP: &[(&str, &str)] = &[
    ("C-x ?", "describe-prefix C-x"),
    ("C-x C-h", "describe-prefix C-x"),
    ("C-c ?", "describe-prefix C-c"),
    ("C-c C-h", "describe-prefix C-c"),
];

/// Every modal with a text input, on top of `LINE_EDITING`. `C-w` kills a word, as in a shell.
const INPUT: &[(&str, &str)] = &[("C-w", "backward-kill-word"), ("C-g", "modal-quit"), ("ESC", "modal-quit")];

const MINIBUFFER: &[(&str, &str)] = &[
    ("RET", "minibuffer-submit"),
    ("TAB", "minibuffer-complete"),
    ("M-p", "input-history-previous"),
    ("M-n", "input-history-next"),
];

const SEARCH: &[(&str, &str)] = &[
    ("C-s", "search-repeat-forward"),
    ("C-r", "search-repeat-backward"),
    ("RET", "search-exit"),
    ("M-p", "input-history-previous"),
    ("M-n", "input-history-next"),
];

const PICKER: &[(&str, &str)] = &[
    ("RET", "picker-select"),
    ("TAB", "picker-cycle-next"),
    ("<backtab>", "picker-cycle-previous"),
    ("<down>", "picker-next"),
    ("C-n", "picker-next"),
    ("<up>", "picker-previous"),
    ("C-p", "picker-previous"),
    ("<next>", "picker-page-down"),
    ("C-v", "picker-page-down"),
    ("<prior>", "picker-page-up"),
    ("M-v", "picker-page-up"),
    ("C-c", "modal-quit"),
];

const CHOICE: &[(&str, &str)] = &[("C-g", "modal-quit"), ("ESC", "modal-quit")];

const MENU: &[(&str, &str)] = &[("C-g", "modal-quit"), ("ESC", "modal-quit")];

const JUMP: &[(&str, &str)] = &[("C-g", "modal-quit"), ("ESC", "modal-quit")];

/// Generated read-only buffers (`Mode::special`): listings, logs, help, location lists.
/// Their modes bind their own keys over these.
const SPECIAL: &[(&str, &str)] = &[
    ("h", "mode-help"),
    ("?", "mode-help"),
    ("n", "row-next"),
    ("p", "row-previous"),
    ("g", "revert-buffer"),
    ("q", "quit-window"),
];

const UNDO_TREE: &[(&str, &str)] = &[
    ("<up>", "undo-tree-undo"),
    ("p", "undo-tree-undo"),
    ("P", "undo-tree-undo"),
    ("k", "undo-tree-undo"),
    ("<down>", "undo-tree-redo"),
    ("n", "undo-tree-redo"),
    ("N", "undo-tree-redo"),
    ("j", "undo-tree-redo"),
    ("<left>", "undo-tree-prev-branch"),
    ("b", "undo-tree-prev-branch"),
    ("B", "undo-tree-prev-branch"),
    ("h", "undo-tree-prev-branch"),
    ("<right>", "undo-tree-next-branch"),
    ("f", "undo-tree-next-branch"),
    ("F", "undo-tree-next-branch"),
    ("l", "undo-tree-next-branch"),
    ("q", "modal-quit"),
    ("Q", "modal-quit"),
    ("RET", "modal-quit"),
    ("ESC", "modal-quit"),
    ("C-g", "modal-quit"),
];

pub(crate) fn install_defaults(ed: &mut Editor) {
    ed.bind_all("global", LINE_EDITING);
    ed.bind_all("global", GLOBAL);
    for (key, command) in CTRL_X {
        ed.bind_all("global", &[(&format!("C-x {}", key), command), (&format!("C-x C-{}", key), command)]);
    }
    ed.bind_all("global", PREFIX_HELP);
    ed.bind_all("input", LINE_EDITING);
    ed.bind_all("input", INPUT);
    ed.bind_all("minibuffer", MINIBUFFER);
    ed.bind_all("search", SEARCH);
    ed.bind_all("picker", PICKER);
    ed.bind_all("choice", CHOICE);
    ed.bind_all("menu", MENU);
    ed.bind_all("undo-tree", UNDO_TREE);
    ed.bind_all("jump", JUMP);
    ed.bind_all("special", SPECIAL);

    let self_insert = [
        (KeymapId::GLOBAL, "self-insert-command"),
        (KeymapId::INPUT, "self-insert-command"),
        (KeymapId::CHOICE, "choice-select"),
        (KeymapId::MENU, "menu-select"),
        (KeymapId::JUMP, "jump-input"),
    ];
    for (map, command) in self_insert {
        ed.keymaps.get_mut(map).self_insert = ed.commands.id(command);
    }
    // Keys search doesn't use end it where it is, then act on the buffer (C-n, C-v, M-x).
    ed.keymaps.get_mut(KeymapId::SEARCH).fallback = ed.commands.id("search-exit-and-replay");
}
