//! Running the `git` command line. These calls block, so the UI only makes them from
//! background jobs.

use std::path::Path;

use ted_core::process::{Output, Program};

/// Runs `git <args>` in `root`, feeding `stdin` if given.
pub fn git_output(root: &Path, args: &[String], stdin: Option<&str>) -> Output {
    Program::new("git", root)
        .args(["-c", "core.quotepath=false", "-c", "color.ui=false"])
        .args(args.iter().cloned())
        .output(stdin.map(str::as_bytes))
}

/// Runs git and returns stdout, or the error output when it fails.
pub fn git(root: &Path, args: &[String], stdin: Option<&str>) -> Result<String, String> {
    git_output(root, args, stdin).into_result()
}

/// Convenience for literal argument lists.
pub fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}
