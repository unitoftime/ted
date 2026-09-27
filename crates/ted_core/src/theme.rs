//! Themes: named sets of face overrides.

use crate::face::Face;
use crate::frame::Color;

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub name: String,
    pub faces: Vec<(String, Face)>,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::rgb(r, g, b)
}

const TANGO_DARK: &[(&str, Face)] = &[
    ("default", Face::fg_bg(rgb(238, 238, 236), rgb(46, 52, 54))),
    ("hl-line", Face::bg(rgb(55, 62, 65))),
    ("region", Face::bg(rgb(72, 85, 96))),
    ("cursor", Face::bg(rgb(245, 121, 0))),
    ("shadow", Face::fg(rgb(136, 138, 133))),
    ("search", Face::bg(rgb(140, 110, 40))),
    ("lazy-highlight", Face::bg(rgb(70, 75, 50))),
    ("show-paren-match", Face::bg(rgb(78, 105, 130)).bold()),
    ("jump-label", Face::fg(rgb(255, 70, 70)).bold()),
    ("error", Face::fg(rgb(239, 41, 41))),
    ("warning", Face::fg(rgb(252, 175, 62))),
    ("line-number", Face::fg_bg(rgb(136, 138, 133), rgb(38, 43, 45))),
    ("line-number-current-line", Face::fg(rgb(252, 233, 79)).bold()),
    ("mode-line", Face::fg_bg(rgb(238, 238, 236), rgb(32, 74, 135)).bold()),
    ("mode-line-inactive", Face::fg_bg(rgb(238, 238, 236), rgb(35, 40, 42))),
    ("window-divider", Face::bg(rgb(60, 68, 70))),
    ("minibuffer", Face::fg_bg(rgb(238, 238, 236), rgb(35, 40, 42))),
    ("minibuffer-cursor", Face::bg(rgb(252, 233, 79))),
    ("minibuffer-message", Face::fg(rgb(114, 159, 207))),
    ("popup", Face::fg_bg(rgb(238, 238, 236), rgb(40, 45, 48))),
    ("popup-border", Face::bg(rgb(52, 101, 164))),
    ("popup-header", Face::fg_bg(Color::WHITE, rgb(32, 74, 135)).bold()),
    ("popup-selection", Face::fg_bg(Color::WHITE, rgb(52, 101, 164)).bold()),
    ("popup-prompt", Face::fg(rgb(114, 159, 207)).bold()),
    ("popup-input", Face::fg(Color::WHITE)),
    ("popup-separator", Face::bg(rgb(45, 55, 65))),
    ("popup-highlight", Face::fg(rgb(255, 220, 100)).bold()),
    ("popup-backdrop", Face::bg(Color::rgba(0, 0, 0, 120))),
    ("keyword", Face::fg(rgb(114, 159, 207)).bold()),
    ("type", Face::fg(rgb(142, 200, 142))),
    ("function", Face::fg(rgb(252, 175, 62))),
    ("string", Face::fg(rgb(233, 185, 110))),
    ("comment", Face::fg(rgb(136, 138, 133))),
    ("number", Face::fg(rgb(173, 127, 168))),
    ("operator", Face::fg(rgb(211, 215, 207))),
    ("punctuation", Face::fg(rgb(186, 189, 182))),
    ("preprocessor", Face::fg(rgb(252, 233, 79))),
    ("constant", Face::fg(rgb(173, 127, 168))),
    ("heading", Face::fg(rgb(252, 215, 60)).bold()),
    ("link", Face::fg(rgb(114, 159, 207)).underline()),
    ("marked", Face::fg(rgb(252, 175, 62)).bold()),
    ("match", Face::fg(rgb(252, 233, 79)).bold()),
];

const DARK: &[(&str, Face)] = &[
    ("default", Face::fg_bg(rgb(220, 225, 230), rgb(20, 22, 26))),
    ("hl-line", Face::bg(rgb(28, 31, 36))),
    ("region", Face::bg(rgb(45, 68, 100))),
    ("cursor", Face::bg(Color::WHITE)),
    ("shadow", Face::fg(rgb(110, 118, 128))),
    ("search", Face::bg(rgb(140, 110, 40))),
    ("lazy-highlight", Face::bg(rgb(70, 75, 50))),
    ("show-paren-match", Face::bg(rgb(50, 80, 110)).bold()),
    ("jump-label", Face::fg(rgb(255, 80, 80)).bold()),
    ("error", Face::fg(rgb(240, 80, 80))),
    ("warning", Face::fg(rgb(230, 180, 80))),
    ("line-number", Face::fg_bg(rgb(90, 100, 115), rgb(24, 26, 30))),
    ("line-number-current-line", Face::fg(rgb(220, 220, 120)).bold()),
    ("mode-line", Face::fg_bg(rgb(240, 245, 250), rgb(45, 55, 70)).bold()),
    ("mode-line-inactive", Face::fg_bg(rgb(240, 245, 250), rgb(30, 34, 40))),
    ("window-divider", Face::bg(rgb(55, 60, 72))),
    ("minibuffer", Face::fg_bg(rgb(230, 235, 240), rgb(18, 20, 24))),
    ("minibuffer-cursor", Face::bg(rgb(240, 200, 60))),
    ("minibuffer-message", Face::fg(rgb(140, 175, 210))),
    ("popup", Face::fg_bg(rgb(220, 225, 230), rgb(22, 24, 28))),
    ("popup-border", Face::bg(rgb(70, 95, 130))),
    ("popup-header", Face::fg_bg(Color::WHITE, rgb(35, 48, 70)).bold()),
    ("popup-selection", Face::fg_bg(Color::WHITE, rgb(45, 68, 100)).bold()),
    ("popup-prompt", Face::fg(rgb(100, 160, 255)).bold()),
    ("popup-input", Face::fg(Color::WHITE)),
    ("popup-separator", Face::bg(rgb(45, 55, 65))),
    ("popup-highlight", Face::fg(rgb(255, 220, 100)).bold()),
    ("popup-backdrop", Face::bg(Color::rgba(0, 0, 0, 120))),
    ("keyword", Face::fg(rgb(197, 134, 192)).bold()),
    ("type", Face::fg(rgb(78, 201, 176))),
    ("function", Face::fg(rgb(220, 220, 170))),
    ("string", Face::fg(rgb(206, 145, 120))),
    ("comment", Face::fg(rgb(106, 153, 85))),
    ("number", Face::fg(rgb(181, 206, 168))),
    ("operator", Face::fg(rgb(212, 212, 212))),
    ("punctuation", Face::fg(rgb(160, 165, 175))),
    ("preprocessor", Face::fg(rgb(156, 220, 254))),
    ("constant", Face::fg(rgb(86, 156, 214))),
    ("heading", Face::fg(rgb(250, 195, 80)).bold()),
    ("link", Face::fg(rgb(100, 180, 255)).underline()),
    ("marked", Face::fg(rgb(220, 180, 80)).bold()),
    ("match", Face::fg(rgb(255, 205, 90)).bold()),
];

/// The built-in themes, the values the `theme` setting accepts.
pub const NAMES: &[&str] = &["tango-dark", "dark"];
const FACES: [&[(&str, Face)]; NAMES.len()] = [TANGO_DARK, DARK];

impl Theme {
    pub fn by_name(name: &str) -> Option<Theme> {
        let index = NAMES.iter().position(|n| *n == name)?;
        Some(Theme { name: name.to_string(), faces: FACES[index].iter().map(|(n, f)| (n.to_string(), *f)).collect() })
    }

    /// The name of the built-in theme after `name`, wrapping around.
    pub fn next_name(name: &str) -> &'static str {
        let index = NAMES.iter().position(|n| *n == name).map_or(0, |i| (i + 1) % NAMES.len());
        NAMES[index]
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::by_name("tango-dark").expect("builtin theme exists")
    }
}
