//! Menus for commit, push, pull, fetch, branch, tag, merge, rebase, stash and reset operations.

use std::path::{Path, PathBuf};

use ted_core::process::Output;
use ted_core::{Editor, Menu, PickerItem};

use crate::git::{args, git, git_output};
use crate::diff::{self, Source};
use crate::{commit, log, model, process, status};

fn run(root: &Path, list: &'static [&'static str], done: &'static str) -> impl FnOnce(&mut Editor) {
    let root = root.to_path_buf();
    move |ed| process::run(ed, root, args(list), None, done, |_| {})
}

pub fn commit(ed: &mut Editor, root: PathBuf) {
    let (r1, r2) = (root.clone(), root.clone());
    let menu = Menu::new("git-commit", "Commit")
        .group("Create")
        .entry('c', "Commit", move |ed| commit::start(ed, r1, false))
        .group("Edit HEAD")
        .entry('a', "Amend", move |ed| commit::start(ed, r2, true))
        .entry('e', "Extend (keep message)", run(&root, &["commit", "--amend", "--no-edit"], "Extended HEAD"));
    ed.push_modal(menu);
}

pub fn diff(ed: &mut Editor, root: PathBuf) {
    let show = |source: Source| {
        let root = root.clone();
        move |ed: &mut Editor| diff::show(ed, root, source)
    };
    let prompt = |id: &'static str, label: &'static str, initial: &'static str, source: fn(String) -> Source| {
        let root = root.clone();
        move |ed: &mut Editor| {
            ed.prompt(id, label, initial, move |ed, input| {
                if !input.is_empty() {
                    diff::show(ed, root, source(input));
                }
            })
        }
    };
    let dwim_root = root.clone();
    let menu = Menu::new("git-diff", "Diff")
        .group("Diff")
        .entry('d', "Dwim (thing at point)", move |ed| diff::dwim(ed, dwim_root))
        .entry('u', "Unstaged", show(Source::Unstaged))
        .entry('s', "Staged", show(Source::Staged))
        .entry('w', "Worktree (vs HEAD)", show(Source::Worktree))
        .group("Compare")
        .entry('r', "Range (A..B)", prompt("git-diff-range", "Diff range: ", "", Source::Range))
        .entry('c', "Show commit", prompt("git-show-commit", "Show commit: ", "HEAD", Source::Commit));
    ed.push_modal(menu);
}

pub fn push(ed: &mut Editor, root: PathBuf) {
    let tag_root = root.clone();
    let menu = Menu::new("git-push", "Push")
        .group("Push")
        .entry('u', "Push to upstream", run(&root, &["push"], "Pushed"))
        .entry('s', "Push and set upstream", run(&root, &["push", "-u", "origin", "HEAD"], "Pushed"))
        .group("Tags")
        .entry('t', "Push a tag to origin", move |ed| push_tag(ed, tag_root))
        .entry('T', "Push all tags", run(&root, &["push", "--tags"], "Pushed all tags"))
        .group("Force")
        .entry('f', "Force push (with lease)", run(&root, &["push", "--force-with-lease"], "Force pushed"));
    ed.push_modal(menu);
}

pub fn pull(ed: &mut Editor, root: PathBuf) {
    let menu = Menu::new("git-pull", "Pull")
        .group("Pull")
        .entry('u', "Pull from upstream", run(&root, &["pull"], "Pulled"))
        .entry('r', "Pull with rebase", run(&root, &["pull", "--rebase"], "Pulled"));
    ed.push_modal(menu);
}

pub fn fetch(ed: &mut Editor, root: PathBuf) {
    let menu = Menu::new("git-fetch", "Fetch")
        .group("Fetch")
        .entry('f', "Fetch upstream", run(&root, &["fetch"], "Fetched"))
        .entry('a', "Fetch all remotes (prune)", run(&root, &["fetch", "--all", "--prune"], "Fetched"));
    ed.push_modal(menu);
}

pub fn branch(ed: &mut Editor, root: PathBuf) {
    let (r1, r2, r3) = (root.clone(), root.clone(), root);
    let menu = Menu::new("git-branch", "Branch")
        .group("Checkout")
        .entry('b', "Checkout branch", move |ed| checkout(ed, r1))
        .entry('c', "Create and checkout", move |ed| create(ed, r2))
        .group("Delete")
        .entry('k', "Delete branch", move |ed| delete(ed, r3, "branch", &["refs/heads"]));
    ed.push_modal(menu);
}

pub fn tag(ed: &mut Editor, root: PathBuf) {
    let create = |annotated: bool| {
        let root = root.clone();
        move |ed: &mut Editor| tag_commit(ed, root, annotated)
    };
    let delete_root = root.clone();
    let menu = Menu::new("git-tag", "Tag")
        .group("Tag the commit at point, else HEAD")
        .entry('t', "Lightweight", create(false))
        .entry('a', "Annotated (with a message)", create(true))
        .group("Delete")
        .entry('k', "Delete tag", move |ed| delete(ed, delete_root, "tag", &["refs/tags"]));
    ed.push_modal(menu);
}

pub fn merge(ed: &mut Editor, root: PathBuf) {
    let merge = |options: &'static [&'static str]| {
        let root = root.clone();
        move |ed: &mut Editor| merge_branch(ed, root, options)
    };
    let abort_root = root.clone();
    let menu = Menu::new("git-merge", "Merge")
        .group("Merge into the current branch")
        .entry('m', "A branch", merge(&[]))
        .entry('n', "A branch, always with a merge commit", merge(&["--no-ff"]))
        .group("Stopped merge")
        .entry('c', "Continue", run(&root, &["merge", "--continue"], "Merged"))
        .entry('a', "Abort", move |ed| abort(ed, abort_root, "merge"));
    ed.push_modal(menu);
}

pub fn rebase(ed: &mut Editor, root: PathBuf) {
    let (r1, r2) = (root.clone(), root.clone());
    let menu = Menu::new("git-rebase", "Rebase")
        .group("Rebase onto")
        .entry('u', "Upstream", run(&root, &["rebase"], "Rebased"))
        .entry('e', "Another branch", move |ed| rebase_onto(ed, r1))
        .group("Stopped rebase")
        .entry('r', "Continue", run(&root, &["rebase", "--continue"], "Rebased"))
        .entry('s', "Skip this commit", run(&root, &["rebase", "--skip"], "Rebased"))
        .entry('a', "Abort", move |ed| abort(ed, r2, "rebase"));
    ed.push_modal(menu);
}

pub fn stash(ed: &mut Editor, root: PathBuf) {
    let (r1, r2, r3, r4, r5, r6) = (root.clone(), root.clone(), root.clone(), root.clone(), root.clone(), root);
    let menu = Menu::new("git-stash", "Stash")
        .group("Stash")
        .entry('z', "Both", move |ed| stash_push(ed, r1, Changes::Both))
        .entry('s', "Staged", move |ed| stash_push(ed, r2, Changes::Staged))
        .entry('u', "Unstaged", move |ed| stash_push(ed, r3, Changes::Unstaged))
        .group("Use")
        .entry('a', "Apply", move |ed| with_stash(ed, r4, "Apply", |ed, root, name| use_stash(ed, root, "apply", name)))
        .entry('p', "Pop", move |ed| with_stash(ed, r5, "Pop", |ed, root, name| use_stash(ed, root, "pop", name)))
        .entry('k', "Drop", move |ed| with_stash(ed, r6, "Drop", drop_stash));
    ed.push_modal(menu);
}

pub fn reset(ed: &mut Editor, root: PathBuf) {
    let entry = |mode: &'static str| {
        let root = root.clone();
        move |ed: &mut Editor| reset_to(ed, root, mode)
    };
    let menu = Menu::new("git-reset", "Reset")
        .group("Reset HEAD to a commit")
        .entry('m', "Mixed (keep worktree, reset index)", entry("mixed"))
        .entry('s', "Soft (keep worktree and index)", entry("soft"))
        .entry('k', "Keep (keep uncommitted changes)", entry("keep"))
        .entry('h', "Hard (discard all changes)", entry("hard"));
    ed.push_modal(menu);
}

/// The commit at point in a status or log buffer, else HEAD.
fn commit_at_point(ed: &Editor) -> String {
    status::commit_at_point(ed).or_else(|| log::commit_at_point(ed)).unwrap_or_else(|| "HEAD".into())
}

/// Resets HEAD to a prompted commit, defaulting to the one at point, with `git reset
/// --<mode>`. A hard reset asks first, since it discards uncommitted changes.
fn reset_to(ed: &mut Editor, root: PathBuf, mode: &'static str) {
    let initial = commit_at_point(ed);
    ed.prompt("git-reset", format!("Reset ({}) to: ", mode), initial, move |ed, target| {
        if target.is_empty() {
            return;
        }
        let label = format!("Hard reset to {}, discarding all uncommitted changes? (y/n) ", target);
        let run = move |ed: &mut Editor| {
            let done = format!("Reset to {}", target);
            process::run(ed, root, args(&["reset", &format!("--{}", mode), &target]), None, &done, |_| {});
        };
        if mode != "hard" {
            return run(ed);
        }
        ed.confirm("git-reset-hard", label, move |ed, yes| match yes {
            true => run(ed),
            false => ed.set_status("Reset cancelled"),
        });
    });
}

/// Which changes to tracked files a stash takes.
#[derive(Clone, Copy)]
enum Changes {
    Both,
    Staged,
    Unstaged,
}

/// Stashes `changes`, with a message if one is given.
fn stash_push(ed: &mut Editor, root: PathBuf, changes: Changes) {
    ed.prompt("git-stash-message", "Stash message (optional): ", "", move |ed, message| {
        if let Changes::Unstaged = changes {
            let work = move |root: &Path| match stash_unstaged(root, &message) {
                Ok(()) => Output { ok: true, stdout: String::new(), stderr: String::new() },
                Err(stderr) => Output { ok: false, stdout: String::new(), stderr },
            };
            return process::run_work(ed, root, "git stash (unstaged changes)".to_string(), "Stashed", work, |_| {});
        }
        let mut git_args = args(&["stash", "push"]);
        if let Changes::Staged = changes {
            git_args.push("--staged".to_string());
        }
        if !message.is_empty() {
            git_args.extend(["-m".to_string(), message]);
        }
        process::run(ed, root, git_args, None, "Stashed", |_| {});
    });
}

/// Stashes only the changes the worktree has over the index, which git can't do alone:
/// the stash `git stash create` makes is stored on a base commit holding the index
/// instead of on HEAD, so showing or applying it gives just those changes. The worktree
/// then goes back to the index, once the stash is safely stored. Blocks; runs in a job.
fn stash_unstaged(root: &Path, message: &str) -> Result<(), String> {
    let run = |list: &[&str]| git(root, &args(list), None);
    if git_output(root, &args(&["diff", "--quiet"]), None).ok {
        return Err("No unstaged changes to stash".to_string());
    }
    let mut create = vec!["stash", "create"];
    if !message.is_empty() {
        create.push(message);
    }
    let stash = run(&create)?;
    // Its parents (HEAD, then the index's commit), tree and message.
    let info = run(&["show", "-s", "--format=%P%x00%T%x00%s", stash.trim()])?;
    let unexpected = || Err(format!("Unexpected stash commit: {}", info.trim()));
    let mut fields = info.trim_end().split('\0');
    let (Some(parents), Some(tree), Some(subject)) = (fields.next(), fields.next(), fields.next()) else {
        return unexpected();
    };
    let mut parents = parents.split_whitespace();
    let (Some(head), Some(index)) = (parents.next(), parents.next()) else {
        return unexpected();
    };
    // A stash's parents must differ, so the base is a new commit rather than the index's.
    let base = run(&["commit-tree", &format!("{}^{{tree}}", index), "-p", head, "-m", subject])?;
    let commit = run(&["commit-tree", tree, "-p", base.trim(), "-p", index, "-m", subject])?;
    run(&["stash", "store", "-m", subject, commit.trim()])?;
    run(&["checkout-index", "--all", "--force", "--index"]).map(|_| ())
}

fn use_stash(ed: &mut Editor, root: PathBuf, action: &str, name: String) {
    let done = format!("{} {}", if action == "pop" { "Popped" } else { "Applied" }, name);
    process::run(ed, root, args(&["stash", action, &name]), None, &done, |_| {});
}

pub fn drop_stash(ed: &mut Editor, root: PathBuf, name: String) {
    ed.confirm("git-stash-drop", format!("Drop {}? (y/n) ", name), move |ed, yes| {
        if yes {
            let done = format!("Dropped {}", name);
            process::run(ed, root, args(&["stash", "drop", &name]), None, &done, |_| {});
        }
    });
}

/// Runs `then` on the stash at point in the status buffer, else on one picked from the list.
fn with_stash(
    ed: &mut Editor,
    root: PathBuf,
    action: &str,
    then: impl FnOnce(&mut Editor, PathBuf, String) + Send + 'static,
) {
    if let Some(name) = status::stash_at_point(ed) {
        then(ed, root, name);
        return;
    }
    let title = format!("{} stash", action);
    let r = root.clone();
    process::query(ed, root, model::stash_list_args(), move |ed, out| {
        let stashes = model::parse_stashes(&out);
        if stashes.is_empty() {
            ed.set_status("No stashes");
            return;
        }
        let items = stashes.iter().map(|s| PickerItem::new(s.name.as_str(), s.subject.as_str())).collect();
        ed.pick("git-stash", title, items, move |ed, index| then(ed, r, stashes[index].name.clone()));
    });
}

/// Picks a ref under `namespaces` (`refs/heads`, `refs/remotes` without its `*/HEAD`s,
/// `refs/tags`) and passes its full name to `then`.
fn pick_ref(
    ed: &mut Editor,
    root: PathBuf,
    id: impl Into<String>,
    title: impl Into<String>,
    namespaces: &[&str],
    then: impl FnOnce(&mut Editor, PathBuf, String) + Send + 'static,
) {
    let (id, title) = (id.into(), title.into());
    let list = args(&[&["for-each-ref", "--format=%(refname)"], namespaces].concat());
    let r = root.clone();
    process::query(ed, root, list, move |ed, out| {
        let refs: Vec<String> = out.lines().filter(|l| !l.ends_with("/HEAD")).map(str::to_string).collect();
        let items = refs.iter().map(|full| split_ref(full)).map(|(name, kind)| PickerItem::new(name, kind)).collect();
        ed.pick(&id, title, items, move |ed, index| then(ed, r, refs[index].clone()));
    });
}

fn checkout(ed: &mut Editor, root: PathBuf) {
    pick_ref(ed, root, "git-checkout", "Checkout branch", BRANCHES, |ed, root, full| {
        // For a remote branch, checking out its local name creates a tracking branch.
        let target = match full.strip_prefix("refs/remotes/") {
            Some(remote) => remote.split_once('/').map_or(remote, |(_, name)| name),
            None => short_name(&full),
        };
        let done = format!("Checked out {}", target);
        process::run(ed, root, args(&["checkout", target]), None, &done, |_| {});
    });
}

fn create(ed: &mut Editor, root: PathBuf) {
    ed.prompt("git-branch-create", "Create branch: ", "", move |ed, name| {
        if name.is_empty() {
            return;
        }
        let done = format!("Created {}", name);
        process::run(ed, root, args(&["checkout", "-b", &name]), None, &done, |_| {});
    });
}

/// Deletes a picked ref under `namespaces` after confirmation. `kind` (`branch` or `tag`)
/// is what it is called, and the git command that deletes it.
fn delete(ed: &mut Editor, root: PathBuf, kind: &'static str, namespaces: &[&str]) {
    let id = format!("git-{}-delete", kind);
    pick_ref(ed, root, id.clone(), format!("Delete {}", kind), namespaces, move |ed, root, full| {
        let name = short_name(&full).to_string();
        ed.confirm(&id, format!("Delete {} {}? (y/n) ", kind, name), move |ed, yes| {
            if yes {
                let done = format!("Deleted {}", name);
                let log_root = root.clone();
                process::run(ed, root, args(&[kind, "-d", &name]), None, &done, move |ed| log::refresh(ed, &log_root));
            }
        });
    });
}

/// Pushes a picked tag to `origin`.
fn push_tag(ed: &mut Editor, root: PathBuf) {
    pick_ref(ed, root, "git-push-tag", "Push tag", &["refs/tags"], |ed, root, full| {
        let done = format!("Pushed {}", short_name(&full));
        process::run(ed, root, args(&["push", "origin", &full]), None, &done, |_| {});
    });
}

/// Tags the commit at point, else HEAD, under a prompted name. An annotated tag is an
/// object of its own, with a message that is prompted for too.
fn tag_commit(ed: &mut Editor, root: PathBuf, annotated: bool) {
    let commit = commit_at_point(ed);
    ed.prompt("git-tag-name", format!("Tag {} as: ", commit), "", move |ed, name| {
        if name.is_empty() {
            return;
        }
        let label = format!("Message for {}: ", name);
        let tag = move |ed: &mut Editor, options: &[&str]| {
            let done = format!("Tagged {} as {}", commit, name);
            let tag_args = args(&[&["tag"], options, &[name.as_str(), commit.as_str()]].concat());
            let log_root = root.clone();
            process::run(ed, root, tag_args, None, &done, move |ed| log::refresh(ed, &log_root));
        };
        if !annotated {
            return tag(ed, &[]);
        }
        ed.prompt("git-tag-message", label, "", move |ed, message| match message.is_empty() {
            true => ed.set_status("An annotated tag needs a message"),
            false => tag(ed, &["-a", "-m", &message]),
        });
    });
}

/// Merges a picked branch into the current one with `git merge <options>`.
fn merge_branch(ed: &mut Editor, root: PathBuf, options: &'static [&'static str]) {
    pick_ref(ed, root, "git-merge-branch", "Merge", BRANCHES, move |ed, root, full| {
        let branch = short_name(&full);
        let done = format!("Merged {}", branch);
        process::run(ed, root, args(&[&["merge"], options, &[branch]].concat()), None, &done, |_| {});
    });
}

/// Rebases the current branch onto a picked one.
fn rebase_onto(ed: &mut Editor, root: PathBuf) {
    pick_ref(ed, root, "git-rebase-onto", "Rebase onto", BRANCHES, |ed, root, full| {
        let onto = short_name(&full);
        let done = format!("Rebased onto {}", onto);
        process::run(ed, root, args(&["rebase", onto]), None, &done, |_| {});
    });
}

/// Abandons the stopped `operation` (`merge` or `rebase`) after confirmation, since the
/// conflicts resolved so far go with it.
fn abort(ed: &mut Editor, root: PathBuf, operation: &'static str) {
    let label = format!("Abort the {}, discarding its progress? (y/n) ", operation);
    ed.confirm(&format!("git-{}-abort", operation), label, move |ed, yes| match yes {
        true => {
            let done = format!("Aborted the {}", operation);
            process::run(ed, root, args(&[operation, "--abort"]), None, &done, |_| {});
        }
        false => ed.set_status("Abort cancelled"),
    });
}

/// The namespaces of local and remote-tracking branches.
const BRANCHES: &[&str] = &["refs/heads", "refs/remotes"];

/// A full ref name as its short name and what kind of ref that is.
fn split_ref(full: &str) -> (&str, &'static str) {
    const KINDS: [(&str, &str); 3] = [("refs/heads/", "local"), ("refs/remotes/", "remote"), ("refs/tags/", "tag")];
    KINDS.iter().find_map(|(prefix, kind)| Some((full.strip_prefix(prefix)?, *kind))).unwrap_or((full, "ref"))
}

fn short_name(full: &str) -> &str {
    split_ref(full).0
}
