//! On-disk changes, for servers that don't watch the file system themselves (gopls):
//! `workspace/didChangeWatchedFiles`.
//!
//! A server whose spec names files to watch has every directory under its root watched
//! for as long as it runs: the project's own, so nothing git ignores. Each burst of changes
//! (a checkout, another program's edits) reaches it as one notification naming the files
//! it cares about. A directory that went is reported as itself, since the server knows
//! what it held. A new one is explored on a job thread and watched before its files are
//! read, so nothing written to it meanwhile is missed.
//!
//! Where the platform reports no changes, servers aren't told of any; `lsp-restart` makes
//! one read the disk again.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::ops::Bound;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use ted_core::watch::{Change, Changes, Event, Watcher};
use ted_core::{Editor, Project};

use crate::client::{self, ServerId};
use crate::protocol::path_to_uri;
use crate::servers::ServerSpec;

/// `FileChangeType`s. A server reads the file to see what became of it, so a new one is
/// reported as changed too.
const CHANGED: u8 = 2;
const DELETED: u8 = 3;

#[derive(Default)]
struct WatchedFiles {
    /// `None` where the platform reports no changes.
    watcher: Option<Watcher>,
    /// The directories watched for each server that is told of changes.
    dirs: HashMap<ServerId, BTreeSet<PathBuf>>,
}

pub fn register(ed: &mut Editor) {
    let watcher = Watcher::events_only(ed, on_change);
    ed.ext_mut::<WatchedFiles>().watcher = watcher;
}

/// Starts telling `server` what changes under its root, if it wants to hear of any.
pub fn watch(ed: &mut Editor, server: ServerId) {
    watch_root(ed, server, false);
}

/// Stops watching for `server`.
pub fn unwatch(ed: &mut Editor, server: ServerId) {
    let state = ed.ext_mut::<WatchedFiles>();
    if let (Some(watcher), Some(dirs)) = (state.watcher.as_mut(), state.dirs.remove(&server)) {
        for dir in &dirs {
            watcher.unwatch(dir);
        }
    }
}

fn watch_root(ed: &mut Editor, server: ServerId, report: bool) {
    let Some(client) = client::live(ed, server).filter(|c| !c.spec.watched.is_empty()) else {
        return;
    };
    let root = client.root.clone();
    let state = ed.ext_mut::<WatchedFiles>();
    if state.watcher.is_some() {
        state.dirs.insert(server, BTreeSet::new());
        explore(ed, server, vec![root], report);
    }
}

/// Watches the project's directories at and below `tops` for `server`, once a job thread
/// has found them. With `report`, the server is then told of the files in them, which it
/// may not have seen.
fn explore(ed: &mut Editor, server: ServerId, tops: Vec<PathBuf>, report: bool) {
    let Some(client) = client::live(ed, server) else {
        return;
    };
    let (spec, project) = (client.spec, Project::of(Some(&client.root)));
    ed.spawn(move |ctx| {
        let dirs = project.dirs_under(tops);
        ctx.send(move |ed| {
            if !adopt(ed, server, &dirs) || !report {
                return;
            }
            ed.spawn(move |ctx| {
                let files = dirs.iter().flat_map(|dir| files_in(dir, spec));
                let events: Vec<Value> = files.map(|file| event(&file, CHANGED)).collect();
                ctx.send(move |ed| tell(ed, server, events));
            });
        });
    });
}

/// Watches `dirs` for `server`. False if nothing is watched for it any more.
fn adopt(ed: &mut Editor, server: ServerId, dirs: &[PathBuf]) -> bool {
    let state = ed.ext_mut::<WatchedFiles>();
    let (Some(watcher), Some(watched)) = (state.watcher.as_mut(), state.dirs.get_mut(&server)) else {
        return false;
    };
    for dir in dirs {
        if watched.insert(dir.clone()) {
            watcher.watch(dir);
        }
    }
    true
}

fn on_change(ed: &mut Editor, changes: Changes) -> bool {
    let servers: Vec<ServerId> = ed.ext_mut::<WatchedFiles>().dirs.keys().copied().collect();
    for server in servers {
        match &changes {
            Changes::Paths(changes) => forward(ed, server, changes),
            Changes::Unknown => rescan(ed, server),
        }
    }
    false
}

/// Tells `server` which of `changes` are to files it cares about, and follows the
/// directories that came and went.
fn forward(ed: &mut Editor, server: ServerId, changes: &[Change]) {
    let Some(spec) = client::live(ed, server).map(|c| c.spec) else {
        return;
    };
    let state = ed.ext_mut::<WatchedFiles>();
    let (Some(watcher), Some(watched)) = (state.watcher.as_mut(), state.dirs.get_mut(&server)) else {
        return;
    };
    let (mut events, mut appeared) = (Vec::new(), Vec::new());
    for Change { path, event: what } in changes {
        if !path.parent().is_some_and(|dir| watched.contains(dir)) {
            continue;
        }
        // A watched directory went, or something else took its place: so do the watches in
        // it, and the server drops what it knew there.
        if watched.contains(path) {
            let from = (Bound::Included(path.as_path()), Bound::Unbounded);
            let inside = watched.range::<Path, _>(from).take_while(|dir| dir.starts_with(path));
            for dir in inside.cloned().collect::<Vec<_>>() {
                watcher.unwatch(&dir);
                watched.remove(&dir);
            }
            events.push(event(path, DELETED));
        }
        match what {
            Event::NewDir if !is_hidden(path) => appeared.push(path.clone()),
            Event::Written if spec.watches(path) => events.push(event(path, CHANGED)),
            Event::Removed if spec.watches(path) => events.push(event(path, DELETED)),
            _ => {}
        }
    }
    tell(ed, server, events);
    if !appeared.is_empty() {
        explore(ed, server, appeared, true);
    }
}

/// Events were lost: watches `server`'s tree afresh and has it read all of it again. The
/// root stands for the files the server knows, so it notices the ones that went.
fn rescan(ed: &mut Editor, server: ServerId) {
    let Some(root) = client::live(ed, server).map(|c| c.root.clone()) else {
        return;
    };
    unwatch(ed, server);
    tell(ed, server, vec![event(&root, CHANGED)]);
    watch_root(ed, server, true);
}

fn tell(ed: &mut Editor, server: ServerId, events: Vec<Value>) {
    if !events.is_empty() {
        client::notify(ed, server, "workspace/didChangeWatchedFiles", json!({ "changes": events }));
    }
}

fn event(path: &Path, kind: u8) -> Value {
    json!({ "uri": path_to_uri(path), "type": kind })
}

/// The files in `dir` that `spec`'s server is told about.
fn files_in(dir: &Path, spec: &'static ServerSpec) -> impl Iterator<Item = PathBuf> {
    let entries = fs::read_dir(dir).into_iter().flatten().flatten();
    let files = entries.filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()));
    files.map(|entry| entry.path()).filter(|file| spec.watches(file))
}

fn is_hidden(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name.as_encoded_bytes().starts_with(b"."))
}
