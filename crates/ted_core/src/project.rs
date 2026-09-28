//! Projects: the unit that searches and builds work on.
//!
//! Every buffer has a working directory (`Buffer::directory`); its project is the nearest
//! enclosing git repository, else that directory alone. Commands that look across files
//! (xref, compile, git, the buffer switcher) resolve the project with `Editor::project`
//! rather than each deciding where to run.
//!
//! Listing a large repository takes a while, so the last listing of each project is kept
//! in `FileLists` and refreshed in the background whenever it is used: the file switcher
//! opens populated, and new files show up once the refresh lands.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::editor::Editor;
use crate::process::Program;
use crate::text::directory_of;

/// Files listed per project at most.
pub const MAX_FILES: usize = 50_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub root: PathBuf,
    git: bool,
}

impl Project {
    /// The project containing the absolute directory `dir`.
    pub fn containing(dir: &Path) -> Self {
        match dir.ancestors().find(|d| d.join(".git").exists()) {
            Some(root) => Self { root: root.to_path_buf(), git: true },
            None => Self { root: dir.to_path_buf(), git: false },
        }
    }

    /// The project of the file or directory at `path`; the working directory's for `None`.
    pub fn of(path: Option<&Path>) -> Self {
        Self::containing(&directory_of(path))
    }

    /// Whether the root is a git repository (rather than a lone directory).
    pub fn is_git(&self) -> bool {
        self.git
    }

    pub fn name(&self) -> String {
        self.root.file_name().map_or_else(|| self.root.display().to_string(), |n| n.to_string_lossy().to_string())
    }

    /// The project's files, as paths relative to the root: git's view in a repository
    /// (tracked and untracked, minus ignored), else a walk that skips hidden directories.
    /// Blocks on the file system; call it from a job.
    pub fn files(&self) -> Vec<PathBuf> {
        if self.git {
            if let Some(files) = self.git_files() {
                return files;
            }
        }
        self.walk_files()
    }

    fn git_files(&self) -> Option<Vec<PathBuf>> {
        let listing = Program::new("git", &self.root)
            .args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"])
            .output(None)
            .into_result()
            .ok()?;
        let names = listing.split('\0').filter(|name| !name.is_empty());
        Some(names.take(MAX_FILES).map(PathBuf::from).collect())
    }

    fn walk_files(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let mut dirs = vec![self.root.clone()];
        while let Some(dir) = dirs.pop() {
            for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
                let hidden = entry.file_name().to_string_lossy().starts_with('.');
                match entry.file_type() {
                    Ok(t) if t.is_dir() && !hidden => dirs.push(entry.path()),
                    Ok(t) if t.is_file() => {
                        if let Ok(relative) = entry.path().strip_prefix(&self.root) {
                            files.push(relative.to_path_buf());
                        }
                    }
                    _ => {}
                }
                if files.len() >= MAX_FILES {
                    return files;
                }
            }
        }
        files
    }
}

/// A project's files, relative to its root; `None` while its first listing runs.
type Listing = Option<Arc<[PathBuf]>>;

/// Each project's files (relative to its root) as last listed, shared with job threads.
/// Get it with `Editor::file_lists`.
#[derive(Clone, Default)]
pub struct FileLists {
    by_root: Arc<Mutex<HashMap<PathBuf, Listing>>>,
}

impl FileLists {
    /// The project's files as last listed, if they have been.
    pub fn cached(&self, project: &Project) -> Option<Arc<[PathBuf]>> {
        self.by_root.lock().get(&project.root).cloned().flatten()
    }

    /// Lists the project's files now and keeps the result. Blocks; call it from a job.
    pub fn list(&self, project: &Project) -> Arc<[PathBuf]> {
        let files: Arc<[PathBuf]> = project.files().into();
        self.by_root.lock().insert(project.root.clone(), Some(files.clone()));
        files
    }

    /// Lists the project in the background unless it is listed or being listed.
    fn warm(&self, ed: &Editor, project: &Project) {
        let mut by_root = self.by_root.lock();
        if by_root.contains_key(&project.root) {
            return;
        }
        by_root.insert(project.root.clone(), None);
        drop(by_root);
        let (lists, project) = (self.clone(), project.clone());
        ed.spawn(move |_| {
            lists.list(&project);
        });
    }
}

pub(crate) fn register(ed: &mut Editor) {
    // So the first file switcher in a repository opens with its files.
    ed.hooks.on_file_visited(|ed, id| {
        let project = ed.buffers[id].project();
        if project.is_git() {
            let lists = ed.file_lists();
            lists.warm(ed, &project);
        }
    });
}
