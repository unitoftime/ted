//! Directory watching: the editor learns that files changed on disk as it happens, instead
//! of polling them. On Linux the kernel reports changes (inotify); elsewhere, or when that
//! is unavailable, every file is rechecked periodically.
//!
//! Watching directories rather than files keeps working across atomic saves, where a
//! program writes a new file and renames it over the old one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::editor::Editor;

#[cfg(target_os = "linux")]
mod inotify;

#[cfg(not(target_os = "linux"))]
mod inotify {
    //! No event source on this platform: `Watcher` falls back to polling.

    use std::path::Path;

    use super::OnChange;
    use crate::jobs::JobContext;

    pub struct Inotify;

    impl Inotify {
        pub fn start(_: JobContext, _: OnChange) -> Option<Self> {
            None
        }

        pub fn add(&self, _: &Path) -> Option<i32> {
            None
        }

        pub fn remove(&self, _: i32) {}
    }
}

/// What changed in the watched directories.
pub enum Changes {
    /// These files, sorted.
    Paths(Vec<PathBuf>),
    /// Anything may have: events were lost, or there is nothing reporting them.
    Unknown,
}

/// Runs on the UI thread with each batch of changes. Returns whether it changed anything
/// visible.
pub type OnChange = fn(&mut Editor, Changes) -> bool;

/// How often everything is rechecked when nothing reports changes.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

pub struct Watcher {
    inotify: Option<inotify::Inotify>,
    /// Per directory asked for: its watch, if the kernel accepted it, and how many callers
    /// asked.
    dirs: HashMap<PathBuf, (Option<i32>, usize)>,
}

impl Watcher {
    /// Starts reporting changes in watched directories to `on_change`.
    pub fn new(ed: &mut Editor, on_change: OnChange) -> Self {
        let (_, ctx) = ed.job_context();
        let inotify = inotify::Inotify::start(ctx, on_change);
        if inotify.is_none() {
            ed.add_timer(POLL_INTERVAL, move |ed| on_change(ed, Changes::Unknown));
        }
        Self { inotify, dirs: HashMap::new() }
    }

    /// Reports changes to the files in `dir` until it is unwatched as many times.
    pub fn watch(&mut self, dir: &Path) {
        let Some(inotify) = &self.inotify else { return };
        let (wd, users) = self.dirs.entry(dir.to_path_buf()).or_insert((None, 0));
        *users += 1;
        // Retried by later callers when it failed (e.g. the directory did not exist yet).
        if wd.is_none() {
            *wd = inotify.add(dir);
        }
    }

    pub fn unwatch(&mut self, dir: &Path) {
        let (Some(inotify), Some((wd, users))) = (&self.inotify, self.dirs.get_mut(dir)) else { return };
        *users -= 1;
        if *users == 0 {
            if let Some(wd) = *wd {
                inotify.remove(wd);
            }
            self.dirs.remove(dir);
        }
    }
}
