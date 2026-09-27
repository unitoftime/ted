//! Running the `git` command line. These calls block, so the UI only makes them from
//! background jobs.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Everything a git invocation printed.
pub struct GitOutput {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Runs `git <args>` in `root`, feeding `stdin` if given.
pub fn git_output(root: &Path, args: &[String], stdin: Option<&str>) -> GitOutput {
    let failed = |e: String| GitOutput { ok: false, stdout: String::new(), stderr: e };
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .args(["-c", "core.quotepath=false", "-c", "color.ui=false"])
        .args(args)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return failed(format!("cannot run git: {}", e)),
    };
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        if let Err(e) = pipe.write_all(input.as_bytes()) {
            return failed(e.to_string());
        }
    }
    match child.wait_with_output() {
        Ok(out) => GitOutput {
            ok: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        },
        Err(e) => failed(e.to_string()),
    }
}

/// Runs git and returns stdout, or the error output when it fails.
pub fn git(root: &Path, args: &[String], stdin: Option<&str>) -> Result<String, String> {
    let out = git_output(root, args, stdin);
    if out.ok {
        return Ok(out.stdout);
    }
    let message = if out.stderr.trim().is_empty() { out.stdout } else { out.stderr };
    Err(message.trim().to_string())
}

/// Convenience for literal argument lists.
pub fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}
