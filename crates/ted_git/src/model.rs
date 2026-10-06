//! Repository state as shown in the status buffer, parsed from git's plumbing output.

use std::path::{Path, PathBuf};

use crate::git::{args, git};
use crate::renames;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    Untracked,
    Unstaged,
    Staged,
    Stashes,
    Recent,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Head {
    /// `None` when HEAD is detached.
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub subject: String,
    /// The rebase HEAD is stopped in, if any.
    pub rebase: Option<Rebase>,
    /// What the merge HEAD is stopped in brings in, if there is one: a branch at each
    /// merged commit, else its short hash.
    pub merging: Option<String>,
}

/// A rebase stopped partway, on a conflict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rebase {
    /// The branch being rebased; `None` when HEAD was detached.
    pub branch: Option<String>,
    /// A branch at the commit it is rebased onto, else that commit's short hash.
    pub onto: String,
    /// The commit it stopped on, counting from 1, and how many it applies.
    pub step: u32,
    pub steps: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub hash: String,
    /// Branches and tags pointing at the commit.
    pub refs: Vec<Ref>,
    pub subject: String,
    pub author: String,
    /// Relative, e.g. "3 days ago".
    pub date: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stash {
    /// `stash@{0}`
    pub name: String,
    pub subject: String,
}

/// `git stash list` arguments in the form `parse_stashes` reads.
pub fn stash_list_args() -> Vec<String> {
    args(&["stash", "list", "--format=%gd%x00%s"])
}

pub fn parse_stashes(output: &str) -> Vec<Stash> {
    output
        .lines()
        .filter_map(|line| {
            let (name, subject) = line.split_once('\0')?;
            Some(Stash { name: name.to_string(), subject: subject.to_string() })
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    /// HEAD itself, when detached.
    Head,
    /// The branch HEAD is on.
    Current,
    Local,
    Remote,
    Tag,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ref {
    /// Short name: `main`, `origin/main`, `v1.0`.
    pub name: String,
    pub kind: RefKind,
}

/// The `--format` fields `parse_commit` reads.
const COMMIT_FORMAT: &str = "%h%x00%D%x00%s%x00%an%x00%ar";

/// `git log` arguments for the latest `count` commits, in the form `parse_log` reads.
pub fn log_args(count: usize) -> Vec<String> {
    let format = format!("--format={}", COMMIT_FORMAT);
    vec!["log".into(), "--decorate=full".into(), format, "-n".into(), count.to_string()]
}

/// Commits from `git log` run with `log_args`.
pub fn parse_log(output: &str) -> Vec<Commit> {
    output.lines().filter_map(|line| parse_commit(&mut line.split('\0'))).collect()
}

/// A commit from the `COMMIT_FORMAT` fields.
fn parse_commit<'a>(fields: &mut impl Iterator<Item = &'a str>) -> Option<Commit> {
    let mut next = || fields.next().map(str::to_string);
    let hash = next()?;
    let refs = parse_refs(&next()?);
    Some(Commit { hash, refs, subject: next()?, author: next()?, date: next()? })
}

/// A commit as the header of its diff shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitDetails {
    pub commit: Commit,
    pub email: String,
    /// The author date, `yyyy-mm-dd hh:mm`.
    pub date: String,
    /// The message after the subject.
    pub body: String,
}

/// `git show` arguments for `rev`'s details, in the form `parse_details` reads.
pub fn details_args(rev: &str) -> Vec<String> {
    let format = format!("--format={}%x00%ae%x00%ad%x00%b", COMMIT_FORMAT);
    args(&["show", "--no-patch", "--decorate=full", "--date=format:%Y-%m-%d %H:%M", &format, rev])
}

pub fn parse_details(output: &str) -> Option<CommitDetails> {
    let mut fields = output.splitn(8, '\0');
    let commit = parse_commit(&mut fields)?;
    let (email, date) = (fields.next()?.to_string(), fields.next()?.to_string());
    Some(CommitDetails { commit, email, date, body: fields.next()?.trim_end().to_string() })
}

/// Refs from a full `%D` decoration: `HEAD -> refs/heads/main, refs/remotes/origin/main,
/// tag: refs/tags/v1`. Remote HEADs, the stash and notes are left out.
pub fn parse_refs(decoration: &str) -> Vec<Ref> {
    let short = |name: &str, kind| Ref { name: name.to_string(), kind };
    decoration
        .split(", ")
        .filter_map(|part| {
            if part == "HEAD" {
                return Some(short("HEAD", RefKind::Head));
            }
            if let Some(branch) = part.strip_prefix("HEAD -> refs/heads/") {
                return Some(short(branch, RefKind::Current));
            }
            if let Some(tag) = part.strip_prefix("tag: refs/tags/") {
                return Some(short(tag, RefKind::Tag));
            }
            if let Some(branch) = part.strip_prefix("refs/heads/") {
                return Some(short(branch, RefKind::Local));
            }
            let remote = part.strip_prefix("refs/remotes/").filter(|r| !r.ends_with("/HEAD"))?;
            Some(short(remote, RefKind::Remote))
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// The `@@ -a,b +c,d @@` line.
    pub header: String,
    pub lines: Vec<String>,
}

impl Hunk {
    /// First line number in the new file, from the header.
    pub fn new_start(&self) -> usize {
        self.header
            .split_whitespace()
            .find_map(|part| part.strip_prefix('+'))
            .and_then(|range| range.split(',').next())
            .and_then(|n| n.parse().ok())
            .unwrap_or(1)
    }

    /// Appends the hunk cut down to the changes `picked` chooses by line, as a patch
    /// applied forward or in `reverse` takes them alone: nothing, if it chooses none. A
    /// change left out is written as the side being patched has it: as context where that
    /// side has the line, not at all where it doesn't. The header keeps its line counts,
    /// which is what `git apply --recount` is for. Returns whether a change left out
    /// stays as context.
    fn write_picked(&self, reverse: bool, picked: impl Fn(usize) -> bool, out: &mut String) -> bool {
        // The sign of the changes whose lines the side being patched has.
        let present = if reverse { b'+' } else { b'-' };
        let start = out.len();
        let (mut any, mut stays, mut written) = (false, false, true);
        out.push_str(&self.header);
        out.push('\n');
        for (l, line) in self.lines.iter().enumerate() {
            let sign = line.as_bytes().first().copied();
            let change = matches!(sign, Some(b'+' | b'-'));
            // "\ No newline at end of file" goes with the line before it.
            let taken = if sign == Some(b'\\') { written } else { !change || picked(l) };
            written = taken || sign == Some(present);
            if !written {
                continue;
            }
            if taken {
                out.push_str(line);
                any |= change;
            } else {
                out.push(' ');
                out.push_str(&line[1..]);
                stays = true;
            }
            out.push('\n');
        }
        if !any {
            out.truncate(start);
        }
        any && stays
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    /// The path a renamed file had before.
    pub from: Option<String>,
    /// "modified", "new file", "deleted", "renamed" or "unmerged".
    pub kind: &'static str,
    /// Lines from `diff --git` up to the first hunk; needed to rebuild patches.
    pub header: Vec<String>,
    pub hunks: Vec<Hunk>,
    /// Lines added and removed across the hunks.
    pub added: usize,
    pub removed: usize,
    /// Set for a file git diffs as binary, which has no hunks.
    pub binary: Option<Binary>,
    /// The file's permissions before and after, when the change alters them.
    pub mode: Option<(u32, u32)>,
}

/// A binary file's size in bytes on each side of a change: `None` where there is no file
/// (it is new or deleted), or the size is not known.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Binary {
    pub old: Option<u64>,
    pub new: Option<u64>,
}

impl FileDiff {
    /// Every path the change touches: a renamed file's old path, then the path itself.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.from.as_deref().into_iter().chain([self.path.as_str()])
    }

    /// The lines a patch of the file starts with: the header of its change, or `in_place`
    /// that of a change to the file where it is now, which for a renamed file is under
    /// its new path.
    fn patch_header(&self, in_place: bool) -> String {
        if in_place {
            return format!("diff --git a/{0} b/{0}\n--- a/{0}\n+++ b/{0}\n", self.path);
        }
        let mut out = String::new();
        for line in &self.header {
            out.push_str(line);
            out.push('\n');
        }
        out
    }

    /// A patch `git apply` accepts for the whole file. A renamed file's patch renames it
    /// too.
    pub fn patch(&self) -> String {
        let mut out = self.patch_header(false);
        for hunk in &self.hunks {
            hunk.write_picked(false, |_| true, &mut out);
        }
        out
    }

    /// A patch of the changes `picked` chooses, by hunk and line of it, for `git apply
    /// --recount` to take alone, forward or in `reverse`: empty, if it chooses none.
    ///
    /// Applied forward, a renamed file's patch renames it too; in reverse it is a patch
    /// to the file in place, which takes the changes back and leaves the rename alone.
    /// So is any patch that leaves a change out as context: the file is then there on
    /// both sides, even where the whole change adds or deletes it.
    pub fn patch_picked(&self, reverse: bool, picked: impl Fn(usize, usize) -> bool) -> String {
        let mut hunks = String::new();
        let mut stays = false;
        for (h, hunk) in self.hunks.iter().enumerate() {
            stays |= hunk.write_picked(reverse, |l| picked(h, l), &mut hunks);
        }
        if hunks.is_empty() {
            return hunks;
        }
        let in_place = if self.from.is_some() { reverse } else { stays };
        let mut out = self.patch_header(in_place);
        out.push_str(&hunks);
        out
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub head: Head,
    /// Untracked paths; a directory holding nothing tracked is one entry ending in `/`.
    pub untracked: Vec<String>,
    pub unstaged: Vec<FileDiff>,
    pub staged: Vec<FileDiff>,
    pub stashes: Vec<Stash>,
    pub recent: Vec<Commit>,
}

impl Status {
    pub fn files(&self, section: Section) -> &[FileDiff] {
        match section {
            Section::Unstaged => &self.unstaged,
            Section::Staged => &self.staged,
            _ => &[],
        }
    }
}

/// Reads the full repository status. Runs several git commands; call from a job.
pub fn load(root: &Path) -> Result<Status, String> {
    let porcelain =
        git(root, &args(&["status", "--porcelain=v2", "--branch", "--untracked-files=normal", "-z"]), None)?;
    let (mut head, mut untracked) = parse_status(&porcelain);
    if let Some(git_dir) = git_dir(root) {
        head.rebase = load_rebase(root, &git_dir);
        head.merging = load_merge(root, &git_dir);
    }
    let unstaged = renames::worktree_diff(root, &diff_args(&[]), Some(&mut untracked))?;
    let staged = load_diff(root, &diff_args(&["--cached"]))?;
    // These fail in a repository without commits; that just means no history.
    let stashes = parse_stashes(&git(root, &stash_list_args(), None).unwrap_or_default());
    let recent = parse_log(&git(root, &log_args(10), None).unwrap_or_default());
    head.subject = recent.first().map(|c| c.subject.clone()).unwrap_or_default();
    Ok(Status { head, untracked, unstaged, staged, stashes, recent })
}

/// Where git keeps the state of the worktree at `root`: its `.git`, or the directory that
/// names when it is a file, as in a linked worktree. Read off the file system, since
/// every status refresh looks there for a stopped rebase or merge.
fn git_dir(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    match std::fs::read_to_string(&dot_git) {
        Ok(file) => file.strip_prefix("gitdir: ").map(|dir| root.join(dir.trim_end())),
        Err(_) => Some(dot_git),
    }
}

/// The rebase `root` is stopped in, read from the state git keeps for it.
fn load_rebase(root: &Path, git_dir: &Path) -> Option<Rebase> {
    // Where the merge and apply backends keep it, with their files for the step and the
    // number of steps.
    const STATES: [(&str, &str, &str); 2] = [("rebase-merge", "msgnum", "end"), ("rebase-apply", "next", "last")];
    let (dir, step, steps) =
        STATES.iter().map(|(dir, step, steps)| (git_dir.join(dir), step, steps)).find(|(dir, ..)| dir.is_dir())?;
    let read = |file: &str| std::fs::read_to_string(dir.join(file)).ok().map(|text| text.trim().to_string());
    // `git am` keeps its state in `rebase-apply` too, without an `onto`.
    let onto = branch_at(root, &read("onto")?)?;
    let branch = read("head-name")?.strip_prefix("refs/heads/").map(str::to_string);
    Some(Rebase { branch, onto, step: read(step)?.parse().ok()?, steps: read(steps)?.parse().ok()? })
}

/// What the merge `root` is stopped in brings in, from the heads git keeps for it.
fn load_merge(root: &Path, git_dir: &Path) -> Option<String> {
    let heads = std::fs::read_to_string(git_dir.join("MERGE_HEAD")).ok()?;
    Some(heads.lines().filter_map(|commit| branch_at(root, commit)).collect::<Vec<_>>().join(", "))
}

/// A branch at `commit`, local before remote, else the commit's short hash.
fn branch_at(root: &Path, commit: &str) -> Option<String> {
    let named = git(root, &args(&["log", "-1", "--decorate=full", "--format=%h%x00%D", commit]), None).ok()?;
    let (hash, decoration) = named.trim_end().split_once('\0')?;
    let refs = parse_refs(decoration);
    let at = |kind: RefKind| refs.iter().find(|r| r.kind == kind);
    Some(at(RefKind::Local).or_else(|| at(RefKind::Remote)).map_or(hash, |r| r.name.as_str()).to_string())
}

/// `git diff` arguments for `spec`: what to compare (`--cached`, a revision) and any
/// `-- <paths>` to limit it to.
pub fn diff_args(spec: &[&str]) -> Vec<String> {
    patch_args(&["diff"], spec)
}

/// Arguments for `command` to print the patch of `spec` as `parse_diff` reads it. Blob
/// ids are in full, so that one names its object whatever else the repository holds.
pub fn patch_args(command: &[&str], spec: &[&str]) -> Vec<String> {
    args(&[command, &["--no-ext-diff", "--full-index"], spec].concat())
}

/// The files the diff `args` print changes. Runs git; call from a job.
pub fn load_diff(root: &Path, args: &[String]) -> Result<Vec<FileDiff>, String> {
    let mut files = parse_diff(&git(root, args, None)?);
    measure_binaries(root, &mut files);
    Ok(files)
}

/// Fills in the sizes of the binary files in `files`, which a diff leaves out. Git knows
/// those of its objects; a side that is a file in the working tree at `root` is not one,
/// and is measured there. Runs git once, and only when there is a binary file.
pub fn measure_binaries(root: &Path, files: &mut [FileDiff]) {
    let mut ids = String::new();
    let mut binaries = Vec::new();
    for file in files.iter_mut().filter(|f| f.binary.is_some()) {
        let Some((old, new)) = blob_ids(&file.header) else { continue };
        ids.extend([old, "\n", new, "\n"]);
        binaries.push(file);
    }
    if binaries.is_empty() {
        return;
    }
    let Ok(sizes) = git(root, &args(&["cat-file", "--batch-check=%(objectsize)"]), Some(&ids)) else { return };
    // An id git has no object for gets a line that is not a size.
    let mut sizes = ids.lines().zip(sizes.lines()).map(|(id, size)| (id, size.parse::<u64>().ok()));
    for file in binaries {
        let (Some((_, old)), Some((id, new))) = (sizes.next(), sizes.next()) else { break };
        let new = match new {
            // All zeros stands for no file; any other id is the working tree file's.
            None if id.bytes().any(|b| b != b'0') => std::fs::metadata(root.join(&file.path)).ok().map(|m| m.len()),
            size => size,
        };
        file.binary = Some(Binary { old, new });
    }
}

/// The blobs a file's change is between, from its `index <old>..<new>` header line.
fn blob_ids(header: &[String]) -> Option<(&str, &str)> {
    let ids = header.iter().find_map(|l| l.strip_prefix("index "))?;
    ids.split(' ').next()?.split_once("..")
}

/// Branch info and untracked paths from `git status --porcelain=v2 --branch -z`.
pub fn parse_status(output: &str) -> (Head, Vec<String>) {
    let mut head = Head::default();
    let mut untracked = Vec::new();
    let mut entries = output.split('\0').filter(|e| !e.is_empty());
    while let Some(entry) = entries.next() {
        if let Some(branch) = entry.strip_prefix("# branch.head ") {
            head.branch = (branch != "(detached)").then(|| branch.to_string());
        } else if let Some(upstream) = entry.strip_prefix("# branch.upstream ") {
            head.upstream = Some(upstream.to_string());
        } else if let Some(ab) = entry.strip_prefix("# branch.ab ") {
            for part in ab.split_whitespace() {
                if let Some(n) = part.strip_prefix('+') {
                    head.ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = part.strip_prefix('-') {
                    head.behind = n.parse().unwrap_or(0);
                }
            }
        } else if let Some(path) = entry.strip_prefix("? ") {
            untracked.push(path.to_string());
        } else if entry.starts_with("2 ") {
            // Renames carry their original path as a separate entry.
            entries.next();
        }
    }
    (head, untracked)
}

/// Splits `git diff` output into files and hunks. Line endings inside lines (`\r`) are
/// kept so rebuilt patches apply byte-for-byte.
pub fn parse_diff(output: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    for raw in output.split_inclusive('\n') {
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        if line.starts_with("diff --git ") || line.starts_with("diff --cc ") {
            let kind = if line.starts_with("diff --cc ") { "unmerged" } else { "modified" };
            let header = vec![line.to_string()];
            let (path, from) = (String::new(), None);
            let (hunks, binary, mode) = (Vec::new(), None, None);
            files.push(FileDiff { path, from, kind, header, hunks, added: 0, removed: 0, binary, mode });
            continue;
        }
        let Some(file) = files.last_mut() else { continue };
        if line.starts_with("@@") {
            file.hunks.push(Hunk { header: line.to_string(), lines: Vec::new() });
        } else if let Some(hunk) = file.hunks.last_mut() {
            match line.as_bytes().first() {
                Some(b'+') => file.added += 1,
                Some(b'-') => file.removed += 1,
                _ => {}
            }
            hunk.lines.push(line.to_string());
        } else {
            file.header.push(line.to_string());
        }
    }
    for file in &mut files {
        file.path = path_of(&file.header);
        file.from = file.header.iter().find_map(|l| l.strip_prefix("rename from ")).map(str::to_string);
        if file.kind == "modified" {
            file.kind = kind_of(&file.header);
        }
        let is_binary = file.header.iter().any(|l| l.starts_with("Binary files "));
        file.binary = is_binary.then(Binary::default);
        file.mode = mode_change(&file.header);
    }
    files
}

/// The permissions a change takes a file from and to, from its `old mode` and `new mode`
/// header lines.
fn mode_change(header: &[String]) -> Option<(u32, u32)> {
    let permissions = |prefix: &str| {
        let mode = header.iter().find_map(|l| l.strip_prefix(prefix))?;
        Some(u32::from_str_radix(mode, 8).ok()? & 0o7777)
    };
    Some((permissions("old mode ")?, permissions("new mode ")?))
}

fn path_of(header: &[String]) -> String {
    let find = |prefix: &str| header.iter().find_map(|l| l.strip_prefix(prefix)).filter(|p| *p != "/dev/null");
    if let Some(path) = find("+++ b/").or_else(|| find("--- a/")).or_else(|| find("rename to ")) {
        return path.to_string();
    }
    // Binary or mode-only changes: "diff --git a/x b/x".
    header[0].rsplit_once(" b/").map(|(_, p)| p.to_string()).unwrap_or_else(|| header[0].clone())
}

fn kind_of(header: &[String]) -> &'static str {
    let has = |prefix: &str| header.iter().any(|l| l.starts_with(prefix));
    if has("new file mode") {
        "new file"
    } else if has("deleted file mode") {
        "deleted"
    } else if has("rename from") {
        "renamed"
    } else {
        "modified"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_round_trips_to_patches() {
        let diff = "diff --git a/src/a.rs b/src/a.rs\nindex 1..2 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,2 +1,2 @@\n-old\n+new\n ctx\r\n@@ -10 +10,2 @@ fn f()\n x\n+y\ndiff --git a/new.txt b/new.txt\nnew file mode 100644\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+hi\n";
        let files = parse_diff(diff);
        assert_eq!(files.len(), 2);
        assert_eq!((files[0].path.as_str(), files[0].kind), ("src/a.rs", "modified"));
        assert_eq!((files[1].path.as_str(), files[1].kind), ("new.txt", "new file"));
        assert_eq!(files[0].hunks.len(), 2);
        assert_eq!(files[0].hunks[1].new_start(), 10);
        assert_eq!(files[0].hunks[0].lines[2], " ctx\r", "CR is preserved");

        let first_file = diff.split("diff --git a/new.txt").next().unwrap();
        assert_eq!(files[0].patch(), first_file);
        assert!(files[0].patch_picked(false, |hunk, _| hunk == 1).ends_with("@@ -10 +10,2 @@ fn f()\n x\n+y\n"));
    }

    #[test]
    fn log_parses_ref_badges() {
        let out = "abc1234\0HEAD -> refs/heads/feature/x, refs/remotes/origin/feature/x, refs/remotes/origin/HEAD, tag: refs/tags/v1, refs/stash\0Subject\0Me\x002 days ago\n\
                   def5678\0\0Older\0You\x003 days ago\n";
        let commits = parse_log(out);
        assert_eq!(commits.len(), 2);
        let kinds: Vec<_> = commits[0].refs.iter().map(|r| (r.name.as_str(), r.kind)).collect();
        assert_eq!(
            kinds,
            [("feature/x", RefKind::Current), ("origin/feature/x", RefKind::Remote), ("v1", RefKind::Tag)]
        );
        assert_eq!((commits[0].subject.as_str(), commits[0].date.as_str()), ("Subject", "2 days ago"));
        assert!(commits[1].refs.is_empty());
        assert_eq!(parse_refs("HEAD, refs/heads/main")[0].kind, RefKind::Head);
    }

    #[test]
    fn status_parses_branch_and_untracked() {
        let out = "# branch.oid abc\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -1\0\
                   2 R. N... 100644 100644 100644 a b R100 new.rs\0old.rs\0? notes.md\0";
        let (head, untracked) = parse_status(out);
        assert_eq!(head.branch.as_deref(), Some("main"));
        assert_eq!(head.upstream.as_deref(), Some("origin/main"));
        assert_eq!((head.ahead, head.behind), (2, 1));
        assert_eq!(untracked, vec!["notes.md"]);
    }
}
