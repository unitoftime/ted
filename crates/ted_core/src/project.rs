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

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::editor::Editor;
use crate::process::Program;
use crate::text::directory_of;

/// Files listed per project at most.
pub const MAX_FILES: usize = 50_000;

/// Directories found by one `Project::dirs_under` at most.
pub const MAX_DIRS: usize = 10_000;

/// Paths handed to one `git check-ignore`. It prints as it reads, so its answer has to fit
/// in the pipe while the paths are still being written.
const IGNORE_BATCH: usize = 256;

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

    /// The project's directories at and below `tops`, level by level, at most `MAX_DIRS`.
    /// Those git ignores are left out with all they hold, and below `tops` so are hidden
    /// ones and other repositories. Blocks on the file system and git; call it from a job.
    pub fn dirs_under(&self, tops: Vec<PathBuf>) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut level = tops;
        while !level.is_empty() && found.len() < MAX_DIRS {
            let ignored = self.ignored(&level);
            level.retain(|dir| !ignored.contains(dir));
            let below = level.iter().flat_map(|dir| subdirs(dir)).collect();
            found.append(&mut level);
            level = below;
        }
        found.truncate(MAX_DIRS);
        found
    }

    /// Which of `paths` (absolute, in the project) git ignores; none outside a repository.
    fn ignored(&self, paths: &[PathBuf]) -> HashSet<PathBuf> {
        let mut ignored = HashSet::new();
        for batch in paths.chunks(IGNORE_BATCH).filter(|_| self.git) {
            let mut input = Vec::new();
            for path in batch {
                input.extend_from_slice(path.as_os_str().as_encoded_bytes());
                input.push(0);
            }
            // It fails when none of them is ignored, so only what it printed counts.
            let answer = Program::new("git", &self.root).args(["check-ignore", "-z", "--stdin"]).output(Some(&input));
            ignored.extend(answer.stdout.split('\0').filter(|path| !path.is_empty()).map(PathBuf::from));
        }
        ignored
    }
}

/// The directories in `dir` that a walk of its project goes into.
fn subdirs(dir: &Path) -> impl Iterator<Item = PathBuf> {
    let entries = fs::read_dir(dir).into_iter().flatten().flatten();
    let visible = entries.filter(|entry| {
        entry.file_type().is_ok_and(|t| t.is_dir()) && !entry.file_name().to_string_lossy().starts_with('.')
    });
    visible.map(|entry| entry.path()).filter(|dir| !dir.join(".git").exists())
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
