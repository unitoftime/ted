//! Workspaces: named sets of windows and buffers anchored at a root directory, so one ted
//! holds what would otherwise be several instances, each set up for its own work.
//!
//! One workspace is active: its windows are the editor's `layout` and `saved_layouts`, and
//! its root is the working directory (where file prompts, dired and `*scratch*` start).
//! The others stay loaded in the background with their terminals and builds running, and
//! switching swaps windows in and out. Buffers are shared, so a file open in two
//! workspaces is one buffer; a workspace's buffers are the ones its windows have shown,
//! and they are all the buffer switcher offers. `*scratch*` belongs to every workspace.
//!
//! Named workspaces are saved as sessions (see `session`) in `workspaces/` in the config
//! directory: when switched away from, shortly after their buffers change, and when ted
//! exits. The unnamed workspace ted starts in is never saved; naming it makes it one that
//! is. A saved workspace is loaded the first time it is switched to.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::mem;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::buffer::BufferId;
use crate::commands::files::{anchor_directory, complete_path, offer_to_save};
use crate::editor::Editor;
use crate::key::format_seq;
use crate::keymap::KeymapId;
use crate::layout::Layout;
use crate::session::Session;
use crate::settings;
use crate::text::{absolutize, collapse_tilde};
use crate::ui::{Picker, PickerItem, Prompt};

/// How long after its buffers change a workspace is saved, so opening several at once
/// writes it once.
const SAVE_DELAY: Duration = Duration::from_secs(2);

pub struct Workspace {
    /// `None` for the unnamed workspace ted starts in.
    pub name: Option<String>,
    pub root: PathBuf,
    /// The buffers its windows have shown, most recent first.
    buffers: Vec<BufferId>,
    /// Its windows while in the background; the editor holds them while it is active.
    background: Option<Windows>,
}

struct Windows {
    layout: Layout,
    slots: HashMap<usize, Layout>,
}

impl Workspace {
    pub fn label(&self) -> &str {
        self.name.as_deref().unwrap_or("(unnamed)")
    }

    pub fn buffers(&self) -> &[BufferId] {
        &self.buffers
    }

    /// Its windows: its own in the background, `active` (the editor's) while active.
    fn windows<'a>(&'a self, active: WindowsRef<'a>) -> WindowsRef<'a> {
        self.background.as_ref().map_or(active, |w| (&w.layout, &w.slots))
    }

    /// Makes `ids` its most recent buffers, in that order.
    pub(crate) fn adopt(&mut self, ids: &[BufferId]) {
        self.buffers.retain(|id| !ids.contains(id));
        let mut adopted = Vec::with_capacity(ids.len() + self.buffers.len());
        for &id in ids {
            if !adopted.contains(&id) {
                adopted.push(id);
            }
        }
        adopted.append(&mut self.buffers);
        self.buffers = adopted;
    }
}

/// A workspace's windows: its layout and its layout slots.
pub type WindowsRef<'a> = (&'a Layout, &'a HashMap<usize, Layout>);
type WindowsMut<'a> = (&'a mut Layout, &'a mut HashMap<usize, Layout>);

pub struct Workspaces {
    /// Most recently active first: the first is the active one.
    loaded: Vec<Workspace>,
    /// Where named workspaces are saved; `None` keeps them in memory only.
    dir: Option<PathBuf>,
    save_pending: bool,
}

impl Workspaces {
    /// The unnamed workspace at `root`, active, saving named ones in `dir`.
    pub(crate) fn new(dir: Option<PathBuf>, root: PathBuf) -> Self {
        let unnamed = Workspace { name: None, root, buffers: Vec::new(), background: None };
        Self { loaded: vec![unnamed], dir, save_pending: false }
    }

    pub fn default_save_dir() -> Option<PathBuf> {
        crate::config::config_dir().map(|d| d.join("workspaces"))
    }

    pub fn active(&self) -> &Workspace {
        &self.loaded[0]
    }

    pub(crate) fn active_mut(&mut self) -> &mut Workspace {
        &mut self.loaded[0]
    }

    /// The loaded workspaces, the active one first, then by when they were last active.
    pub fn loaded(&self) -> &[Workspace] {
        &self.loaded
    }

    fn position(&self, name: Option<&str>) -> Option<usize> {
        self.loaded.iter().position(|ws| ws.name.as_deref() == name)
    }

    /// The names of the saved workspaces, sorted.
    pub fn saved(&self) -> Vec<String> {
        let Some(entries) = self.dir.as_ref().and_then(|dir| fs::read_dir(dir).ok()) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        names.sort_unstable();
        names
    }

    fn path(&self, name: &str) -> Option<PathBuf> {
        self.dir.as_ref().map(|dir| dir.join(name))
    }

    /// Buffer `id` was shown in the active workspace. Returns whether it is new there.
    pub(crate) fn touch(&mut self, id: BufferId) -> bool {
        let buffers = &mut self.loaded[0].buffers;
        match buffers.iter().position(|&b| b == id) {
            Some(i) => {
                buffers[..=i].rotate_right(1);
                false
            }
            None => {
                buffers.insert(0, id);
                true
            }
        }
    }

    /// Buffer `id` was killed.
    pub(crate) fn forget(&mut self, id: BufferId) {
        for ws in &mut self.loaded {
            ws.buffers.retain(|&b| b != id);
        }
    }

    /// Each loaded workspace with its windows, the active one first (whose windows are the
    /// editor's, passed in).
    pub fn with_windows<'a>(&'a self, active: WindowsRef<'a>) -> impl Iterator<Item = (&'a Workspace, WindowsRef<'a>)> {
        self.loaded.iter().map(move |ws| (ws, ws.windows(active)))
    }

    /// As `with_windows`, with each workspace's buffers and its windows to change.
    pub(crate) fn windows_mut<'a>(&'a mut self, active: WindowsMut<'a>) -> Vec<(&'a [BufferId], WindowsMut<'a>)> {
        let (first, rest) = self.loaded.split_first_mut().expect("a workspace is always active");
        let background = rest.iter_mut().map(|ws| {
            let windows = ws.background.as_mut().expect("background workspaces hold their windows");
            (&ws.buffers[..], (&mut windows.layout, &mut windows.slots))
        });
        std::iter::once((&first.buffers[..], active)).chain(background).collect()
    }
}

/// The switcher's picker id.
const SWITCHER: &str = "workspace-switch";

/// The commands the switcher's keymap binds, with the word its title shows each by.
const SWITCHER_ACTIONS: &[(&str, &str)] = &[
    ("workspace-new", "new"),
    ("workspace-rename", "rename"),
    ("workspace-unload", "unload"),
    ("workspace-delete", "delete"),
];

pub(crate) fn register(ed: &mut Editor) {
    let c = &mut ed.commands;
    c.register("workspace-switch", "Switch to, create or manage workspaces", |ed, _| switcher(ed));
    c.register("workspace-new", "Create a workspace at a directory and switch to it", |ed, _| {
        take_switcher(ed);
        new_prompt(ed);
    });
    c.register("workspace-rename", "Rename the workspace selected in the switcher, else the active one", |ed, _| {
        let Some(target) = Target::take(ed) else { return };
        let (label, initial) = match &target.name {
            Some(name) => (name.clone(), name.clone()),
            None => ("(unnamed)".to_string(), directory_name(&ed.workspaces.active().root)),
        };
        let prompt = format!("Rename workspace {} to: ", label);
        ed.prompt("workspace-name", prompt, initial, move |ed, name| {
            if rename(ed, target.name.as_deref(), name.trim()) {
                target.done(ed);
            }
        });
    });
    c.register("workspace-unload", "Close the workspace selected in the switcher and its own buffers", |ed, _| {
        let Some(target) = Target::take(ed) else { return };
        unload(ed, target.name.clone(), move |ed| target.done(ed));
    });
    c.register("workspace-delete", "Delete the saved workspace selected in the switcher", |ed, _| {
        let Some(target) = Target::take(ed) else { return };
        let Some(name) = target.name.clone() else {
            return ed.set_status("The unnamed workspace isn't saved");
        };
        let label = format!("Delete workspace {}? (y/n) ", name);
        ed.confirm("workspace-delete", label, move |ed, yes| {
            if yes {
                delete(ed, name, move |ed| target.done(ed));
            } else {
                ed.set_status("Canceled deleting the workspace");
            }
        });
    });
}

/// The workspace a command manages: the one selected in the switcher, which closes and
/// opens again once the command is done, else the active one.
struct Target {
    name: Option<String>,
    reopen: bool,
}

impl Target {
    fn take(ed: &mut Editor) -> Option<Target> {
        match take_switcher(ed) {
            Some(switcher) => Some(Target { name: switcher.selected_value::<Option<String>>()?.clone(), reopen: true }),
            None => Some(Target { name: ed.workspaces.active().name.clone(), reopen: false }),
        }
    }

    fn done(&self, ed: &mut Editor) {
        if self.reopen {
            switcher(ed);
        }
    }
}

/// Closes the switcher if it is open, returning it.
fn take_switcher(ed: &mut Editor) -> Option<Picker> {
    ed.top_modal().is_some_and(|modal| modal.id() == SWITCHER).then(|| ed.take_modal::<Picker>()).flatten()
}

fn item(label: &str, root: &Path) -> PickerItem {
    PickerItem::new(label, collapse_tilde(root))
}

/// The loaded workspaces, most recently active first (the active one last, so RET switches
/// away), then the saved ones not loaded yet. Its title lists the keys that manage them.
fn switcher(ed: &mut Editor) {
    let workspaces = &ed.workspaces;
    let loaded = workspaces.loaded();
    let mut entries: Vec<(PickerItem, Option<String>)> =
        loaded[1..].iter().chain(&loaded[..1]).map(|ws| (item(ws.label(), &ws.root), ws.name.clone())).collect();
    for name in workspaces.saved() {
        if workspaces.position(Some(&name)).is_none() {
            let root = workspaces.path(&name).and_then(|path| Session::read(&path).ok()).map(|s| s.root);
            let subtitle = root.map(|root| collapse_tilde(&root)).unwrap_or_default();
            entries.push((PickerItem::new(&name, subtitle), Some(name)));
        }
    }
    let picker = Picker::with_values(SWITCHER, switcher_title(ed), entries, |ed, name| {
        switch(ed, name.as_deref());
    });
    ed.push_modal(picker.keymap(KeymapId::WORKSPACE_SWITCH));
}

/// "Switch Workspace", then each management key as bound, e.g. "· M-n new".
fn switcher_title(ed: &Editor) -> String {
    let bindings = ed.keymaps.get(KeymapId::WORKSPACE_SWITCH).bindings();
    let mut title = "Switch Workspace".to_string();
    for (command, word) in SWITCHER_ACTIONS {
        let command = ed.commands.id(command);
        if let Some((keys, _)) = bindings.iter().find(|(_, binding)| Some(binding.command) == command) {
            let _ = write!(title, " · {} {}", format_seq(keys), word);
        }
    }
    title
}

/// Asks for the root (the active buffer's project by default), then the name (the root's
/// directory name by default).
fn new_prompt(ed: &mut Editor) {
    let initial = anchor_directory(&ed.project().root);
    let prompt = Prompt::new("workspace-root", "Workspace root: ", |ed, root| {
        let root = absolutize(root.trim());
        if !root.is_dir() {
            ed.set_status(format!("{} is not a directory", collapse_tilde(&root)));
            return;
        }
        ed.prompt("workspace-name", "Workspace name: ", directory_name(&root), move |ed, name| {
            create(ed, name.trim(), root);
        });
    });
    ed.push_modal(prompt.initial(initial).completer(complete_path));
}

fn directory_name(dir: &Path) -> String {
    dir.file_name().map_or_else(String::new, |n| n.to_string_lossy().to_string())
}

/// Why `name` can't name a new workspace, if it can't.
fn check_name(ed: &Editor, name: &str) -> Result<(), String> {
    if name.is_empty() || name.starts_with('.') || name.contains('/') {
        return Err(format!("'{}' can't name a workspace", name));
    }
    let workspaces = &ed.workspaces;
    if workspaces.position(Some(name)).is_some() || workspaces.saved().iter().any(|saved| saved == name) {
        return Err(format!("There is already a workspace named {}", name));
    }
    Ok(())
}

/// Switches to the workspace named `name` (`None`: the unnamed one), loading it if it is
/// saved but not loaded.
pub fn switch(ed: &mut Editor, name: Option<&str>) {
    match ed.workspaces.position(name) {
        Some(0) => {}
        Some(index) => {
            activate(ed, index);
            enter_root(ed);
        }
        None => {
            let path = name.and_then(|name| ed.workspaces.path(name));
            match path.map(|path| Session::read(&path)) {
                // Named by its file, which renaming it while unloaded moves.
                Some(Ok(session)) => {
                    open(ed, name.map(str::to_string), session.root.clone());
                    session.restore(ed);
                }
                Some(Err(e)) => return ed.set_status(format!("Cannot load the workspace: {}", e)),
                None => return ed.set_status("No such workspace"),
            }
        }
    }
    ed.check_external_changes();
    let active = ed.workspaces.active();
    let msg = format!("Workspace {} ({})", active.label(), collapse_tilde(&active.root));
    ed.set_status(msg);
}

/// Makes the workspace `name` active with `root`, created with no buffers if it isn't
/// loaded.
pub(crate) fn open(ed: &mut Editor, name: Option<String>, root: PathBuf) {
    match ed.workspaces.position(name.as_deref()) {
        Some(index) => {
            if index > 0 {
                activate(ed, index);
            }
            ed.workspaces.active_mut().root = root;
        }
        None => {
            let scratch = ed.ensure_scratch();
            let layout = Layout::new(scratch, ed.settings.get(settings::WRAP_LINES));
            let background = Some(Windows { layout, slots: HashMap::new() });
            ed.workspaces.loaded.push(Workspace { name, root, buffers: Vec::new(), background });
            activate(ed, ed.workspaces.loaded.len() - 1);
        }
    }
    enter_root(ed);
}

/// Saves the active workspace and swaps the windows of the loaded workspace `index` in.
fn activate(ed: &mut Editor, index: usize) {
    save_active(ed);
    ed.remember_places();
    let incoming = ed.workspaces.loaded[index].background.take().expect("background workspaces hold their windows");
    let outgoing = Windows {
        layout: mem::replace(&mut ed.layout, incoming.layout),
        slots: mem::replace(&mut ed.saved_layouts, incoming.slots),
    };
    ed.workspaces.loaded[0].background = Some(outgoing);
    ed.workspaces.loaded[..=index].rotate_right(1);
    // Buffers may have shrunk while it was in the background.
    ed.clamp_layout();
}

fn enter_root(ed: &mut Editor) {
    let root = ed.workspaces.active().root.clone();
    if let Err(e) = std::env::set_current_dir(&root) {
        ed.set_status(format!("Cannot enter {}: {}", collapse_tilde(&root), e));
    }
}

/// Creates the workspace `name` at `root`, switches to it and lists the root.
fn create(ed: &mut Editor, name: &str, root: PathBuf) {
    if let Err(e) = check_name(ed, name) {
        return ed.set_status(e);
    }
    open(ed, Some(name.to_string()), root.clone());
    if let Err(e) = ed.open_file(&root) {
        ed.set_status(format!("Error opening {}: {}", root.display(), e));
    }
    save_active(ed);
}

/// Renames the workspace `target` (`None`: the unnamed one, which is saved from then on).
/// Returns whether it did.
fn rename(ed: &mut Editor, target: Option<&str>, name: &str) -> bool {
    if target == Some(name) {
        return false;
    }
    if let Err(e) = check_name(ed, name) {
        ed.set_status(e);
        return false;
    }
    let workspaces = &mut ed.workspaces;
    match workspaces.position(target) {
        Some(index) => {
            let old = workspaces.loaded[index].name.replace(name.to_string());
            if let Some(path) = old.and_then(|old| workspaces.path(&old)) {
                let _ = fs::remove_file(path);
            }
            save(ed, index);
        }
        None => {
            let paths = target.and_then(|old| workspaces.path(old)).zip(workspaces.path(name));
            if let Some(Err(e)) = paths.map(|(from, to)| fs::rename(from, to)) {
                ed.set_status(format!("Cannot rename the workspace: {}", e));
                return false;
            }
        }
    }
    ed.set_status(format!("Renamed the workspace to {}", name));
    true
}

/// Unloads the background workspace `name` once the modified files only it shows are saved
/// or knowingly left, killing the buffers only it shows, then runs `then`.
fn unload(ed: &mut Editor, name: Option<String>, then: impl FnOnce(&mut Editor) + 'static) {
    let Some(index) = ed.workspaces.position(name.as_deref()).filter(|&i| i > 0) else {
        return ed.set_status("Switch to another workspace first");
    };
    let workspaces = ed.workspaces.loaded();
    let shared = |id: &BufferId| workspaces.iter().enumerate().any(|(i, ws)| i != index && ws.buffers.contains(id));
    let only_here: Vec<BufferId> = workspaces[index].buffers.iter().copied().filter(|id| !shared(id)).collect();
    offer_to_save(ed, only_here.clone(), move |ed| {
        let Some(index) = ed.workspaces.position(name.as_deref()).filter(|&i| i > 0) else { return };
        save(ed, index);
        let ws = ed.workspaces.loaded.remove(index);
        for id in only_here {
            if ed.buffers.contains(id) && !ed.buffers[id].is_scratch() {
                ed.kill_buffer(id);
            }
        }
        ed.set_status(format!("Unloaded workspace {}", ws.label()));
        then(ed);
    });
}

/// Deletes the saved workspace `name`, unloading it first if it is loaded, then runs `then`.
fn delete(ed: &mut Editor, name: String, then: impl FnOnce(&mut Editor) + 'static) {
    let loaded = ed.workspaces.position(Some(&name));
    let path = ed.workspaces.path(&name);
    // A loaded one's buffers are killed instead, releasing what they hold.
    let saved = path.as_deref().filter(|_| loaded.is_none()).and_then(|path| Session::read(path).ok());
    let remove = move |ed: &mut Editor| {
        match path.map_or(Ok(()), fs::remove_file) {
            Ok(()) => ed.set_status(format!("Deleted workspace {}", name)),
            Err(e) => return ed.set_status(format!("Cannot delete workspace {}: {}", name, e)),
        }
        then(ed);
    };
    match loaded {
        Some(0) => ed.set_status("Switch to another workspace first"),
        Some(index) => {
            let name = ed.workspaces.loaded[index].name.clone();
            unload(ed, name, remove);
        }
        None => {
            if let Some(session) = saved {
                discard(ed, &session);
            }
            remove(ed);
        }
    }
}

/// Runs the `restore_discarded` hooks on each restore argument `session` holds.
fn discard(ed: &mut Editor, session: &Session) {
    for hook in ed.hooks.restore_discarded.clone() {
        for (command, argument) in session.restore_arguments() {
            hook(ed, command, argument);
        }
    }
}

/// Saves the active workspace soon: its buffers changed.
pub(crate) fn schedule_save(ed: &mut Editor) {
    if ed.workspaces.dir.is_none() || ed.workspaces.active().name.is_none() || ed.workspaces.save_pending {
        return;
    }
    ed.workspaces.save_pending = true;
    ed.after(SAVE_DELAY, |ed| {
        ed.workspaces.save_pending = false;
        save_active(ed);
    });
}

fn save_active(ed: &mut Editor) {
    save(ed, 0);
}

/// Saves every loaded named workspace; for when ted exits.
pub(crate) fn save_all(ed: &mut Editor) {
    for index in 0..ed.workspaces.loaded.len() {
        save(ed, index);
    }
}

/// Writes the loaded workspace `index` to its file, if it is named.
fn save(ed: &mut Editor, index: usize) {
    let ws = &ed.workspaces.loaded[index];
    let Some(path) = ws.name.as_deref().and_then(|name| ed.workspaces.path(name)) else { return };
    let session = Session::capture(ws, ws.windows((&ed.layout, &ed.saved_layouts)), &ed.buffers);
    let result = path.parent().map_or(Ok(()), fs::create_dir_all).and_then(|()| session.write(&path));
    if let Err(e) = result {
        ed.set_status(format!("Cannot save the workspace to {}: {}", path.display(), e));
    }
}
