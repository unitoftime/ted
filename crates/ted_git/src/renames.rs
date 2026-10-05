//! Renames in the working tree. Until both sides are staged, git sees a renamed file as
//! a deletion and an untracked file. To show the rename it is, the untracked files are
//! marked intent-to-add in a scratch copy of the index, where git's own rename detection
//! pairs them with the deletions by the rule it applies to staged changes.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::git::{args, git, git_with_index};
use crate::model::{parse_diff, FileDiff};

/// A copy of the repository's index for git to change, leaving the real one as it is.
struct ScratchIndex(PathBuf);

impl ScratchIndex {
    fn of(root: &Path) -> Result<Self, String> {
        static COPIES: AtomicUsize = AtomicUsize::new(0);
        let index = git(root, &args(&["rev-parse", "--git-path", "index"]), None)?;
        let name = format!("ted-index-{}-{}", std::process::id(), COPIES.fetch_add(1, Ordering::Relaxed));
        let copy = Self(std::env::temp_dir().join(name));
        std::fs::copy(root.join(index.trim_end_matches('\n')), &copy.0)
            .map_err(|e| format!("cannot copy the index: {}", e))?;
        Ok(copy)
    }

    fn git(&self, root: &Path, args: &[String], stdin: Option<&str>) -> Result<String, String> {
        git_with_index(root, &self.0, args, stdin)
    }
}

impl Drop for ScratchIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The files of `diff` (a `git diff` command line comparing against the working tree),
/// with the renames in it found. `untracked` is the status's list of untracked paths,
/// when there is one: those a rename accounts for are taken out of it.
///
/// Costs nothing extra unless the diff deletes a file while files are untracked.
pub fn worktree_diff(
    root: &Path,
    diff: &[String],
    untracked: Option<&mut Vec<String>>,
) -> Result<Vec<FileDiff>, String> {
    let mut files = parse_diff(&git(root, diff, None)?);
    let nothing_untracked = untracked.as_ref().is_some_and(|paths| paths.is_empty());
    if nothing_untracked || !files.iter().any(|f| f.kind == "deleted") {
        return Ok(files);
    }
    // A refinement of the diff: if it fails, the deletions are shown as git reports them.
    let Ok((renames, others)) = detect(root, diff) else { return Ok(files) };
    if renames.is_empty() {
        return Ok(files);
    }
    let from: HashSet<&str> = renames.iter().filter_map(|f| f.from.as_deref()).collect();
    files.retain(|f| !from.contains(f.path.as_str()));
    if let Some(untracked) = untracked {
        let to: HashSet<&str> = renames.iter().map(|f| f.path.as_str()).collect();
        let mut left: Vec<&str> = others.split_terminator('\0').filter(|path| !to.contains(path)).collect();
        left.sort_unstable();
        // A directory stays listed while anything in it is still untracked.
        untracked.retain(|entry| match entry.ends_with('/') {
            true => left[left.partition_point(|path| *path < entry.as_str())..]
                .first()
                .is_some_and(|path| path.starts_with(entry.as_str())),
            false => !to.contains(entry.as_str()),
        });
    }
    files.extend(renames);
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// The renames `diff` shows once every untracked file counts as added, and those files
/// as `git ls-files -z` lists them.
fn detect(root: &Path, diff: &[String]) -> Result<(Vec<FileDiff>, String), String> {
    let others = git(root, &args(&["ls-files", "--others", "--exclude-standard", "-z"]), None)?;
    if others.is_empty() {
        return Ok((Vec::new(), others));
    }
    let index = ScratchIndex::of(root)?;
    let add = ["--literal-pathspecs", "add", "--intent-to-add", "--pathspec-from-file=-", "--pathspec-file-nul"];
    index.git(root, &args(&add), Some(&others))?;
    let mut renames = diff.to_vec();
    renames.insert(1, "--diff-filter=R".to_string());
    Ok((parse_diff(&index.git(root, &renames, None)?), others))
}
