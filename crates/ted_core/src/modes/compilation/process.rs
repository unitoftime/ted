//! The build process: a shell command in its own process group, with stdout and stderr
//! sharing one pipe so output keeps the order the program wrote it in.

use std::io::{self, PipeReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};

pub struct Process {
    pub child: Child,
    pub output: PipeReader,
}

pub fn spawn(command: &str, dir: &Path) -> io::Result<Process> {
    let (output, writer) = io::pipe()?;
    let (program, flag) = if cfg!(windows) { ("cmd", "/C") } else { ("sh", "-c") };
    let mut cmd = Command::new(program);
    cmd.args([flag, command])
        .current_dir(dir)
        .env("TERM", "dumb")
        .stdin(Stdio::null())
        .stdout(writer.try_clone()?)
        .stderr(writer);
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let child = cmd.spawn()?;
    // `cmd` holds the pipe's write ends; the reader only sees EOF once they are closed.
    drop(cmd);
    Ok(Process { child, output })
}

/// Stops the build started as `pid` by `spawn`, along with everything it started (make's
/// compilers, test binaries): `spawn` made it the leader of its own process group.
pub fn terminate(pid: u32) {
    #[cfg(unix)]
    // SAFETY: kill(2) takes no pointers; a negative pid signals the whole process group.
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGTERM);
    }
    #[cfg(not(unix))]
    let _ = pid;
}
