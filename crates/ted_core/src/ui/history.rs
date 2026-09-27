//! Input history: what was entered into each prompt (and search), recalled with `M-p` /
//! `M-n` and kept across sessions in `~/.config/ted/history`.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;

use crate::ui::LineInput;

const MAX_ENTRIES: usize = 100;

/// Entries by history id (a prompt's id, `search`), oldest first.
#[derive(Debug, Default)]
pub struct InputHistory {
    save_path: Option<PathBuf>,
    entries: HashMap<String, Vec<String>>,
}

impl InputHistory {
    pub fn default_save_path() -> Option<PathBuf> {
        crate::config::config_dir().map(|d| d.join("history"))
    }

    /// Loads the history persisted at `save_path` (one `id<TAB>entry` per line); `None`
    /// gives an in-memory history.
    pub fn load_or_default(save_path: Option<PathBuf>) -> Self {
        let mut history = Self { save_path, entries: HashMap::new() };
        let content = history.save_path.as_ref().and_then(|p| fs::read_to_string(p).ok()).unwrap_or_default();
        for (id, entry) in content.lines().filter_map(|line| line.split_once('\t')) {
            history.entries.entry(id.to_string()).or_default().push(entry.to_string());
        }
        history
    }

    pub fn entries(&self, id: &str) -> &[String] {
        self.entries.get(id).map_or(&[], Vec::as_slice)
    }

    pub fn newest(&self, id: &str) -> Option<&str> {
        self.entries(id).last().map(String::as_str)
    }

    /// Adds `text` as the newest entry of `id` (moving it there if already present) and
    /// saves, so nothing is lost if ted exits abruptly.
    pub fn record(&mut self, id: &str, text: &str) {
        if self.newest(id) == Some(text) {
            return;
        }
        let list = self.entries.entry(id.to_string()).or_default();
        list.retain(|e| e != text);
        list.push(text.to_string());
        if list.len() > MAX_ENTRIES {
            list.remove(0);
        }
        let _ = self.save();
    }

    fn save(&self) -> io::Result<()> {
        let Some(path) = &self.save_path else { return Ok(()) };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = String::new();
        for (id, list) in &self.entries {
            for entry in list {
                out.push_str(id);
                out.push('\t');
                out.push_str(entry);
                out.push('\n');
            }
        }
        fs::write(path, out)
    }
}

/// A modal's position while stepping through its history.
#[derive(Debug, Default)]
pub struct HistoryCursor {
    /// Entry shown, counting back from the newest (0); `None` while editing `draft`.
    recalled: Option<usize>,
    /// What was typed before recalling, restored by stepping past the newest entry.
    draft: String,
}

impl HistoryCursor {
    /// Steps `delta` entries back (positive) or forward through `entries`, putting the
    /// result in `input`. Returns false when there is no older entry.
    pub fn step(&mut self, input: &mut LineInput, entries: &[String], delta: isize) -> bool {
        let target = self.recalled.map_or(-1, |i| i as isize) + delta;
        if target >= entries.len() as isize {
            return false;
        }
        if self.recalled.is_none() {
            self.draft = input.text().to_string();
        }
        match usize::try_from(target) {
            Ok(back) => {
                input.set(entries[entries.len() - 1 - back].as_str());
                self.recalled = Some(back);
            }
            Err(_) => {
                input.set(std::mem::take(&mut self.draft));
                self.recalled = None;
            }
        }
        true
    }
}
