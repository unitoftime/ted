//! Recently visited files, most recent first. Saved as one path per line after every
//! visit, so the list survives restarts and crashes alike.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::text::absolutize;

pub const DEFAULT_MAX_RECENT_FILES: usize = 2048;

#[derive(Debug, Clone)]
pub struct RecentFiles {
    save_path: Option<PathBuf>,
    entries: Vec<PathBuf>,
    max_entries: usize,
}

impl Default for RecentFiles {
    /// An in-memory list that is never persisted.
    fn default() -> Self {
        Self::new(None, DEFAULT_MAX_RECENT_FILES)
    }
}

impl RecentFiles {
    pub fn new(save_path: Option<PathBuf>, max_entries: usize) -> Self {
        Self { save_path, entries: Vec::new(), max_entries: max_entries.max(1) }
    }

    pub fn default_save_path() -> Option<PathBuf> {
        crate::config::config_dir().map(|d| d.join("recentf"))
    }

    /// Loads the list persisted at `save_path`; `None` gives an in-memory list. Blank lines,
    /// `#` comments and repeats are skipped.
    pub fn load_or_default(save_path: Option<PathBuf>) -> Self {
        let content = save_path.as_ref().and_then(|p| fs::read_to_string(p).ok()).unwrap_or_default();
        let mut recent = Self::new(save_path, DEFAULT_MAX_RECENT_FILES);
        let mut seen = HashSet::new();
        recent.entries = content
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#') && seen.insert(*line))
            .take(recent.max_entries)
            .map(PathBuf::from)
            .collect();
        recent
    }

    pub fn entries(&self) -> &[PathBuf] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Moves `path` to the front, adding it if new, and saves. Directories aren't tracked.
    pub fn touch(&mut self, path: &Path) {
        let path = absolutize(path);
        if path.is_dir() {
            return;
        }
        self.entries.retain(|p| *p != path);
        self.entries.insert(0, path);
        self.entries.truncate(self.max_entries);
        let _ = self.save();
    }

    /// Writes the list to its save path, if it has one.
    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.save_path else { return Ok(()) };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = String::with_capacity(self.entries.len() * 48);
        for p in &self.entries {
            out.push_str(&p.to_string_lossy());
            out.push('\n');
        }
        fs::write(path, out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recent_files_mru_and_persistence() {
        let temp_dir = std::env::temp_dir().join(format!("ted_recentf_test_{}", std::process::id()));
        let save_file = temp_dir.join("recentf");
        let _ = fs::remove_dir_all(&temp_dir);

        let mut rf = RecentFiles::new(Some(save_file.clone()), 3);

        let p1 = temp_dir.join("file1.rs");
        let p2 = temp_dir.join("file2.rs");
        let p3 = temp_dir.join("file3.rs");
        let p4 = temp_dir.join("file4.rs");

        rf.touch(&p1);
        rf.touch(&p2);
        rf.touch(&p3);

        assert_eq!(rf.entries().len(), 3);
        assert_eq!(rf.entries()[0], p3);
        assert_eq!(rf.entries()[1], p2);
        assert_eq!(rf.entries()[2], p1);

        // Visiting again moves a file to the front.
        rf.touch(&p1);
        assert_eq!(rf.entries()[0], p1);
        assert_eq!(rf.entries()[1], p3);
        assert_eq!(rf.entries()[2], p2);
        assert_eq!(rf.entries().len(), 3);

        // A full list drops its oldest entry.
        rf.touch(&p4);
        assert_eq!(rf.entries().len(), 3);
        assert_eq!(rf.entries()[0], p4);
        assert_eq!(rf.entries()[1], p1);
        assert_eq!(rf.entries()[2], p3);

        let loaded = RecentFiles::load_or_default(Some(save_file));
        assert_eq!(loaded.entries(), rf.entries());

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
