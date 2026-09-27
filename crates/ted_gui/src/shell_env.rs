//! Imports the login shell's environment when ted is started from a desktop launcher.
//!
//! Launchers hand ted the bare session environment: a `PATH` extended in shell rc files
//! (language servers in `~/go/bin`, `~/.cargo/bin`) and agent sockets set up there
//! (`SSH_AUTH_SOCK` from ssh-agent) would be missing from everything ted runs. Started from
//! a terminal, ted already inherits that environment and this does nothing.

use std::ffi::OsStr;
use std::io::{IsTerminal, Read};
use std::os::unix::ffi::OsStrExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// How long a slow rc file may delay startup before ted gives up and keeps its own env.
const TIMEOUT: Duration = Duration::from_secs(3);

/// Brackets the `env` output so anything the rc files print is ignored.
const MARKER: &[u8] = b"__TED_ENV__";

/// Variables describing the probe shell itself rather than the user's environment.
const SKIP: &[&[u8]] = &[b"PWD", b"OLDPWD", b"SHLVL", b"_", b"TERM"];

/// Copies the login shell's variables into this process when not started from a terminal.
/// Call first thing in `main`: it changes the process environment, which is only sound
/// while no other thread may be reading it.
pub fn import_if_launched_from_desktop() {
    if std::io::stdin().is_terminal() {
        return;
    }
    let Some(shell) = std::env::var_os("SHELL") else { return };
    let Some(output) = capture(&shell) else { return };
    let Some(env) = between_markers(&output) else { return };
    for entry in env.split(|&b| b == 0) {
        let Some(eq) = entry.iter().position(|&b| b == b'=') else { continue };
        let (key, value) = (&entry[..eq], &entry[eq + 1..]);
        if !key.is_empty() && !SKIP.contains(&key) {
            std::env::set_var(OsStr::from_bytes(key), OsStr::from_bytes(value));
        }
    }
}

/// Runs an interactive login shell (so both profile and rc files run) that prints its
/// environment between markers. `None` if the shell fails or exceeds `TIMEOUT`.
fn capture(shell: &OsStr) -> Option<Vec<u8>> {
    let marker = std::str::from_utf8(MARKER).expect("ASCII marker");
    let script = format!("printf {marker}; env -0; printf {marker}");
    let mut child = Command::new(shell)
        .args(["-l", "-i", "-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    // Reads up to the closing marker rather than EOF: daemons the rc files start (an ssh
    // agent) can hold the pipe open after the shell exits.
    std::thread::spawn(move || {
        let (mut out, mut chunk) = (Vec::new(), [0u8; 8192]);
        while let Ok(n @ 1..) = stdout.read(&mut chunk) {
            out.extend_from_slice(&chunk[..n]);
            if between_markers(&out).is_some() {
                break;
            }
        }
        let _ = tx.send(out);
    });
    let output = rx.recv_timeout(TIMEOUT).ok();
    let _ = child.kill();
    let _ = child.wait();
    output
}

fn between_markers(output: &[u8]) -> Option<&[u8]> {
    let find = |from: usize| output[from..].windows(MARKER.len()).position(|w| w == MARKER).map(|i| from + i);
    let start = find(0)? + MARKER.len();
    let end = find(start)?;
    Some(&output[start..end])
}
