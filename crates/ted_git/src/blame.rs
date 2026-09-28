//! In-place blame: `git-blame` annotates each chunk of a file's lines with the commit
//! that last changed them, in the buffer's margin. The buffer is read-only while blaming,
//! with `RET` (show the commit), `n` / `p` (next/previous chunk), `q` (stop) and `h` (these
//! keys) on top of its usual keys. Any change to the text ends the blame.

use std::collections::HashMap;
use std::path::PathBuf;

use ted_core::{Editor, FaceId, StyledText};

use crate::git::{args, git};
use crate::{diff, repo, GitFaces};

pub const KEYMAP: &str = "git-blame";

const HASH_COLS: usize = 7;
const AUTHOR_COLS: usize = 12;
/// `hash author yyyy-mm-dd `
const MARGIN_COLS: usize = HASH_COLS + 1 + AUTHOR_COLS + 1 + 10 + 1;
/// Git's name for lines that exist only in the working tree.
const UNCOMMITTED: &str = "0000000000000000000000000000000000000000";

struct Commit {
    hash: String,
    author: String,
    /// Author date as `yyyy-mm-dd`, in the author's time zone.
    date: String,
}

/// Blame of one version of a file: its commits and which one each line comes from.
struct Annotation {
    commits: Vec<Commit>,
    lines: Vec<usize>,
}

/// Buffer-local: the blame being shown, if any.
#[derive(Default)]
struct Blame(Option<Active>);

struct Active {
    root: PathBuf,
    annotation: Annotation,
    /// Restored when the blame ends.
    was_read_only: bool,
}

pub fn register(ed: &mut Editor) {
    let c = &mut ed.commands;
    c.register("git-blame", "Annotate the file's lines with the commits that last changed them (toggle)", |ed, _| {
        toggle(ed)
    });
    c.register("git-blame-visit", "Show the commit that last changed this line", |ed, _| visit(ed));
    c.register("git-blame-next-chunk", "Move to the next chunk of lines from one commit", |ed, _| step(ed, true));
    c.register("git-blame-previous-chunk", "Move to the previous chunk of lines from one commit", |ed, _| {
        step(ed, false)
    });
    c.register("git-blame-quit", "Stop blaming this buffer", |ed, _| quit(ed));
    ed.keymaps.ensure(KEYMAP);
    ed.bind_all(
        KEYMAP,
        &[
            ("RET", "git-blame-visit"),
            ("n", "git-blame-next-chunk"),
            ("p", "git-blame-previous-chunk"),
            ("q", "git-blame-quit"),
            ("h", "mode-help"),
        ],
    );
}

fn toggle(ed: &mut Editor) {
    if ed.active_buffer().local::<Blame>().is_some_and(|b| b.0.is_some()) {
        quit(ed);
        return;
    }
    let (Some(root), Some(path)) = (repo(ed), ed.active_buffer().path().map(|p| p.to_path_buf())) else {
        ed.set_status("Blame needs a file inside a git repository");
        return;
    };
    let Ok(relative) = path.strip_prefix(&root).map(|p| p.to_string_lossy().to_string()) else {
        ed.set_status("File is outside the repository");
        return;
    };
    let id = ed.active_buffer_id();
    let buf = &ed.buffers[id];
    // Blaming the buffer's text rather than the file keeps unsaved lines aligned.
    let (text, version) = (buf.text().to_string(), buf.version());
    let git_args = args(&["blame", "--porcelain", "--contents", "-", "--", &relative]);
    ed.set_status(format!("Blaming {}...", relative));
    ed.spawn(move |ctx| {
        let result = git(&root, &git_args, Some(&text)).map(|out| parse(&out));
        ctx.send(move |ed| {
            let annotation = match result {
                Ok(annotation) => annotation,
                Err(e) => return ed.set_status(format!("git blame failed: {}", e)),
            };
            let faces = *ed.ext_mut::<GitFaces>();
            let Some(buf) = ed.buffers.get_mut(id).filter(|b| b.version() == version) else {
                return ed.set_status("Buffer changed while blaming; try again");
            };
            buf.set_margin(MARGIN_COLS, margin(&annotation, &faces));
            let was_read_only = buf.is_read_only();
            buf.set_read_only(true);
            *buf.local_mut::<Blame>() = Blame(Some(Active { root, annotation, was_read_only }));
            if let Some(keymap) = ed.keymaps.id(KEYMAP) {
                ed.buffers[id].enable_keymap(keymap);
            }
            ed.set_status(format!("Blaming {} (RET show commit, n/p chunks, q quit)", relative));
        });
    });
}

fn quit(ed: &mut Editor) {
    let keymap = ed.keymaps.id(KEYMAP);
    let buf = ed.active_buffer_mut();
    let Some(active) = buf.local_mut::<Blame>().0.take() else { return };
    buf.clear_margin();
    buf.set_read_only(active.was_read_only);
    if let Some(keymap) = keymap {
        buf.disable_keymap(keymap);
    }
    ed.set_status("Blame ended");
}

/// The active buffer's blame, if it still describes the text; a blame the text has moved
/// away from ends here.
fn current(ed: &mut Editor) -> Option<&Active> {
    let buf = ed.active_buffer();
    buf.local::<Blame>()?.0.as_ref()?;
    if buf.margin().is_none() {
        quit(ed);
        ed.set_status("Buffer changed; blame ended");
        return None;
    }
    ed.active_buffer().local::<Blame>()?.0.as_ref()
}

fn visit(ed: &mut Editor) {
    let line = ed.active_buffer().char_to_line(ed.active_view().cursor.pos);
    let Some(active) = current(ed) else { return };
    let Some(&commit) = active.annotation.lines.get(line) else { return };
    let (root, hash) = (active.root.clone(), active.annotation.commits[commit].hash.clone());
    if hash == UNCOMMITTED {
        ed.set_status("Not committed yet");
    } else {
        diff::show(ed, root, diff::Source::Commit(hash));
    }
}

/// Moves to the first line of the next (or this/previous) chunk of lines from one commit.
fn step(ed: &mut Editor, forward: bool) {
    let line = ed.active_buffer().char_to_line(ed.active_view().cursor.pos);
    let Some(active) = current(ed) else { return };
    let lines = &active.annotation.lines;
    let starts_chunk = |l: usize| l == 0 || lines.get(l) != lines.get(l - 1);
    let target = if forward {
        (line + 1..lines.len()).find(|&l| starts_chunk(l))
    } else {
        (0..line).rev().find(|&l| starts_chunk(l))
    };
    if let Some(target) = target {
        let mut doc = ed.doc();
        let pos = doc.buf.line_to_char(target);
        doc.set_cursor(pos);
    }
}

/// The margin: commit details on the first line of each chunk, nothing on the rest.
fn margin(annotation: &Annotation, faces: &GitFaces) -> Vec<StyledText> {
    let mut previous = None;
    annotation
        .lines
        .iter()
        .map(|&c| {
            let mut text = StyledText::new();
            if previous.replace(c) != Some(c) {
                let commit = &annotation.commits[c];
                let author: String = commit.author.chars().take(AUTHOR_COLS).collect();
                text.push(&commit.hash[..HASH_COLS.min(commit.hash.len())], Some(faces.hash));
                text.push(&format!(" {:<w$} ", author, w = AUTHOR_COLS), Some(FaceId::DEFAULT));
                text.push(&commit.date, Some(FaceId::SHADOW));
            }
            text
        })
        .collect()
}

/// Reads `git blame --porcelain`: a header line `<hash> <orig> <final> [<count>]` per
/// line, commit details the first time a commit appears, then the line itself after a tab.
fn parse(output: &str) -> Annotation {
    let mut annotation = Annotation { commits: Vec::new(), lines: Vec::new() };
    let mut index: HashMap<&str, usize> = HashMap::new();
    let (mut current, mut time) = (0, 0i64);
    for line in output.lines() {
        if line.starts_with('\t') {
            annotation.lines.push(current);
            continue;
        }
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        let is_header = key.len() == 40 && key.bytes().all(|b| b.is_ascii_hexdigit());
        if is_header {
            current = *index.entry(key).or_insert_with(|| {
                let commit = Commit { hash: key.to_string(), author: String::new(), date: String::new() };
                annotation.commits.push(commit);
                annotation.commits.len() - 1
            });
            continue;
        }
        let commit = &mut annotation.commits[current];
        match key {
            "author" => commit.author = value.to_string(),
            "author-time" => time = value.parse().unwrap_or(0),
            // The time zone follows the time, so the date can be written now.
            "author-tz" => commit.date = date(time + parse_tz(value)),
            _ => {}
        }
    }
    annotation
}

/// `+0130` -> seconds east of UTC.
fn parse_tz(tz: &str) -> i64 {
    let sign = if tz.starts_with('-') { -1 } else { 1 };
    let digits: i64 = tz.trim_start_matches(['+', '-']).parse().unwrap_or(0);
    sign * ((digits / 100) * 3600 + (digits % 100) * 60)
}

/// `yyyy-mm-dd` for seconds since the epoch (proleptic Gregorian calendar).
fn date(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{:04}-{:02}-{:02}", year, month, day)
}
