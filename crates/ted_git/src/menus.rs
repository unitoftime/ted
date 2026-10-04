//! Menus for commit, push, pull, fetch, branch, stash and reset operations.

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
    let menu = Menu::new("git-push", "Push")
        .group("Push")
        .entry('p', "Push to upstream", run(&root, &["push"], "Pushed"))
        .entry('u', "Push and set upstream", run(&root, &["push", "-u", "origin", "HEAD"], "Pushed"))
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
        .entry('k', "Delete branch", move |ed| delete(ed, r3));
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

/// Resets HEAD to a prompted commit, defaulting to the one at point, with `git reset
/// --<mode>`. A hard reset asks first, since it discards uncommitted changes.
fn reset_to(ed: &mut Editor, root: PathBuf, mode: &'static str) {
    let initial = status::commit_at_point(ed).or_else(|| log::commit_at_point(ed)).unwrap_or_else(|| "HEAD".into());
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

/// Local branches and remote-tracking branches (without `*/HEAD`), by full ref name.
fn with_branches(
    ed: &mut Editor,
    root: PathBuf,
    remotes: bool,
    then: impl FnOnce(&mut Editor, PathBuf, Vec<String>) + Send + 'static,
) {
    let mut refs = args(&["for-each-ref", "--format=%(refname)", "refs/heads"]);
    if remotes {
        refs.push("refs/remotes".into());
    }
    let r = root.clone();
    process::query(ed, root, refs, move |ed, out| {
        let names = out.lines().filter(|l| !l.ends_with("/HEAD")).map(str::to_string).collect();
        then(ed, r, names);
    });
}

fn checkout(ed: &mut Editor, root: PathBuf) {
    with_branches(ed, root, true, |ed, root, refs| {
        let items = refs
            .iter()
            .map(|r| PickerItem::new(short_name(r), if r.starts_with("refs/remotes/") { "remote" } else { "local" }))
            .collect();
        ed.pick("git-checkout", "Checkout branch", items, move |ed, index| {
            let full = &refs[index];
            // For a remote branch, checking out its local name creates a tracking branch.
            let target = match full.strip_prefix("refs/remotes/") {
                Some(remote) => remote.split_once('/').map_or(remote, |(_, name)| name).to_string(),
                None => short_name(full).to_string(),
            };
            let done = format!("Checked out {}", target);
            process::run(ed, root, args(&["checkout", &target]), None, &done, |_| {});
        });
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

fn delete(ed: &mut Editor, root: PathBuf) {
    with_branches(ed, root, false, |ed, root, refs| {
        let names: Vec<String> = refs.iter().map(|r| short_name(r).to_string()).collect();
        let items = names.iter().map(|n| PickerItem::new(n.as_str(), "local")).collect();
        ed.pick("git-branch-delete", "Delete branch", items, move |ed, index| {
            let name = names[index].clone();
            ed.confirm("git-branch-delete", format!("Delete branch {}? (y/n) ", name), move |ed, yes| {
                if yes {
                    let done = format!("Deleted {}", name);
                    process::run(ed, root, args(&["branch", "-d", &name]), None, &done, |_| {});
                }
            });
        });
    });
}

fn short_name(full: &str) -> &str {
    full.strip_prefix("refs/heads/").or_else(|| full.strip_prefix("refs/remotes/")).unwrap_or(full)
}
