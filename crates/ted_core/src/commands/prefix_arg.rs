//! The prefix argument: `C-u`, then optional digits, typed before a command to give it a
//! count (`C-u 80 -` inserts 80 dashes). While it's being typed a transient keymap reads
//! its digits; the first other key runs its command with the argument.

use crate::command::PrefixArg;
use crate::editor::Editor;
use crate::keymap::KeymapId;

pub fn register(ed: &mut Editor) {
    let c = &mut ed.commands;
    c.register("universal-argument", "Begin a prefix argument for the next command (4, times 4 per repeat)", |ed, _| {
        match ed.prefix_arg() {
            None => continue_argument(ed, PrefixArg::Raw(1), KeymapId::UNIVERSAL_ARGUMENT),
            Some(PrefixArg::Raw(presses)) => {
                continue_argument(ed, PrefixArg::Raw(presses + 1), KeymapId::UNIVERSAL_ARGUMENT)
            }
            // After digits, `C-u` ends the argument so digits typed next insert (`C-u 5 C-u 0`).
            Some(arg) => ed.set_prefix_arg(arg),
        }
    });
    c.register_hidden("digit-argument", "Append a digit to the prefix argument", |ed, arg| {
        let Some(digit) = arg.int() else { return };
        let n = match ed.prefix_arg() {
            Some(PrefixArg::Number(n)) if n < 0 => n.saturating_mul(10).saturating_sub(digit),
            Some(PrefixArg::Number(n)) => n.saturating_mul(10).saturating_add(digit),
            Some(PrefixArg::Minus) => -digit,
            Some(PrefixArg::Raw(_)) | None => digit,
        };
        continue_argument(ed, PrefixArg::Number(n), KeymapId::DIGIT_ARGUMENT);
    });
    c.register_hidden("negative-argument", "Make the prefix argument negative", |ed, _| {
        continue_argument(ed, PrefixArg::Minus, KeymapId::UNIVERSAL_ARGUMENT);
    });
}

/// Hands `arg` on to the next command, reading the next key in `keymap` first.
fn continue_argument(ed: &mut Editor, arg: PrefixArg, keymap: KeymapId) {
    ed.set_prefix_arg(arg);
    ed.set_transient_keymap(keymap);
    ed.set_status(format!("{}-", arg));
}
