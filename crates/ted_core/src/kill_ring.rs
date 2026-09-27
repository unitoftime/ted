//! The kill ring, kept in step with the system clipboard: every kill is copied out, and text
//! copied in other applications joins the ring before the next yank.

use parking_lot::Mutex;
use std::collections::VecDeque;

static SYSTEM_CLIPBOARD: Mutex<Option<arboard::Clipboard>> = Mutex::new(None);
static LAST_CLIPBOARD_TEXT: Mutex<Option<String>> = Mutex::new(None);

fn with_clipboard<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut arboard::Clipboard) -> R,
{
    let mut lock = SYSTEM_CLIPBOARD.lock();
    if lock.is_none() {
        *lock = arboard::Clipboard::new().ok();
    }
    lock.as_mut().map(f)
}

pub fn get_system_clipboard() -> Option<String> {
    with_clipboard(|cb| cb.get_text().ok()).flatten()
}

pub fn set_system_clipboard(text: &str) {
    *LAST_CLIPBOARD_TEXT.lock() = Some(text.to_string());
    with_clipboard(|cb| {
        let _ = cb.set_text(text.to_string());
    });
}

/// How a kill combines with the previous one. Consecutive kills merge into one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillMode {
    New,
    Append,
    Prepend,
}

pub struct KillRing {
    entries: VecDeque<String>,
    max_entries: usize,
}

impl Default for KillRing {
    fn default() -> Self {
        Self::new(64)
    }
}

impl KillRing {
    pub fn new(max_entries: usize) -> Self {
        Self { entries: VecDeque::new(), max_entries: max_entries.max(1) }
    }

    pub fn push(&mut self, text: String, mode: KillMode) {
        if text.is_empty() {
            return;
        }
        if let (Some(front), KillMode::Append | KillMode::Prepend) = (self.entries.front_mut(), mode) {
            if mode == KillMode::Append {
                front.push_str(&text);
            } else {
                front.insert_str(0, &text);
            }
            set_system_clipboard(front);
            return;
        }
        if self.entries.len() >= self.max_entries {
            self.entries.pop_back();
        }
        set_system_clipboard(&text);
        self.entries.push_front(text);
    }

    /// Pulls in text copied from other applications since our last kill.
    pub fn sync_from_system_clipboard(&mut self) {
        let Some(text) = get_system_clipboard().filter(|t| !t.is_empty()) else {
            return;
        };
        let mut last = LAST_CLIPBOARD_TEXT.lock();
        if last.as_deref() == Some(text.as_str()) {
            return;
        }
        *last = Some(text.clone());
        if self.entries.front() != Some(&text) {
            if self.entries.len() >= self.max_entries {
                self.entries.pop_back();
            }
            self.entries.push_front(text);
        }
    }

    pub fn current(&self) -> Option<&str> {
        self.entries.front().map(|s| s.as_str())
    }

    pub fn peek(&self, index: usize) -> Option<&str> {
        self.entries.get(index).map(|s| s.as_str())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// Serializes tests that touch the process-wide system clipboard.
pub static TEST_CLIPBOARD_LOCK: Mutex<()> = Mutex::new(());
