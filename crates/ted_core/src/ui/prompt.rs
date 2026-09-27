//! Minibuffer modals: free-text prompts and single-key choices.

use crate::editor::Editor;
use crate::keymap::KeymapId;
use crate::ui::{HistoryCursor, LineInput, Modal};

type SubmitFn = Box<dyn FnOnce(&mut Editor, String)>;
type CancelFn = Box<dyn FnOnce(&mut Editor)>;
type ChooseFn = Box<dyn FnOnce(&mut Editor, char)>;
type ChangeFn = Box<dyn FnMut(&mut Editor, &str)>;

/// Result of tab-completing prompt text.
pub struct Completion {
    /// The input extended by the candidates' common prefix.
    pub text: String,
    /// The part of the input the candidates complete; a full candidate is `base + candidate`.
    pub base: String,
    pub candidates: Vec<String>,
    /// Candidates ending with this keep the prompt open when chosen (e.g. directories).
    pub continue_suffix: Option<char>,
}

pub type Completer = fn(&str) -> Completion;

/// Reads a line of text, then calls `on_submit` with it.
pub struct Prompt {
    id: String,
    pub label: String,
    pub input: LineInput,
    keymap: KeymapId,
    on_submit: Option<SubmitFn>,
    on_cancel: Option<CancelFn>,
    on_change: Option<ChangeFn>,
    /// Submit the input as typed rather than trimmed.
    verbatim: bool,
    pub completer: Option<Completer>,
    /// Consecutive TAB presses that did not change the input.
    pub(crate) idle_tabs: usize,
    history: HistoryCursor,
}

impl Prompt {
    pub fn new(id: &str, label: impl Into<String>, on_submit: impl FnOnce(&mut Editor, String) + 'static) -> Self {
        Self {
            id: id.to_string(),
            label: label.into(),
            input: LineInput::default(),
            keymap: KeymapId::MINIBUFFER,
            on_submit: Some(Box::new(on_submit)),
            on_cancel: None,
            on_change: None,
            verbatim: false,
            completer: None,
            idle_tabs: 0,
            history: HistoryCursor::default(),
        }
    }

    /// Reads keys through `keymap` instead of `minibuffer`.
    pub fn keymap(mut self, keymap: KeymapId) -> Self {
        self.keymap = keymap;
        self
    }

    pub fn initial(mut self, text: impl Into<String>) -> Self {
        self.input.set(text);
        self
    }

    pub fn completer(mut self, completer: Completer) -> Self {
        self.completer = Some(completer);
        self
    }

    pub fn on_cancel(mut self, f: impl FnOnce(&mut Editor) + 'static) -> Self {
        self.on_cancel = Some(Box::new(f));
        self
    }

    /// Calls `f` with the input whenever the user edits it.
    pub fn on_change(mut self, f: impl FnMut(&mut Editor, &str) + 'static) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }

    /// Submits the input exactly as typed, keeping leading and trailing whitespace.
    pub fn verbatim(mut self) -> Self {
        self.verbatim = true;
        self
    }

    /// Consumes the prompt, records the input (trimmed unless `verbatim`) in its history and
    /// runs the submit callback with it.
    pub fn submit(mut self, ed: &mut Editor) {
        let text = self.input.text();
        let text = if self.verbatim { text } else { text.trim() }.to_string();
        if !text.is_empty() {
            ed.input_history.record(&self.id, &text);
        }
        if let Some(f) = self.on_submit.take() {
            f(ed, text);
        }
    }
}

impl Modal for Prompt {
    fn id(&self) -> &str {
        &self.id
    }

    fn keymap(&self) -> KeymapId {
        self.keymap
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn input(&self) -> Option<&LineInput> {
        Some(&self.input)
    }

    fn input_mut(&mut self) -> Option<&mut LineInput> {
        Some(&mut self.input)
    }

    fn input_changed(&mut self, ed: &mut Editor) {
        self.idle_tabs = 0;
        if let Some(f) = &mut self.on_change {
            f(ed, self.input.text());
        }
    }

    fn history(&mut self) -> Option<(&str, &mut HistoryCursor, &mut LineInput)> {
        Some((&self.id, &mut self.history, &mut self.input))
    }

    fn uses_minibuffer(&self) -> bool {
        true
    }

    fn cancel(mut self: Box<Self>, ed: &mut Editor) {
        if let Some(f) = self.on_cancel.take() {
            f(ed);
        }
    }
}

/// Asks the user to press one of a few keys (y/n, r/k, ...).
pub struct Choice {
    id: String,
    label: String,
    /// Accepted keys, lowercase.
    keys: String,
    keymap: KeymapId,
    on_choose: Option<ChooseFn>,
    on_cancel: Option<CancelFn>,
}

impl Choice {
    pub fn new(
        id: &str,
        label: impl Into<String>,
        keys: &str,
        on_choose: impl FnOnce(&mut Editor, char) + 'static,
    ) -> Self {
        Self {
            id: id.to_string(),
            label: label.into(),
            keys: keys.to_lowercase(),
            keymap: KeymapId::CHOICE,
            on_choose: Some(Box::new(on_choose)),
            on_cancel: None,
        }
    }

    pub fn on_cancel(mut self, f: impl FnOnce(&mut Editor) + 'static) -> Self {
        self.on_cancel = Some(Box::new(f));
        self
    }

    /// The normalized key if `key` is one of the accepted choices.
    pub fn accepts(&self, key: char) -> Option<char> {
        let key = key.to_ascii_lowercase();
        self.keys.contains(key).then_some(key)
    }

    pub fn choose(mut self, ed: &mut Editor, key: char) {
        if let Some(f) = self.on_choose.take() {
            f(ed, key);
        }
    }
}

impl Modal for Choice {
    fn id(&self) -> &str {
        &self.id
    }

    fn keymap(&self) -> KeymapId {
        self.keymap
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn uses_minibuffer(&self) -> bool {
        true
    }

    fn cancel(mut self: Box<Self>, ed: &mut Editor) {
        if let Some(f) = self.on_cancel.take() {
            f(ed);
        }
    }
}
