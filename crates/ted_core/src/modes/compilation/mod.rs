//! Compilation: runs a build command at the project root, streaming its output into a
//! read-only `compilation` buffer. Errors and warnings are highlighted and form the
//! buffer's location list (see `locations`): RET visits one, `n` / `p` move between them
//! and `next-error` steps through them from any buffer. `g` reruns the command, `C-c C-k`
//! kills it, and starting a build while one runs replaces it.
//!
//! The job thread reads the process's merged stdout/stderr and parses it (`parse`); the
//! UI thread only appends finished batches of text, decorations and entries.

mod parse;
mod process;

use std::io::{self, Read};
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Instant;

use crate::buffer::{Buffer, BufferId, Decoration, StyledText};
use crate::editor::Editor;
use crate::face::FaceId;
use crate::jobs::{JobContext, JobHandle};
use crate::locations::{self, LocationList, Severity};
use crate::settings::Setting;
use crate::text::collapse_tilde;

use parse::{Batch, OutputParser};
use process::Process;

pub const BUFFER_NAME: &str = "compilation";
pub const MODE: &str = "Compilation";
const DECORATIONS: &str = "compilation";

#[derive(Default)]
struct Compilation {
    command: String,
    dir: PathBuf,
    run: Option<Run>,
}

struct Run {
    job: JobHandle,
    pid: u32,
    started: Instant,
}

pub fn register(ed: &mut Editor) {
    let default_command =
        ed.settings.define::<String>("compile.command", "make", "Build command `compile` offers first");
    let c = &mut ed.commands;
    c.register("compile", "Run a build command at the project root (prompts unless given one)", move |ed, arg| {
        // From the compilation buffer itself, stay in the directory of the last build.
        let dir = match ed.active_buffer().local::<Compilation>() {
            Some(state) => state.dir.clone(),
            None => ed.project().root,
        };
        if let Some(command) = arg.str() {
            run(ed, command.to_string(), dir);
            return;
        }
        let label = format!("Compile in {}: ", collapse_tilde(&dir));
        let initial = last_run(ed, default_command).0;
        ed.prompt("compile", label, initial, move |ed, command| {
            if !command.is_empty() {
                run(ed, command, dir);
            }
        });
    });
    c.register("recompile", "Run the last build command again, in the same directory", move |ed, _| {
        let (command, dir) = last_run(ed, default_command);
        run(ed, command, dir);
    });
    c.register("kill-compilation", "Stop the running build", |ed, _| {
        let Some(id) = ed.buffers.find(is_compilation) else {
            ed.set_status("No compilation");
            return;
        };
        match stop(ed, id) {
            Some(run) => {
                let text = format!("Compilation stopped after {:.2}s", run.started.elapsed().as_secs_f64());
                append(ed, id, Batch::footer(&text, Some(FaceId::ERROR)));
                ed.set_status(text);
            }
            None => ed.set_status("No build is running"),
        }
    });

    ed.define_mode(locations::list_mode(MODE).revert("recompile"));
    ed.bind_all("compilation", &[("C-c C-k", "kill-compilation")]);
}

fn is_compilation(buf: &Buffer) -> bool {
    buf.path().is_none() && buf.mode().name == MODE
}

/// The last build's command and directory, else the configured command at the project root.
fn last_run(ed: &Editor, default_command: Setting<String>) -> (String, PathBuf) {
    let last = ed.buffers.find(is_compilation).and_then(|id| ed.buffers[id].local::<Compilation>());
    match last.filter(|state| !state.command.is_empty()) {
        Some(state) => (state.command.clone(), state.dir.clone()),
        None => (ed.settings.get(default_command).to_string(), ed.project().root),
    }
}

/// Starts `command` in `dir`, replacing any running build and the previous output.
fn run(ed: &mut Editor, command: String, dir: PathBuf) {
    let id = ed.special_buffer(BUFFER_NAME, MODE);
    stop(ed, id);

    let mut header = StyledText::new();
    header.line(&[(&collapse_tilde(&dir), Some(FaceId::HEADING)), ("  $ ", Some(FaceId::SHADOW)), (&command, None)]);
    header.line(&[]);
    let buf = &mut ed.buffers[id];
    buf.set_styled(DECORATIONS, header);
    buf.set_directory(&dir);
    locations::clear(buf);
    let end = buf.len_chars();
    ed.show_buffer(id);
    // Cursors at the end follow the output as it arrives.
    for view in ed.layout.views_showing(id) {
        view.reset();
        view.goto(end);
    }
    locations::set_current(ed, id);

    let run = match process::spawn(&command, &dir) {
        Ok(process) => {
            let pid = process.child.id();
            let job_dir = dir.clone();
            let job = ed.spawn(move |ctx| stream(&ctx, id, process, job_dir));
            ed.set_status(format!("Compiling: {}", command));
            Some(Run { job, pid, started: Instant::now() })
        }
        Err(e) => {
            let text = format!("Failed to run '{}': {}", command, e);
            append(ed, id, Batch::footer(&text, Some(FaceId::ERROR)));
            ed.set_status(text);
            None
        }
    };
    *ed.buffers[id].local_mut::<Compilation>() = Compilation { command, dir, run };
}

/// Stops the build running in `id`, if any: its process group is terminated and output
/// still in flight is dropped.
fn stop(ed: &mut Editor, id: BufferId) -> Option<Run> {
    let run = ed.buffers[id].local_mut::<Compilation>().run.take()?;
    run.job.cancel();
    process::terminate(run.pid);
    Some(run)
}

/// Job thread: parses output as it arrives and sends it on in batches, one per read, so
/// a chatty build costs the UI a few appends rather than one per line.
fn stream(ctx: &JobContext, id: BufferId, mut process: Process, dir: PathBuf) {
    let mut parser = OutputParser::new(dir);
    let mut chunk = vec![0; 64 * 1024];
    loop {
        let n = match process.output.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let mut batch = Batch::default();
        parser.feed(&chunk[..n], &mut batch);
        if !batch.is_empty() && !ctx.send(move |ed| append(ed, id, batch)) {
            break;
        }
    }
    let mut batch = Batch::default();
    parser.finish(&mut batch);
    let status = process.child.wait();
    ctx.send(move |ed| {
        append(ed, id, batch);
        finish(ed, id, status);
    });
}

/// Appends `batch` to buffer `id`: text, decorations and location entries. Views whose
/// cursor was at the end keep following the output.
fn append(ed: &mut Editor, id: BufferId, batch: Batch) {
    let Some(buf) = ed.buffers.get_mut(id).filter(|_| !batch.is_empty()) else {
        return;
    };
    let old_len = buf.len_chars();
    let first_line = buf.len_lines() - 1;
    let first_byte = buf.text().len_bytes();
    buf.append_generated(&batch.text);

    let text = buf.text().clone();
    let char_at = |byte: usize| text.byte_to_char(first_byte + byte);
    for span in &batch.spans {
        let range = char_at(span.range.start)..char_at(span.range.end);
        buf.decorations_mut().add(DECORATIONS, Decoration::new(range, span.face));
    }
    for mut entry in batch.entries {
        entry.line += first_line;
        locations::push(buf, entry);
    }

    let new_len = buf.len_chars();
    for view in ed.layout.views_showing(id).filter(|v| v.cursor.pos == old_len) {
        view.cursor.pos = new_len;
    }
}

fn finish(ed: &mut Editor, id: BufferId, status: io::Result<ExitStatus>) {
    let Some(run) = ed.buffers.get_mut(id).and_then(|buf| buf.local_mut::<Compilation>().run.take()) else {
        return;
    };
    let secs = run.started.elapsed().as_secs_f64();
    let (text, face) = match status {
        Ok(s) if s.success() => (format!("Compilation finished in {:.2}s", secs), None),
        Ok(s) => match s.code() {
            Some(code) => {
                (format!("Compilation failed with exit code {} after {:.2}s", code, secs), Some(FaceId::ERROR))
            }
            None => (format!("Compilation terminated by a signal after {:.2}s", secs), Some(FaceId::ERROR)),
        },
        Err(e) => (format!("Compilation failed: {}", e), Some(FaceId::ERROR)),
    };
    append(ed, id, Batch::footer(&text, face));

    let list = ed.buffers[id].local::<LocationList>();
    let count = |severity| list.map_or(0, |l| l.count(severity));
    let (errors, warnings) = (count(Severity::Error), count(Severity::Warning));
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    ed.set_status(format!("{} ({} error{}, {} warning{})", text, errors, plural(errors), warnings, plural(warnings)));
}
