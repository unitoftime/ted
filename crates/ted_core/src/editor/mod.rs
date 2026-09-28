//! The editor: owns buffers, the window layout, registries and the modal stack, and runs
//! the command loop (key -> keymap layers -> command).

mod buffers;
mod modals;
mod render;

use std::any::Any;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::buffer::{Buffer, BufferId, Buffers};
use crate::command::{Arg, CommandId, Commands};
use crate::config::{self, ConfigOp, Script};
use crate::doc::Doc;
use crate::ext::Extensions;
use crate::face::Faces;
use crate::jobs::{JobContext, JobHandle, Scheduler};
use crate::key::{format_seq, Key, KeyCode, KeyEvent};
use crate::keymap::{Binding, KeymapDef, KeymapId, Keymaps, Resolved};
use crate::kill_ring::KillRing;
use crate::layout::Layout;
use crate::mode::{IndentStyle, Indentation, Mode, ModeRegistry};
use crate::plugin::{Hooks, Plugin};
use crate::project::{FileLists, Project};
use crate::recentf::RecentFiles;
use crate::settings::{self, Settings, Value};
use crate::theme::Theme;
use crate::ui::{InputHistory, Modal};
use crate::view::{View, ViewId};
use crate::workspace::Workspaces;

pub use buffers::BufferScope;

/// Chain value an undo command leaves, so consecutive undos keep walking back instead of
/// undoing the undo.
pub struct UndoChain;

/// Startup configuration. `Default` is hermetic (no user files); frontends use `user()`.
#[derive(Default)]
pub struct StartupOptions {
    pub init_script: Option<PathBuf>,
    pub recent_files: Option<PathBuf>,
    pub input_history: Option<PathBuf>,
    /// Where named workspaces are saved; `None` keeps them in memory only.
    pub workspaces: Option<PathBuf>,
    pub plugins: Vec<Box<dyn Plugin>>,
}

impl StartupOptions {
    pub fn user() -> Self {
        Self {
            init_script: config::default_init_script(),
            recent_files: RecentFiles::default_save_path(),
            input_history: InputHistory::default_save_path(),
            workspaces: Workspaces::default_save_dir(),
            plugins: Vec::new(),
        }
    }
}

/// Keymaps, modes and settings as the built-ins and plugins left them, before `init.rhai`;
/// `reload-init` starts over from here.
struct Defaults {
    keymaps: Keymaps,
    modes: ModeRegistry,
    settings: Vec<Value>,
}

pub struct Editor {
    pub settings: Settings,
    pub faces: Faces,
    pub buffers: Buffers,
    pub layout: Layout,
    pub saved_layouts: HashMap<usize, Layout>,
    /// The active workspace (whose windows are `layout` and `saved_layouts`) and the ones
    /// loaded in the background.
    pub workspaces: Workspaces,
    pub commands: Commands,
    pub keymaps: Keymaps,
    pub modes: ModeRegistry,
    pub hooks: Hooks,
    pub kill_ring: KillRing,
    pub recent_files: RecentFiles,
    pub input_history: InputHistory,
    pub status: String,
    pub running: bool,
    /// Set when the editor stopped to restart (`reload-ted`): the session file the
    /// frontend hands to the process that replaces it.
    pub restart: Option<PathBuf>,
    /// Whether the OS window has keyboard focus; without it no window draws as active.
    pub focused: bool,
    modals: Vec<Box<dyn Modal>>,
    pending_keys: Vec<Key>,
    /// Keys to handle as typed once the current one is done (`unread_key`).
    unread_keys: Vec<Key>,
    shift_translated: bool,
    last_command: Option<CommandId>,
    last_chain: Option<Box<dyn Any>>,
    this_chain: Option<Box<dyn Any>>,
    ext: Extensions,
    plugins: Vec<Box<dyn Plugin>>,
    scheduler: Scheduler,
    init_script: Option<PathBuf>,
    defaults: Option<Defaults>,
}

impl Editor {
    /// A hermetic editor (no user config, recent files kept in memory).
    pub fn new(files: &[PathBuf]) -> Self {
        Self::with_options(files, StartupOptions::default())
    }

    pub fn with_options(files: &[PathBuf], options: StartupOptions) -> Self {
        let settings = Settings::default();
        let mut buffers = Buffers::default();
        let scratch = buffers.insert(Buffer::scratch());

        let mut ed = Self {
            faces: Faces::new(&Theme::default()),
            layout: Layout::new(scratch, settings.get(settings::WRAP_LINES)),
            settings,
            buffers,
            saved_layouts: HashMap::new(),
            workspaces: Workspaces::new(options.workspaces, crate::text::absolutize(".")),
            commands: Commands::default(),
            keymaps: Keymaps::default(),
            modes: ModeRegistry::default(),
            hooks: Hooks::default(),
            kill_ring: KillRing::default(),
            recent_files: RecentFiles::load_or_default(options.recent_files),
            input_history: InputHistory::load_or_default(options.input_history),
            status: "Welcome to ted".to_string(),
            running: true,
            restart: None,
            focused: true,
            modals: Vec::new(),
            pending_keys: Vec::new(),
            unread_keys: Vec::new(),
            shift_translated: false,
            last_command: None,
            last_chain: None,
            this_chain: None,
            ext: Extensions::default(),
            plugins: Vec::new(),
            scheduler: Scheduler::default(),
            init_script: options.init_script,
            defaults: None,
        };

        ed.watch_setting(settings::THEME, Editor::apply_theme);
        ed.watch_setting(settings::WRAP_LINES, |ed| {
            let wrap = ed.settings.get(settings::WRAP_LINES);
            for view in ed.layout.views_mut() {
                view.wrap = wrap;
            }
        });
        ed.watch_setting(settings::TAB_WIDTH, Editor::apply_indentation);
        ed.watch_setting(settings::INDENT, Editor::apply_indentation);
        crate::commands::register_builtin(&mut ed);
        crate::modes::register_builtin(&mut ed);
        crate::commands::bindings::install_defaults(&mut ed);
        for plugin in options.plugins {
            ed.add_plugin_boxed(plugin);
        }
        ed.defaults =
            Some(Defaults { keymaps: ed.keymaps.clone(), modes: ed.modes.clone(), settings: ed.settings.values() });
        ed.load_init_script();

        for (i, file) in files.iter().enumerate() {
            if i > 0 {
                let view = ed.layout.split(crate::layout::SplitType::Vertical);
                ed.layout.set_active(view);
            }
            if let Err(e) = ed.open_file(file) {
                ed.set_status(format!("Error opening {}: {}", file.display(), e));
            }
        }
        ed
    }

    // ---------------------------------------------------------------------------------
    // Configuration & extension
    // ---------------------------------------------------------------------------------

    pub fn add_plugin(&mut self, plugin: impl Plugin) {
        self.add_plugin_boxed(Box::new(plugin));
    }

    fn add_plugin_boxed(&mut self, mut plugin: Box<dyn Plugin>) {
        plugin.init(self);
        self.plugins.push(plugin);
    }

    pub fn plugins(&self) -> impl Iterator<Item = &dyn Plugin> {
        self.plugins.iter().map(|p| p.as_ref())
    }

    pub fn ext<T: 'static>(&self) -> Option<&T> {
        self.ext.get()
    }

    pub fn ext_mut<T: Default + 'static>(&mut self) -> &mut T {
        self.ext.get_mut()
    }

    /// Stores editor-wide state that has no sensible default, e.g. the setting handles and
    /// keymaps a plugin creates in `init`.
    pub fn set_ext<T: 'static>(&mut self, value: T) {
        self.ext.insert(value);
    }

    fn load_init_script(&mut self) {
        let Some(path) = self.init_script.clone() else { return };
        let errors = match config::run_init_script(&path) {
            Ok(script) => self.apply_config(&script),
            Err(e) => vec![e],
        };
        match errors.as_slice() {
            [] => {}
            [error] => self.set_status(format!("init.rhai: {}", error)),
            [first, ..] => {
                crate::commands::help::show_errors(self, "init.rhai", &errors);
                self.set_status(format!("init.rhai: {} (and {} more)", first, errors.len() - 1));
            }
        }
    }

    /// Returns keymaps, modes, settings and faces to their defaults, then runs `init.rhai`
    /// again, so edits to it (including removed lines) take effect without a restart.
    pub fn reload_init(&mut self) {
        let Some(defaults) = self.defaults.take() else { return };
        self.keymaps.restore(&defaults.keymaps);
        let mode_settings = |modes: &ModeRegistry| -> Vec<u16> {
            modes.iter().flat_map(|mode| mode.settings().iter().map(|(index, _)| *index)).collect()
        };
        let mut changed = mode_settings(&self.modes);
        self.modes = defaults.modes.clone();
        self.refresh_buffer_modes();
        changed.extend(mode_settings(&self.modes));
        self.faces.clear_customizations();
        changed.extend(self.settings.restore(&defaults.settings));
        changed.sort_unstable();
        changed.dedup();
        for index in changed {
            self.run_setting_watchers(index);
        }
        self.apply_theme();
        self.defaults = Some(defaults);
        self.set_status("Reloaded init.rhai");
        self.load_init_script();
    }

    /// Applies what a config script recorded. Returns every error, the script's own and
    /// those of operations that failed, as `line N: message` in line order.
    pub fn apply_config(&mut self, script: &Script) -> Vec<String> {
        let mut errors = script.errors.clone();
        for (line, op) in &script.ops {
            let result = match op {
                ConfigOp::Set { key, value } => self.set_setting(key, value),
                ConfigOp::Bind { keymap, keys, command } => self.bind(keymap, keys, command),
                ConfigOp::Unbind { keymap, keys } => self.unbind(keymap, keys),
                ConfigOp::Face { name, face } => self.faces.customize(name, *face),
                ConfigOp::Mode { name, values } => self.customize_mode(name, values),
            };
            if let Err(e) = result {
                errors.push((*line, e));
            }
        }
        errors.sort_by_key(|(line, _)| *line);
        errors.into_iter().map(|(line, e)| format!("line {}: {}", line, e)).collect()
    }

    /// Binds `keys` (e.g. `"C-x C-f"`) in the keymap named `keymap` to `spec`: a command
    /// name optionally followed by an argument, e.g. `"layout-restore 2"`.
    pub fn bind(&mut self, keymap: &str, keys: &str, spec: &str) -> Result<(), String> {
        let seq = Key::parse_seq(keys)?;
        let (name, arg) = match spec.trim().split_once(' ') {
            Some((name, arg)) => (name, Arg::parse(arg.trim())),
            None => (spec.trim(), Arg::None),
        };
        let command = self.commands.id(name).ok_or_else(|| format!("Unknown command '{}'", name))?;
        let map = self.keymaps.id(keymap).ok_or_else(|| format!("Unknown keymap '{}'", keymap))?;
        self.keymaps.get_mut(map).bind(&seq, Binding { command, arg });
        Ok(())
    }

    pub fn unbind(&mut self, keymap: &str, keys: &str) -> Result<(), String> {
        let seq = Key::parse_seq(keys)?;
        let map = self.keymaps.id(keymap).ok_or_else(|| format!("Unknown keymap '{}'", keymap))?;
        self.keymaps.get_mut(map).unbind(&seq);
        Ok(())
    }

    /// Defines the keymap `def` names, or adds to it if it exists, and returns it. Every
    /// command it names must be registered: definitions are part of the program, so a bad
    /// one is a bug and panics.
    pub fn define_keymap(&mut self, def: KeymapDef) -> KeymapId {
        let command = |ed: &Editor, name: &str| {
            ed.commands.id(name).unwrap_or_else(|| panic!("keymap '{}' names unknown command '{}'", def.name, name))
        };
        let self_insert = def.self_insert.as_deref().map(|name| command(self, name));
        let fallback = def.fallback.as_deref().map(|name| command(self, name));
        let parent = def.parent.as_deref().map(|name| self.keymaps.ensure(name));
        let id = self.keymaps.ensure(&def.name);
        let map = self.keymaps.get_mut(id);
        map.parent = parent.or(map.parent);
        map.opaque |= def.opaque;
        map.self_insert = self_insert.or(map.self_insert);
        map.fallback = fallback.or(map.fallback);
        for keys in &def.fallback_exempt {
            map.fallback_exempt.push(Key::parse(keys).unwrap_or_else(|e| panic!("keymap '{}': {}", def.name, e)));
        }
        for (keys, spec) in &def.keys {
            if let Err(e) = self.bind(&def.name, keys, spec) {
                panic!("invalid default binding {} -> {} in '{}': {}", keys, spec, def.name, e);
            }
        }
        id
    }

    /// Registers `mode` (replacing one of the same name) and defines its keymap, named
    /// after the mode in kebab-case, which it returns.
    pub fn define_mode(&mut self, mut mode: Mode) -> KeymapId {
        let checked: Vec<(u16, Value)> = mode
            .settings()
            .iter()
            .map(|(index, value)| self.settings.accept(self.settings.name(*index), value))
            .collect::<Result<_, _>>()
            .unwrap_or_else(|e| panic!("mode '{}': {}", mode.name, e));
        for (index, value) in checked {
            mode.set_value(index, value);
        }
        let mut keymap = std::mem::take(&mut mode.keymap_def);
        keymap.name = mode.keymap_name();
        let keymap = self.define_keymap(keymap);
        mode.keymap = Some(keymap);
        self.modes.register(mode);
        self.refresh_buffer_modes();
        keymap
    }

    /// Applies `mode(name, #{ .. })` from `init.rhai`: the mode's own properties
    /// (`comment`, `extensions`, `line_numbers`, `highlight_line`), and any setting, which
    /// then has that value in the mode's buffers.
    fn customize_mode(&mut self, name: &str, values: &settings::Map) -> Result<(), String> {
        const PROPERTIES: [&str; 4] = ["comment", "extensions", "line_numbers", "highlight_line"];
        let settings: Vec<(u16, Value)> = values
            .iter()
            .filter(|(key, _)| !PROPERTIES.contains(&key.as_str()))
            .map(|(key, value)| self.settings.accept(key, value))
            .collect::<Result<_, _>>()?;
        self.update_mode(name, |mode| {
            for (key, value) in values {
                let invalid = |kind: &str| Err(format!("'{}' must be {}", key, kind));
                match (key.as_str(), value) {
                    ("comment", Value::Str(prefix)) => mode.comment_prefix.clone_from(prefix),
                    ("comment", _) => return invalid("a string"),
                    ("extensions", Value::List(list)) => {
                        let extensions = list.iter().map(|ext| match ext {
                            Value::Str(ext) => Some(ext.trim_start_matches('.').to_string()),
                            _ => None,
                        });
                        match extensions.collect() {
                            Some(extensions) => mode.extensions = extensions,
                            None => return invalid("an array of strings"),
                        }
                    }
                    ("extensions", _) => return invalid("an array of strings"),
                    ("line_numbers", Value::Bool(show)) => mode.line_numbers = *show,
                    ("highlight_line", Value::Bool(highlight)) => mode.highlight_line = *highlight,
                    ("line_numbers" | "highlight_line", _) => return invalid("true or false"),
                    _ => {}
                }
            }
            for (index, value) in settings.iter().cloned() {
                mode.set_value(index, value);
            }
            Ok(())
        })?;
        for (index, _) in settings {
            self.run_setting_watchers(index);
        }
        Ok(())
    }

    /// Replaces mode `name` with a copy `change` made, and points its buffers at it.
    pub(crate) fn update_mode(
        &mut self,
        name: &str,
        change: impl FnOnce(&mut Mode) -> Result<(), String>,
    ) -> Result<(), String> {
        let mode = self.modes.get(name).ok_or_else(|| format!("Unknown mode '{}'", name))?;
        let mut mode = Mode::clone(&mode);
        change(&mut mode)?;
        self.modes.register(mode);
        self.refresh_buffer_modes();
        Ok(())
    }

    /// Points buffers at the current definitions of their modes after modes changed.
    fn refresh_buffer_modes(&mut self) {
        for (_, buf) in self.buffers.iter_mut() {
            if let Some(mode) = self.modes.get(&buf.mode().name).filter(|m| !Arc::ptr_eq(m, buf.mode())) {
                buf.update_mode(mode);
            }
        }
    }

    fn apply_theme(&mut self) {
        let theme = Theme::by_name(self.settings.get(settings::THEME)).unwrap_or_default();
        self.faces.apply_theme(&theme);
    }

    fn apply_indentation(&mut self) {
        let width = self.settings.get(settings::TAB_WIDTH).clamp(1, 16) as usize;
        let style = IndentStyle::from_setting(self.settings.get(settings::INDENT));
        self.buffers.set_indentation(Indentation { width, style });
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = msg.into();
    }

    /// What the OS window is titled: the active workspace's name.
    pub fn title(&self) -> String {
        match &self.workspaces.active().name {
            Some(name) => format!("{} - ted", name),
            None => "ted".to_string(),
        }
    }

    /// Stops the editor, running the `quit` hooks and saving the named workspaces first.
    pub fn quit(&mut self) {
        for hook in self.hooks.quit.clone() {
            hook(self);
        }
        crate::workspace::save_all(self);
        self.running = false;
    }

    // ---------------------------------------------------------------------------------
    // Active view & buffer
    // ---------------------------------------------------------------------------------

    pub fn active_view(&self) -> &View {
        self.layout.active()
    }

    pub fn active_view_mut(&mut self) -> &mut View {
        self.layout.active_mut()
    }

    pub fn active_buffer_id(&self) -> BufferId {
        self.layout.active().buffer
    }

    pub fn active_buffer(&self) -> &Buffer {
        &self.buffers[self.active_buffer_id()]
    }

    pub fn active_buffer_mut(&mut self) -> &mut Buffer {
        let id = self.active_buffer_id();
        &mut self.buffers[id]
    }

    /// The project of the active buffer's working directory.
    pub fn project(&self) -> Project {
        self.active_buffer().project()
    }

    /// Every project's cached file list; see `project::FileLists`.
    pub fn file_lists(&mut self) -> FileLists {
        self.ext_mut::<FileLists>().clone()
    }

    /// The active view and its buffer, for editing.
    pub fn doc(&mut self) -> Doc<'_> {
        let view = self.layout.active_mut();
        let buf = &mut self.buffers[view.buffer];
        Doc::new(view, buf)
    }

    /// Where typing goes: the top modal's input while it has one, else the active view.
    /// Motion and editing commands work through this, so they apply to prompts too.
    pub fn focused_doc(&mut self) -> Doc<'_> {
        if self.modals.last().is_some_and(|m| m.input().is_some()) {
            let input = self.modals.last_mut().and_then(|m| m.input_mut()).expect("checked above");
            return input.doc();
        }
        self.doc()
    }

    pub fn doc_for(&mut self, view: ViewId) -> Option<Doc<'_>> {
        let view = self.layout.view_mut(view)?;
        let buf = &mut self.buffers[view.buffer];
        Some(Doc::new(view, buf))
    }

    // ---------------------------------------------------------------------------------
    // Command loop
    // ---------------------------------------------------------------------------------

    pub fn handle_key(&mut self, ev: KeyEvent) {
        self.dispatch(Key::from_event(ev));
        while !self.unread_keys.is_empty() {
            let key = self.unread_keys.remove(0);
            self.dispatch(key);
        }
    }

    /// Queues `key` to be handled as if typed, once the current command finishes: a modal
    /// that ends on a key it doesn't use hands the key on to whatever is below it.
    pub fn unread_key(&mut self, key: Key) {
        self.unread_keys.push(key);
    }

    fn dispatch(&mut self, key: Key) {
        if !self.pending_keys.is_empty() && key == Key::new(KeyCode::Char('g'), true, false, false) {
            self.pending_keys.clear();
            self.execute("keyboard-quit");
            return;
        }

        let layers = self.active_layers();
        match self.keymaps.resolve(&layers, &self.pending_keys, key) {
            Resolved::Prefix { key } => {
                self.pending_keys.push(key);
                self.set_status(format!("{}- (type ? or C-h for help)", format_seq(&self.pending_keys)));
            }
            Resolved::Command { binding, shift_translated, .. } => {
                self.pending_keys.clear();
                self.shift_translated = shift_translated;
                self.execute_command(binding.command, &binding.arg);
                self.shift_translated = false;
            }
            Resolved::Unbound => {
                let mut keys = std::mem::take(&mut self.pending_keys);
                keys.push(key);
                match layers.first().and_then(|&layer| self.keymaps.fallback(layer)).filter(|_| keys.len() > 1) {
                    // An unbound sequence in a passthrough layer goes on to its program.
                    Some(fallback) => {
                        for key in keys {
                            self.execute_command(fallback, &Arg::Key(key));
                        }
                    }
                    None if keys.len() > 1 => self.set_status(format!("{} is undefined", format_seq(&keys))),
                    None => {}
                }
            }
        }
    }

    /// Keymap layers receiving keys: the top modal alone, else the active buffer's.
    pub fn active_layers(&self) -> Vec<KeymapId> {
        match self.modals.last() {
            Some(modal) => vec![modal.keymap()],
            None => self.buffer_layers(self.active_buffer_id()),
        }
    }

    /// The layers of buffer `id`: its minor keymaps (latest first), its mode's, then global.
    pub fn buffer_layers(&self, id: BufferId) -> Vec<KeymapId> {
        let buf = &self.buffers[id];
        let minor = buf.minor_keymaps().iter().rev().copied();
        minor.chain(buf.mode().keymap).chain([KeymapId::GLOBAL]).collect()
    }

    pub fn pending_keys(&self) -> &[Key] {
        &self.pending_keys
    }

    /// Runs the command named `name` as if invoked by a key. Returns false if unknown.
    pub fn execute(&mut self, name: &str) -> bool {
        self.execute_with(name, Arg::None)
    }

    pub fn execute_with(&mut self, name: &str, arg: Arg) -> bool {
        match self.commands.id(name) {
            Some(id) => {
                self.execute_command(id, &arg);
                true
            }
            None => {
                self.set_status(format!("Unknown command '{}'", name));
                false
            }
        }
    }

    pub fn execute_command(&mut self, id: CommandId, arg: &Arg) {
        let buffer = self.active_buffer_id();
        self.this_chain = None;
        self.call(id, arg);
        let continues_undo = self.this_chain.as_ref().is_some_and(|c| c.is::<UndoChain>());
        if !continues_undo {
            if let Some(buf) = self.buffers.get_mut(buffer) {
                buf.break_undo_chain();
            }
        }
        self.after_input_command(continues_undo);
        self.last_chain = self.this_chain.take();
        self.last_command = Some(id);
        for hook in self.hooks.post_command.clone() {
            hook(self);
        }
    }

    /// Invokes a command's handler directly, without command-loop bookkeeping. Use this
    /// when one command delegates to another.
    pub fn call(&mut self, id: CommandId, arg: &Arg) {
        let handler = self.commands.handler(id);
        handler(self, arg);
    }

    /// The command that ran before the current one (Emacs' `last-command`).
    pub fn last_command(&self) -> Option<CommandId> {
        self.last_command
    }

    /// The chain value of type `T` the previous command left with `set_chain`, if any.
    /// Commands use it to continue what the one before them did: consecutive kills merge,
    /// yank-pop replaces the previous yank, `C-l` cycles, undos keep walking back.
    pub fn last_chain<T: 'static>(&self) -> Option<&T> {
        self.last_chain.as_ref()?.downcast_ref()
    }

    /// Leaves `value` for the next command to find with `last_chain`.
    pub fn set_chain<T: 'static>(&mut self, value: T) {
        self.this_chain = Some(Box::new(value));
    }

    /// Whether the current command was reached by dropping Shift from the key.
    pub fn shift_translated(&self) -> bool {
        self.shift_translated
    }

    /// Ends command chains after non-command input (mouse).
    fn interrupt_chain(&mut self) {
        self.last_chain = None;
        let buf = self.active_buffer_mut();
        buf.end_edit_group();
        buf.break_undo_chain();
    }

    // ---------------------------------------------------------------------------------
    // Background work
    // ---------------------------------------------------------------------------------

    /// Runs `job` on a background thread; it reports back with `JobContext::send`.
    pub fn spawn(&self, job: impl FnOnce(JobContext) + Send + 'static) -> JobHandle {
        self.scheduler.spawn(job)
    }

    /// A job context for work driven by threads the caller manages itself (e.g. a
    /// library's event loop), without spawning one.
    pub fn job_context(&self) -> (JobHandle, JobContext) {
        self.scheduler.context()
    }

    /// Called from job threads when results arrive, so the frontend can wake and `poll`.
    pub fn set_waker(&self, wake: impl Fn() + Send + Sync + 'static) {
        self.scheduler.set_waker(Arc::new(wake));
    }

    /// Runs `run` on the UI thread every `interval`; it returns whether it changed anything.
    pub fn add_timer(&mut self, interval: Duration, run: impl Fn(&mut Editor) -> bool + 'static) {
        self.scheduler.add_timer(interval, run);
    }

    /// Runs `run` once on the UI thread after `delay`.
    pub fn after(&mut self, delay: Duration, run: impl FnOnce(&mut Editor) + 'static) {
        self.scheduler.add_timeout(delay, run);
    }

    /// Runs finished job results and due timers. Returns whether the display may have changed.
    pub fn poll(&mut self, now: Instant) -> bool {
        let results = self.scheduler.take_results();
        let timeouts = self.scheduler.take_due_timeouts(now);
        let mut changed = !results.is_empty() || !timeouts.is_empty();
        for task in results {
            task(self);
        }
        for timeout in timeouts {
            timeout(self);
        }
        for timer in self.scheduler.take_due_timers(now) {
            changed |= timer(self);
        }
        changed
    }

    /// When the next timer is due; frontends can sleep until then.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.scheduler.next_deadline()
    }

    // ---------------------------------------------------------------------------------
    // Mouse
    // ---------------------------------------------------------------------------------

    /// The primary button went down; `clicks` counts quick presses in the same spot (the
    /// frontend knows the platform's double-click timing). Runs `<mouse-1>`, or
    /// `<double-mouse-1>` from the second click on, in the window under the pointer.
    pub fn handle_mouse_press(&mut self, x: f32, y: f32, clicks: u32) {
        let Some(view) = self.layout.view_at(x, y) else { return };
        let code = if clicks >= 2 { KeyCode::DoubleMouse1 } else { KeyCode::Mouse1 };
        self.run_pointer_key(view, code, &Arg::Point(x, y));
    }

    /// The pointer moved with the primary button held: `<drag-mouse-1>` in the window the
    /// press activated, even once the pointer leaves it.
    pub fn handle_mouse_drag(&mut self, x: f32, y: f32) {
        let view = self.layout.active_id();
        self.run_pointer_key(view, KeyCode::DragMouse1, &Arg::Point(x, y));
    }

    /// Scrolls the view under the pointer. A mode can take over by binding `<wheel-up>` /
    /// `<wheel-down>`; its command runs in that view with `Arg::Int(lines)`.
    pub fn handle_scroll(&mut self, x: f32, y: f32, delta_lines: isize) {
        let view = self.layout.view_at(x, y).unwrap_or(self.layout.active_id());
        let code = if delta_lines < 0 { KeyCode::WheelUp } else { KeyCode::WheelDown };
        if self.run_pointer_key(view, code, &Arg::Int(delta_lines.unsigned_abs() as i64)) {
            return;
        }
        self.interrupt_chain();
        if let Some(mut doc) = self.doc_for(view) {
            doc.scroll(delta_lines);
        }
    }

    /// Runs the command bound to a pointer key in `view`'s keymaps (fallbacks, which pass
    /// keys to programs, don't take these) in that window, with `arg`. Returns whether one
    /// was bound.
    fn run_pointer_key(&mut self, view: ViewId, code: KeyCode, arg: &Arg) -> bool {
        let key = [Key::new(code, false, false, false)];
        let layers = self.layout.view(view).map(|v| self.buffer_layers(v.buffer)).unwrap_or_default();
        let Some(binding) = layers.iter().find_map(|&layer| self.keymaps.bound(layer, &key)) else {
            return false;
        };
        self.layout.set_active(view);
        self.execute_command(binding.command, arg);
        true
    }
}
