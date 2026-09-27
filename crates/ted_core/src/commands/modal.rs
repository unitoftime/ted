//! Commands for modals: prompt submission and completion, picker navigation, single-key
//! choices, and dismissal. Text inputs are edited with the regular editing commands.

use crate::editor::Editor;
use crate::ui::picker::PAGE_STEP;
use crate::ui::{Choice, Menu, Picker, Prompt, Tooltip};

pub fn register(ed: &mut Editor) {
    let c = &mut ed.commands;
    c.register_hidden("minibuffer-submit", "Accept the minibuffer input", |ed, _| {
        if let Some(prompt) = ed.take_modal::<Prompt>() {
            prompt.submit(ed);
        }
    });
    c.register_hidden("minibuffer-complete", "Complete the input; press again to list candidates", complete);
    c.register_hidden("input-history-previous", "Recall the previous entry of this input's history", |ed, _| {
        recall(ed, 1)
    });
    c.register_hidden("input-history-next", "Recall the next entry of this input's history", |ed, _| recall(ed, -1));

    c.register_hidden("picker-next", "Select the next candidate", |ed, _| select(ed, 1, false));
    c.register_hidden("picker-previous", "Select the previous candidate", |ed, _| select(ed, -1, false));
    c.register_hidden("picker-cycle-next", "Select the next candidate, wrapping", |ed, _| select(ed, 1, true));
    c.register_hidden("picker-cycle-previous", "Select the previous candidate, wrapping", |ed, _| select(ed, -1, true));
    c.register_hidden("picker-page-down", "Select a page further down", |ed, _| select(ed, PAGE_STEP as isize, false));
    c.register_hidden("picker-page-up", "Select a page further up", |ed, _| select(ed, -(PAGE_STEP as isize), false));
    c.register_hidden("picker-select", "Choose the selected candidate", |ed, _| {
        if let Some(picker) = ed.take_modal::<Picker>() {
            picker.select(ed);
        }
    });

    c.register_hidden("choice-select", "Answer the pending question with the typed key", |ed, arg| {
        let Some(key) = arg.char().and_then(|ch| ed.modal::<Choice>()?.accepts(ch)) else {
            return;
        };
        if let Some(choice) = ed.take_modal::<Choice>() {
            choice.choose(ed, key);
        }
    });

    c.register_hidden("menu-select", "Run the menu action for the typed key", |ed, arg| {
        let Some(key) = arg.char().filter(|&k| ed.modal::<Menu>().is_some_and(|m| m.has_key(k))) else {
            return;
        };
        if let Some(menu) = ed.take_modal::<Menu>() {
            menu.choose(ed, key);
        }
    });

    c.register_hidden("tooltip-exit-and-replay", "Close the tooltip, then handle the key", |ed, arg| {
        if let Some(key) = arg.key() {
            ed.take_modal::<Tooltip>();
            ed.unread_key(key);
        }
    });

    c.register_hidden("modal-quit", "Dismiss the active prompt or popup", |ed, _| {
        if let Some(modal) = ed.pop_modal() {
            ed.set_status("Quit");
            modal.cancel(ed);
        }
    });
}

fn recall(ed: &mut Editor, delta: isize) {
    if !ed.recall_history(delta) {
        ed.set_status("No further history");
    }
}

fn select(ed: &mut Editor, delta: isize, wrap: bool) {
    if let Some(picker) = ed.modal_mut::<Picker>() {
        picker.move_selection(delta, wrap);
    }
}

/// TAB in a prompt with a completer: extend the input by the candidates' common prefix;
/// when that makes no progress twice in a row, list the candidates in a picker.
fn complete(ed: &mut Editor, _: &crate::command::Arg) {
    let Some(prompt) = ed.modal_mut::<Prompt>() else {
        return;
    };
    let Some(completer) = prompt.completer else {
        return;
    };
    let completion = completer(prompt.input.text());
    if completion.candidates.is_empty() {
        return;
    }
    if completion.candidates.len() == 1 || completion.text != prompt.input.text() {
        let progressed = completion.candidates.len() > 1;
        prompt.input.set(completion.text);
        prompt.idle_tabs = usize::from(progressed);
    } else if prompt.idle_tabs >= 1 {
        prompt.idle_tabs = 0;
        show_completions(ed);
    } else {
        prompt.idle_tabs += 1;
    }
}

/// Lists the top prompt's completion candidates in a picker. Choosing one fills the prompt;
/// a final candidate (not ending in the continue suffix) also submits it.
pub fn show_completions(ed: &mut Editor) {
    let Some(prompt) = ed.modal::<Prompt>() else {
        return;
    };
    let Some(completer) = prompt.completer else {
        return;
    };
    let completion = completer(prompt.input.text());
    let items = completion
        .candidates
        .iter()
        .map(|c| {
            let kind = match completion.continue_suffix {
                Some(suffix) if c.ends_with(suffix) => "Directory",
                _ => "File",
            };
            crate::ui::PickerItem::new(c.as_str(), kind)
        })
        .collect();
    let title = format!("Completions in {}", if completion.base.is_empty() { "./" } else { &completion.base });
    ed.pick("completions", title, items, move |ed, index| {
        let candidate = &completion.candidates[index];
        let full = format!("{}{}", completion.base, candidate);
        let keep_open = completion.continue_suffix.is_some_and(|s| candidate.ends_with(s));
        let Some(mut prompt) = ed.take_modal::<Prompt>() else {
            return;
        };
        prompt.input.set(full);
        prompt.idle_tabs = 0;
        if keep_open {
            ed.push_modal(prompt);
        } else {
            prompt.submit(ed);
        }
    });
}
