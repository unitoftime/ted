//! Projects: the unit that searches and builds work on.
//!
//! Every buffer has a working directory (`Buffer::directory`); its project is the nearest
//! enclosing git repository, else that directory alone. Commands that look across files
//! (xref, compile, git, the buffer switcher) resolve the project with `Editor::project`
//! rather than each deciding where to run.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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
        let output = Command::new("git")
            .args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"])
            .current_dir(&self.root)
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        let names = output.stdout.split(|&b| b == 0).filter(|name| !name.is_empty());
        Some(names.take(MAX_FILES).map(|name| PathBuf::from(String::from_utf8_lossy(name).as_ref())).collect())
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
