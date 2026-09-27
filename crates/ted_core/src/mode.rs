//! Major modes: per-filetype settings plus a keymap layer.
//!
//! A mode is data: comment syntax, indentation, grammar, file matching, and a keymap whose
//! bindings shadow the global map in buffers using the mode. Mode-specific behavior (dired's
//! RET, markdown list continuation) is just commands bound in that keymap.
//!
//! `Editor::define_mode` registers a mode together with its keymap, which is named after
//! the mode in kebab-case ("Git Log" -> `git-log`), so `init.rhai` binds keys in a mode
//! with `bind_mode("git-log", ...)` and changes its settings with `mode("git-log", ...)`.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use crate::face::FaceId;
use crate::keymap::KeymapId;
use crate::syntax::Grammar;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndentStyle {
    Spaces,
    Tabs,
}

/// How a buffer indents when its mode leaves it open: the editor's `tab_width` and
/// `indent` settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Indentation {
    pub width: usize,
    pub style: IndentStyle,
}

impl Default for Indentation {
    fn default() -> Self {
        Self { width: 4, style: IndentStyle::Spaces }
    }
}

pub type GrammarLoader = fn() -> Option<Arc<Grammar>>;

/// Chooses a face for a whole line from its text (drawn instead of syntax highlighting).
pub type LineFace = Arc<dyn Fn(&str) -> Option<FaceId> + Send + Sync>;

#[derive(Clone)]
pub struct Mode {
    pub name: String,
    pub comment_prefix: String,
    /// Set only where the language dictates it (Makefiles use tabs); otherwise buffers
    /// follow the editor's `tab_width` / `indent` settings.
    pub tab_width: Option<usize>,
    pub indent: Option<IndentStyle>,
    pub read_only: bool,
    pub extensions: Vec<String>,
    /// Exact file names (case-insensitive), e.g. `Makefile`.
    pub file_names: Vec<String>,
    pub grammar: Option<GrammarLoader>,
    /// The mode's keymap, set by `Editor::define_mode`.
    pub keymap: Option<KeymapId>,
    /// Name of a keymap the mode's keymap inherits unbound keys from.
    pub parent_keymap: Option<String>,
    /// Name of the command `revert-buffer` runs in this mode instead of reloading from disk.
    pub revert: Option<String>,
    /// Name of the command that recreates a generated buffer of this mode when ted restarts
    /// (`reload-ted`). It runs with a buffer in the saved working directory active and must
    /// show its buffer in the active window before it returns.
    pub restore: Option<String>,
    /// Faces for whole lines, e.g. dimmed checked markdown tasks.
    pub line_face: Option<LineFace>,
    /// Colors of the text and background instead of `default`'s, e.g. a terminal's own.
    pub face: Option<FaceId>,
    pub line_numbers: bool,
    /// Whether the cursor's line is highlighted (`hl-line`).
    pub highlight_line: bool,
    /// How `mode-help` groups the mode's commands: headings over command names, in order.
    /// Commands left out are listed under their keymap.
    pub help_groups: Vec<(String, Vec<String>)>,
}

impl Mode {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            comment_prefix: "// ".to_string(),
            tab_width: None,
            indent: None,
            read_only: false,
            extensions: Vec::new(),
            file_names: Vec::new(),
            grammar: None,
            keymap: None,
            parent_keymap: None,
            revert: None,
            restore: None,
            line_face: None,
            face: None,
            line_numbers: true,
            highlight_line: true,
            help_groups: Vec::new(),
        }
    }

    pub fn comment(mut self, prefix: &str) -> Self {
        self.comment_prefix = prefix.to_string();
        self
    }

    pub fn tab_width(mut self, width: usize) -> Self {
        self.tab_width = Some(width);
        self
    }

    pub fn indent(mut self, style: IndentStyle) -> Self {
        self.indent = Some(style);
        self
    }

    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }

    pub fn extensions(mut self, exts: &[&str]) -> Self {
        self.extensions = exts.iter().map(|e| e.to_string()).collect();
        self
    }

    pub fn file_names(mut self, names: &[&str]) -> Self {
        self.file_names = names.iter().map(|n| n.to_string()).collect();
        self
    }

    pub fn grammar(mut self, loader: GrammarLoader) -> Self {
        self.grammar = Some(loader);
        self
    }

    /// Keys the mode doesn't bind fall through to the keymap named `parent` (e.g. list
    /// modes inherit `locations`).
    pub fn parent_keymap(mut self, parent: &str) -> Self {
        self.parent_keymap = Some(parent.to_string());
        self
    }

    /// A generated read-only buffer's mode (a listing, log or help text): its keymap
    /// inherits `special`, where `h` shows the mode's keys, `n` / `p` step between rows,
    /// `g` refreshes and `q` quits. A parent set with `parent_keymap` is kept (and should
    /// itself inherit `special`).
    pub fn special(mut self) -> Self {
        self.read_only = true;
        self.parent_keymap.get_or_insert_with(|| "special".to_string());
        self
    }

    /// Lists `commands` under `heading` in `mode-help`, in this order.
    pub fn help_group(mut self, heading: &str, commands: &[&str]) -> Self {
        self.help_groups.push((heading.to_string(), commands.iter().map(|c| c.to_string()).collect()));
        self
    }

    /// Makes `revert-buffer` (`g`) run `command` in this mode.
    pub fn revert(mut self, command: &str) -> Self {
        self.revert = Some(command.to_string());
        self
    }

    /// Makes a restart recreate this mode's buffers by running `command` (see `restore`).
    pub fn restore(mut self, command: &str) -> Self {
        self.restore = Some(command.to_string());
        self
    }

    pub fn face(mut self, face: FaceId) -> Self {
        self.face = Some(face);
        self
    }

    pub fn line_numbers(mut self, show: bool) -> Self {
        self.line_numbers = show;
        self
    }

    pub fn highlight_line(mut self, highlight: bool) -> Self {
        self.highlight_line = highlight;
        self
    }

    pub fn line_face(mut self, f: impl Fn(&str) -> Option<FaceId> + Send + Sync + 'static) -> Self {
        self.line_face = Some(Arc::new(f));
        self
    }

    /// The name of the mode's keymap: its name in kebab-case.
    pub fn keymap_name(&self) -> String {
        keymap_name(&self.name)
    }

    /// Whether files at `path` use this mode.
    pub fn matches(&self, path: &Path) -> bool {
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if self.file_names.iter().any(|n| n.eq_ignore_ascii_case(file_name)) {
            return true;
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        !ext.is_empty() && self.extensions.iter().any(|e| e == ext)
    }

    /// The mode used when nothing more specific matches.
    pub fn fundamental() -> Arc<Mode> {
        static FUNDAMENTAL: OnceLock<Arc<Mode>> = OnceLock::new();
        FUNDAMENTAL.get_or_init(|| Arc::new(Mode::new("Plain Text"))).clone()
    }
}

/// The keymap name of a mode: "Git Log" -> "git-log".
pub(crate) fn keymap_name(mode: &str) -> String {
    mode.split_whitespace().map(str::to_lowercase).collect::<Vec<_>>().join("-")
}

/// Whether two mode names are the same ignoring case, with `-` standing for spaces.
fn same_name(a: &str, b: &str) -> bool {
    fn words(s: &str) -> impl Iterator<Item = &str> {
        s.split(|c: char| c.is_whitespace() || c == '-').filter(|w| !w.is_empty())
    }
    let (mut a, mut b) = (words(a), words(b));
    loop {
        match (a.next(), b.next()) {
            (None, None) => return true,
            (Some(x), Some(y)) if x.eq_ignore_ascii_case(y) => {}
            _ => return false,
        }
    }
}

/// User changes to a mode's settings (`mode(...)` in `init.rhai`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModeOverrides {
    pub tab_width: Option<usize>,
    pub indent: Option<IndentStyle>,
    pub comment: Option<String>,
    pub extensions: Option<Vec<String>>,
    pub line_numbers: Option<bool>,
    pub highlight_line: Option<bool>,
}

impl ModeOverrides {
    pub fn apply(&self, mode: &mut Mode) {
        if let Some(width) = self.tab_width {
            mode.tab_width = Some(width);
        }
        if let Some(indent) = self.indent {
            mode.indent = Some(indent);
        }
        if let Some(comment) = &self.comment {
            mode.comment_prefix.clone_from(comment);
        }
        if let Some(extensions) = &self.extensions {
            mode.extensions.clone_from(extensions);
        }
        if let Some(show) = self.line_numbers {
            mode.line_numbers = show;
        }
        if let Some(highlight) = self.highlight_line {
            mode.highlight_line = highlight;
        }
    }
}

#[derive(Default, Clone)]
pub struct ModeRegistry {
    modes: Vec<Arc<Mode>>,
}

impl ModeRegistry {
    /// Registers `mode`, replacing any mode with the same name. `Editor::define_mode` is
    /// the public way in, which also creates the mode's keymap.
    pub(crate) fn register(&mut self, mode: Mode) -> Arc<Mode> {
        let mode = Arc::new(mode);
        match self.modes.iter_mut().find(|m| m.name == mode.name) {
            Some(slot) => *slot = mode.clone(),
            None => self.modes.push(mode.clone()),
        }
        mode
    }

    /// The mode called `name`, which may also be given as its keymap name (`git-log`).
    pub fn get(&self, name: &str) -> Option<Arc<Mode>> {
        self.modes.iter().find(|m| same_name(&m.name, name)).cloned()
    }

    pub fn for_path(&self, path: Option<&Path>) -> Arc<Mode> {
        path.and_then(|p| self.modes.iter().find(|m| m.matches(p)).cloned()).unwrap_or_else(Mode::fundamental)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<Mode>> {
        self.modes.iter()
    }
}
