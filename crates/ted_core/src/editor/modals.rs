//! The modal stack: prompts, choices, pickers and custom modals.

use crate::editor::Editor;
use crate::ui::{Choice, LineInput, Modal, Picker, PickerItem, Prompt};

impl Editor {
    pub fn push_modal(&mut self, modal: impl Modal) {
        self.modals.push(Box::new(modal));
    }

    pub fn has_modal(&self) -> bool {
        !self.modals.is_empty()
    }

    pub fn top_modal(&self) -> Option<&dyn Modal> {
        self.modals.last().map(|m| m.as_ref())
    }

    pub fn pop_modal(&mut self) -> Option<Box<dyn Modal>> {
        self.modals.pop()
    }

    fn modal_index<M: Modal>(&self) -> Option<usize> {
        self.modals.iter().rposition(|m| m.as_any().is::<M>())
    }

    /// The top-most modal of type `M`.
    pub fn modal<M: Modal>(&self) -> Option<&M> {
        self.modals[self.modal_index::<M>()?].as_any().downcast_ref()
    }

    pub fn modal_mut<M: Modal>(&mut self) -> Option<&mut M> {
        let idx = self.modal_index::<M>()?;
        self.modals[idx].as_any_mut().downcast_mut()
    }

    /// Removes and returns the top-most modal of type `M`.
    pub fn take_modal<M: Modal>(&mut self) -> Option<M> {
        let idx = self.modal_index::<M>()?;
        self.modals.remove(idx).into_any().downcast().ok().map(|m| *m)
    }

    /// Runs `f` with the top-most `M` temporarily detached so it can use the editor too.
    pub fn with_modal<M: Modal, R>(&mut self, f: impl FnOnce(&mut M, &mut Editor) -> R) -> Option<R> {
        let idx = self.modal_index::<M>()?;
        let mut modal = self.modals.remove(idx);
        let result = f(modal.as_any_mut().downcast_mut().expect("index found by type"), self);
        self.modals.insert(idx.min(self.modals.len()), modal);
        Some(result)
    }

    /// The top modal's text input, if it has one.
    pub fn input(&self) -> Option<&LineInput> {
        self.modals.last()?.input()
    }

    /// After a command: ends the top modal input's undo chain unless the command continued
    /// it, and tells the modal when the command edited its text.
    pub(crate) fn after_input_command(&mut self, continues_undo: bool) {
        let Some(mut modal) = self.modals.pop() else {
            return;
        };
        let changed = modal.input_mut().is_some_and(|input| {
            if !continues_undo {
                input.break_undo_chain();
            }
            input.sync()
        });
        let idx = self.modals.len();
        if changed {
            modal.input_changed(self);
        }
        self.modals.insert(idx.min(self.modals.len()), modal);
    }

    /// Steps the top modal's input `delta` entries back (positive) or forward through its
    /// history. Returns false when it has no history or no older entry.
    pub fn recall_history(&mut self, delta: isize) -> bool {
        let Some(mut modal) = self.modals.pop() else {
            return false;
        };
        let recalled = match modal.history() {
            Some((id, cursor, input)) => cursor.step(input, self.input_history.entries(id), delta),
            None => false,
        };
        let idx = self.modals.len();
        if recalled {
            modal.input_changed(self);
        }
        self.modals.insert(idx.min(self.modals.len()), modal);
        recalled
    }

    /// Reads a line in the minibuffer and passes it to `on_submit`.
    pub fn prompt(
        &mut self,
        id: &str,
        label: impl Into<String>,
        initial: impl Into<String>,
        on_submit: impl FnOnce(&mut Editor, String) + 'static,
    ) {
        let prompt = Prompt::new(id, label, on_submit).initial(initial);
        self.push_modal(prompt);
    }

    /// Asks a y/n question.
    pub fn confirm(&mut self, id: &str, label: impl Into<String>, on_answer: impl FnOnce(&mut Editor, bool) + 'static) {
        let choice = Choice::new(id, label, "yn", move |ed, key| on_answer(ed, key == 'y'));
        self.push_modal(choice);
    }

    /// Opens a fuzzy picker; `on_select` receives the index of the chosen item.
    pub fn pick(
        &mut self,
        id: &str,
        title: impl Into<String>,
        items: Vec<PickerItem>,
        on_select: impl FnOnce(&mut Editor, usize) + 'static,
    ) {
        let picker = Picker::new(id, title, items, on_select);
        self.push_modal(picker);
    }
}
