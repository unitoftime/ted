//! Running the `git` command line. These calls block, so the UI only makes them from
//! background jobs.

use std::path::Path;

use ted_core::process::{Output, Program};

/// `git <args>` in `root`. Queries are kept from taking the index lock to save refreshed
/// stat info as they go (`git status` and `git diff` would), since they run in the
/// background while the next command may be one that needs the lock.
fn program(root: &Path, args: &[String]) -> Program {
    Program::new("git", root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["-c", "core.quotepath=false", "-c", "color.ui=false", "-c", "diff.autoRefreshIndex=false"])
        .args(args.iter().cloned())
}

/// Runs `git <args>` in `root`, feeding `stdin` if given.
pub fn git_output(root: &Path, args: &[String], stdin: Option<&str>) -> Output {
    program(root, args).output(stdin.map(str::as_bytes))
}

/// Runs git and returns stdout, or the error output when it fails.
pub fn git(root: &Path, args: &[String], stdin: Option<&str>) -> Result<String, String> {
    git_output(root, args, stdin).into_result()
}

/// As `git`, reading and writing the index file `index` in place of the repository's own.
pub fn git_with_index(root: &Path, index: &Path, args: &[String], stdin: Option<&str>) -> Result<String, String> {
    program(root, args).env("GIT_INDEX_FILE", &index.to_string_lossy()).output(stdin.map(str::as_bytes)).into_result()
}

/// Convenience for literal argument lists.
pub fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}
