//! Menus for commit, push, pull, fetch, branch and stash operations.

use std::path::{Path, PathBuf};

use ted_core::{Editor, Menu, PickerItem};

use crate::git::args;
use crate::{commit, diff, model, process, status};

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
    let (r1, r2, r3, r4, r5) = (root.clone(), root.clone(), root.clone(), root.clone(), root.clone());
    let menu = Menu::new("git-diff", "Diff")
        .group("Diff")
        .entry('d', "Dwim (thing at point)", move |ed| diff::dwim(ed, r1))
        .entry('u', "Unstaged", move |ed| diff::unstaged(ed, r2))
        .entry('s', "Staged", move |ed| diff::staged(ed, r3))
        .entry('w', "Worktree (vs HEAD)", move |ed| diff::worktree(ed, r4))
        .group("Compare")
        .entry('r', "Range (A..B)", move |ed| {
            ed.prompt("git-diff-range", "Diff range: ", "", move |ed, range| {
                if !range.is_empty() {
                    diff::range(ed, r5, range);
                }
            })
        })
        .entry('c', "Show commit", move |ed| {
            ed.prompt("git-show-commit", "Show commit: ", "HEAD", move |ed, rev| {
                if !rev.is_empty() {
                    diff::commit(ed, root, rev);
                }
            })
        });
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
        .entry('p', "Pull from upstream", run(&root, &["pull"], "Pulled"))
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
        .entry('z', "Both (worktree and index)", move |ed| stash_push(ed, r1, &[]))
        .entry('i', "Index", move |ed| stash_push(ed, r2, &["--staged"]))
        .entry('u', "Including untracked", move |ed| stash_push(ed, r3, &["--include-untracked"]))
        .group("Use")
        .entry('a', "Apply", move |ed| with_stash(ed, r4, "Apply", |ed, root, name| use_stash(ed, root, "apply", name)))
        .entry('p', "Pop", move |ed| with_stash(ed, r5, "Pop", |ed, root, name| use_stash(ed, root, "pop", name)))
        .entry('k', "Drop", move |ed| with_stash(ed, r6, "Drop", drop_stash));
    ed.push_modal(menu);
}

/// `git stash push <flags>`, with a message if one is given.
fn stash_push(ed: &mut Editor, root: PathBuf, flags: &'static [&'static str]) {
    ed.prompt("git-stash-message", "Stash message (optional): ", "", move |ed, message| {
        let mut git_args = args(&["stash", "push"]);
        git_args.extend(flags.iter().map(|f| f.to_string()));
        if !message.is_empty() {
            git_args.extend(["-m".to_string(), message]);
        }
        process::run(ed, root, git_args, None, "Stashed", |_| {});
    });
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
