//! Repository state as shown in the status buffer, parsed from git's plumbing output.

use std::path::Path;

use crate::git::{args, git};

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

/// `git log` arguments for the latest `count` commits, in the form `parse_log` reads.
pub fn log_args(count: usize) -> Vec<String> {
    let format = "--format=%h%x00%D%x00%s%x00%an%x00%ar";
    vec!["log".into(), "--decorate=full".into(), format.into(), "-n".into(), count.to_string()]
}

/// Commits from `git log` run with `log_args`.
pub fn parse_log(output: &str) -> Vec<Commit> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\0');
            let mut next = || fields.next().map(str::to_string);
            let hash = next()?;
            let refs = parse_refs(&next()?);
            Some(Commit { hash, refs, subject: next()?, author: next()?, date: next()? })
        })
        .collect()
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    /// "modified", "new file", "deleted", "renamed" or "unmerged".
    pub kind: &'static str,
    /// Lines from `diff --git` up to the first hunk; needed to rebuild patches.
    pub header: Vec<String>,
    pub hunks: Vec<Hunk>,
}

impl FileDiff {
    /// A patch `git apply` accepts, for one hunk or (with `None`) the whole file.
    pub fn patch(&self, hunk: Option<usize>) -> String {
        let mut out = String::new();
        for line in &self.header {
            out.push_str(line);
            out.push('\n');
        }
        let hunks: Box<dyn Iterator<Item = &Hunk>> = match hunk {
            Some(i) => Box::new(self.hunks.get(i).into_iter()),
            None => Box::new(self.hunks.iter()),
        };
        for hunk in hunks {
            out.push_str(&hunk.header);
            out.push('\n');
            for line in &hunk.lines {
                out.push_str(line);
                out.push('\n');
            }
        }
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
    let (mut head, untracked) = parse_status(&porcelain);
    let unstaged = parse_diff(&git(root, &args(&["diff", "--no-ext-diff"]), None)?);
    let staged = parse_diff(&git(root, &args(&["diff", "--cached", "--no-ext-diff"]), None)?);
    // These fail in a repository without commits; that just means no history.
    let stashes = parse_stashes(&git(root, &stash_list_args(), None).unwrap_or_default());
    let recent = parse_log(&git(root, &log_args(10), None).unwrap_or_default());
    head.subject = recent.first().map(|c| c.subject.clone()).unwrap_or_default();
    Ok(Status { head, untracked, unstaged, staged, stashes, recent })
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
            files.push(FileDiff { path: String::new(), kind, header: vec![line.to_string()], hunks: Vec::new() });
            continue;
        }
        let Some(file) = files.last_mut() else { continue };
        if line.starts_with("@@") {
            file.hunks.push(Hunk { header: line.to_string(), lines: Vec::new() });
        } else if let Some(hunk) = file.hunks.last_mut() {
            hunk.lines.push(line.to_string());
        } else {
            file.header.push(line.to_string());
        }
    }
    for file in &mut files {
        file.path = path_of(&file.header);
        if file.kind == "modified" {
            file.kind = kind_of(&file.header);
        }
    }
    files
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
        assert_eq!(files[0].patch(None), first_file);
        assert!(files[0].patch(Some(1)).ends_with("@@ -10 +10,2 @@ fn f()\n x\n+y\n"));
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
