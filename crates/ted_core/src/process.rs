//! Running programs. Everything the editor starts (builds, git, language servers, project
//! file listings, terminals) is described as a `Program` and started from here, so where
//! and how programs run is decided in one place.

use std::io::{self, PipeReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// A program to run: its arguments, working directory and environment additions.
#[derive(Debug, Clone)]
pub struct Program {
    pub program: String,
    pub args: Vec<String>,
    pub dir: PathBuf,
    pub env: Vec<(String, String)>,
}

/// Everything a finished program printed.
#[derive(Debug, Clone)]
pub struct Output {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    /// Stdout if the program succeeded, else what it said went wrong.
    pub fn into_result(self) -> Result<String, String> {
        if self.ok {
            return Ok(self.stdout);
        }
        let message = if self.stderr.trim().is_empty() { self.stdout } else { self.stderr };
        Err(message.trim().to_string())
    }
}

/// A program talking over its stdin and stdout (a language server).
pub struct Piped {
    pub child: Child,
    pub stdin: ChildStdin,
    pub stdout: ChildStdout,
}

/// A program whose stdout and stderr share one pipe, so output keeps the order it was
/// written in (a build).
pub struct Merged {
    pub child: Child,
    pub output: PipeReader,
}

impl Program {
    pub fn new(program: impl Into<String>, dir: impl Into<PathBuf>) -> Self {
        Self { program: program.into(), args: Vec::new(), dir: dir.into(), env: Vec::new() }
    }

    /// `command` run by the shell.
    pub fn shell(command: &str, dir: impl Into<PathBuf>) -> Self {
        let (shell, flag) = if cfg!(windows) { ("cmd", "/C") } else { ("sh", "-c") };
        Self::new(shell, dir).args([flag, command])
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.to_string(), value.to_string()));
        self
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args).current_dir(&self.dir).envs(self.env.iter().map(|(k, v)| (k, v)));
        cmd
    }

    /// Runs the program to completion, feeding it `stdin`. This blocks, so call it from a
    /// job. A program that can't start fails with the reason as its error output.
    pub fn output(&self, stdin: Option<&[u8]>) -> Output {
        let failed = |e: io::Error| Output {
            ok: false,
            stdout: String::new(),
            stderr: format!("cannot run {}: {}", self.program, e),
        };
        let mut cmd = self.command();
        cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => return failed(e),
        };
        if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
            if let Err(e) = pipe.write_all(input) {
                return failed(e);
            }
        }
        match child.wait_with_output() {
            Ok(out) => Output {
                ok: out.status.success(),
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            },
            Err(e) => failed(e),
        }
    }

    /// Starts the program with piped stdin and stdout; its stderr is dropped.
    pub fn spawn_piped(&self) -> io::Result<Piped> {
        let mut child = self.command().stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?;
        match (child.stdin.take(), child.stdout.take()) {
            (Some(stdin), Some(stdout)) => Ok(Piped { child, stdin, stdout }),
            _ => {
                let _ = child.kill();
                Err(io::Error::other("pipes unavailable"))
            }
        }
    }

    /// Starts the program in its own process group (see `terminate_group`), with stdout and
    /// stderr merged into one pipe and no stdin.
    pub fn spawn_merged(&self) -> io::Result<Merged> {
        let (output, writer) = io::pipe()?;
        let mut cmd = self.command();
        cmd.stdin(Stdio::null()).stdout(writer.try_clone()?).stderr(writer);
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        let child = cmd.spawn()?;
        // `cmd` holds the pipe's write ends; the reader only sees EOF once they are closed.
        drop(cmd);
        Ok(Merged { child, output })
    }
}

/// Stops a program started with `spawn_merged` as `pid`, along with everything it started
/// (make's compilers, test binaries): it leads its own process group.
pub fn terminate_group(pid: u32) {
    #[cfg(unix)]
    // SAFETY: kill(2) takes no pointers; a negative pid signals the whole process group.
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGTERM);
    }
    #[cfg(not(unix))]
    let _ = pid;
}
