//! Faces: named text styles.
//!
//! Everything drawn refers to a `FaceId`, never a raw color. Built-in faces have fixed ids
//! (`FaceId::KEYWORD`, ...); plugins register their own with a default. A `Theme` overrides
//! faces by name, and user overrides from `init.rhai` apply on top of the theme.

use std::collections::HashMap;

use crate::frame::{Color, Style};
use crate::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FaceId(u16);

impl Default for FaceId {
    fn default() -> Self {
        FaceId::DEFAULT
    }
}

/// A style where unset colors inherit: text falls back to the `default` face's foreground,
/// backgrounds stay transparent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Face {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    /// Color of the underline; the text color when unset.
    pub underline_color: Option<Color>,
}

impl Face {
    pub const fn fg(fg: Color) -> Self {
        Self { fg: Some(fg), bg: None, bold: false, italic: false, underline: false, underline_color: None }
    }

    pub const fn bg(bg: Color) -> Self {
        Self { fg: None, bg: Some(bg), bold: false, italic: false, underline: false, underline_color: None }
    }

    pub const fn fg_bg(fg: Color, bg: Color) -> Self {
        Self { fg: Some(fg), bg: Some(bg), bold: false, italic: false, underline: false, underline_color: None }
    }

    pub const fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    pub const fn italic(mut self) -> Self {
        self.italic = true;
        self
    }

    pub const fn underline(mut self) -> Self {
        self.underline = true;
        self
    }

    /// Underlined in `color`, leaving the text color alone (e.g. diagnostics).
    pub const fn underline_in(mut self, color: Color) -> Self {
        self.underline = true;
        self.underline_color = Some(color);
        self
    }

    /// `other` layered on top of `self`: its set colors and attributes win.
    pub fn merge(self, other: Face) -> Face {
        Face {
            fg: other.fg.or(self.fg),
            bg: other.bg.or(self.bg),
            bold: self.bold || other.bold,
            italic: self.italic || other.italic,
            underline: self.underline || other.underline,
            underline_color: other.underline_color.or(self.underline_color),
        }
    }
}

macro_rules! builtin_faces {
    ($($id:ident = $name:literal,)*) => {
        #[allow(non_camel_case_types, clippy::upper_case_acronyms)]
        #[repr(u16)]
        enum Builtin { $($id,)* }

        impl FaceId {
            $(pub const $id: FaceId = FaceId(Builtin::$id as u16);)*
        }

        const BUILTIN_NAMES: &[&str] = &[$($name,)*];
    };
}

builtin_faces! {
    DEFAULT = "default",
    CURRENT_LINE = "hl-line",
    REGION = "region",
    CURSOR = "cursor",
    SHADOW = "shadow",
    SEARCH = "search",
    LAZY_HIGHLIGHT = "lazy-highlight",
    MATCH_PAREN = "show-paren-match",
    JUMP_LABEL = "jump-label",
    ERROR = "error",
    WARNING = "warning",
    LINE_NUMBER = "line-number",
    LINE_NUMBER_CURRENT = "line-number-current-line",
    MODE_LINE = "mode-line",
    MODE_LINE_INACTIVE = "mode-line-inactive",
    WINDOW_DIVIDER = "window-divider",
    MINIBUFFER = "minibuffer",
    MINIBUFFER_CURSOR = "minibuffer-cursor",
    MINIBUFFER_MESSAGE = "minibuffer-message",
    POPUP = "popup",
    POPUP_BORDER = "popup-border",
    POPUP_HEADER = "popup-header",
    POPUP_SELECTION = "popup-selection",
    POPUP_PROMPT = "popup-prompt",
    POPUP_INPUT = "popup-input",
    POPUP_SEPARATOR = "popup-separator",
    POPUP_HIGHLIGHT = "popup-highlight",
    POPUP_BACKDROP = "popup-backdrop",
    KEYWORD = "keyword",
    TYPE = "type",
    FUNCTION = "function",
    STRING = "string",
    COMMENT = "comment",
    NUMBER = "number",
    OPERATOR = "operator",
    PUNCTUATION = "punctuation",
    PREPROCESSOR = "preprocessor",
    CONSTANT = "constant",
    HEADING = "heading",
    LINK = "link",
    MARKED = "marked",
    MATCH = "match",
}

pub struct Faces {
    names: Vec<String>,
    by_name: HashMap<String, FaceId>,
    defaults: Vec<Face>,
    /// Effective faces: defaults, then the theme, then user overrides.
    resolved: Vec<Face>,
    user: Vec<(FaceId, Face)>,
}

impl Faces {
    pub fn new(theme: &Theme) -> Self {
        let mut faces = Self {
            names: Vec::new(),
            by_name: HashMap::new(),
            defaults: Vec::new(),
            resolved: Vec::new(),
            user: Vec::new(),
        };
        for name in BUILTIN_NAMES {
            faces.register(name, Face::default());
        }
        faces.apply_theme(theme);
        faces
    }

    /// Registers a face (or updates its default) and returns its id.
    pub fn register(&mut self, name: &str, default: Face) -> FaceId {
        if let Some(&id) = self.by_name.get(name) {
            self.defaults[id.0 as usize] = default;
            return id;
        }
        let id = FaceId(self.names.len() as u16);
        self.names.push(name.to_string());
        self.by_name.insert(name.to_string(), id);
        self.defaults.push(default);
        self.resolved.push(default);
        id
    }

    pub fn id(&self, name: &str) -> Option<FaceId> {
        self.by_name.get(name).copied()
    }

    pub fn name(&self, id: FaceId) -> &str {
        &self.names[id.0 as usize]
    }

    pub fn get(&self, id: FaceId) -> Face {
        self.resolved[id.0 as usize]
    }

    /// Recomputes every face from its default, `theme`, and user overrides.
    pub fn apply_theme(&mut self, theme: &Theme) {
        self.resolved.clone_from(&self.defaults);
        for (name, face) in &theme.faces {
            if let Some(&id) = self.by_name.get(name.as_str()) {
                self.resolved[id.0 as usize] = *face;
            }
        }
        for &(id, face) in &self.user {
            self.resolved[id.0 as usize] = face;
        }
    }

    /// Drops every user override (`reload-init` then applies them afresh).
    pub fn clear_customizations(&mut self) {
        for (id, _) in std::mem::take(&mut self.user) {
            self.resolved[id.0 as usize] = self.defaults[id.0 as usize];
        }
    }

    /// A user override that survives theme switches.
    pub fn customize(&mut self, name: &str, face: Face) -> Result<(), String> {
        let id = self.id(name).ok_or_else(|| format!("Unknown face '{}'", name))?;
        self.user.retain(|(existing, _)| *existing != id);
        self.user.push((id, face));
        self.resolved[id.0 as usize] = face;
        Ok(())
    }

    pub fn fg(&self, id: FaceId) -> Color {
        self.get(id).fg.or(self.get(FaceId::DEFAULT).fg).unwrap_or(Color::WHITE)
    }

    pub fn bg(&self, id: FaceId) -> Color {
        self.get(id).bg.or(self.get(FaceId::DEFAULT).bg).unwrap_or(Color::BLACK)
    }

    /// Text style for `face`: unset foreground inherits `default`, unset background is clear.
    pub fn style(&self, face: Face) -> Style {
        let fg = face.fg.or(self.get(FaceId::DEFAULT).fg).unwrap_or(Color::WHITE);
        Style {
            fg,
            bg: face.bg.unwrap_or(Color::TRANSPARENT),
            bold: face.bold,
            italic: face.italic,
            underline: face.underline.then(|| face.underline_color.unwrap_or(fg)),
        }
    }

    /// Text style for face `id` drawn on its own background.
    pub fn text(&self, id: FaceId) -> Style {
        self.style(self.get(id))
    }
}
