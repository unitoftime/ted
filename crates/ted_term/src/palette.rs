//! Resolving terminal colors: the 16 ANSI colors (`term-color-0`..`15`) and the default
//! foreground/background (`term-default`) are faces so themes can restyle them. The terminal
//! keeps its own black background rather than following the editor theme.

use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color as TermColor, NamedColor, Rgb};
use ted_core::{Color, Face, FaceId, Faces};

/// xterm's default ANSI palette, slightly softened.
const ANSI: [(u8, u8, u8); 16] = [
    (0x2e, 0x34, 0x36),
    (0xcc, 0x00, 0x00),
    (0x4e, 0x9a, 0x06),
    (0xc4, 0xa0, 0x00),
    (0x34, 0x65, 0xa4),
    (0x75, 0x50, 0x7b),
    (0x06, 0x98, 0x9a),
    (0xd3, 0xd7, 0xcf),
    (0x55, 0x57, 0x53),
    (0xef, 0x29, 0x29),
    (0x8a, 0xe2, 0x34),
    (0xfc, 0xe9, 0x4f),
    (0x72, 0x9f, 0xcf),
    (0xad, 0x7f, 0xa8),
    (0x34, 0xe2, 0xe2),
    (0xee, 0xee, 0xec),
];

/// Default text on a black background, like a traditional terminal.
const DEFAULT_FG: Color = Color::rgb(0xd3, 0xd7, 0xcf);
const DEFAULT_BG: Color = Color::rgb(0, 0, 0);
const DEFAULT_FACE: &str = "term-default";

/// Registers the terminal's faces, returning `term-default`.
pub fn register_faces(faces: &mut Faces) -> FaceId {
    let default = faces.register(DEFAULT_FACE, Face::fg_bg(DEFAULT_FG, DEFAULT_BG));
    for (i, (r, g, b)) in ANSI.iter().enumerate() {
        faces.register(&format!("term-color-{}", i), Face::fg(Color::rgb(*r, *g, *b)));
    }
    default
}

/// Resolved colors for one frame.
pub struct Palette {
    ansi: [Rgb; 16],
    pub fg: Rgb,
    pub bg: Rgb,
}

pub fn rgb(c: Color) -> Rgb {
    Rgb { r: c.r, g: c.g, b: c.b }
}

pub fn color(c: Rgb) -> Color {
    Color::rgb(c.r, c.g, c.b)
}

fn dim(c: Rgb) -> Rgb {
    Rgb { r: (c.r as u16 * 2 / 3) as u8, g: (c.g as u16 * 2 / 3) as u8, b: (c.b as u16 * 2 / 3) as u8 }
}

impl Palette {
    pub fn from_faces(faces: &Faces) -> Self {
        let ansi = std::array::from_fn(|i| {
            faces
                .id(&format!("term-color-{}", i))
                .map_or_else(|| rgb(Color::rgb(ANSI[i].0, ANSI[i].1, ANSI[i].2)), |id| rgb(faces.fg(id)))
        });
        let default = faces.id(DEFAULT_FACE).map(|id| faces.get(id)).unwrap_or_default();
        Self { ansi, fg: rgb(default.fg.unwrap_or(DEFAULT_FG)), bg: rgb(default.bg.unwrap_or(DEFAULT_BG)) }
    }

    /// Color for an xterm palette index (0-255), or 256/257 for the default fg/bg.
    pub fn indexed_rgb(&self, index: usize) -> Rgb {
        match index {
            0..=15 => self.ansi[index],
            16..=231 => {
                let i = index - 16;
                let level = |v: usize| if v == 0 { 0 } else { (55 + v * 40) as u8 };
                Rgb { r: level(i / 36), g: level(i / 6 % 6), b: level(i % 6) }
            }
            232..=255 => {
                let v = (8 + (index - 232) * 10) as u8;
                Rgb { r: v, g: v, b: v }
            }
            257 => self.bg,
            _ => self.fg,
        }
    }

    fn resolve(&self, c: TermColor, overrides: &Colors) -> Rgb {
        match c {
            TermColor::Spec(rgb) => rgb,
            TermColor::Indexed(i) => overrides[i as usize].unwrap_or_else(|| self.indexed_rgb(i as usize)),
            TermColor::Named(named) => {
                let index = named as usize;
                if let Some(rgb) = overrides[index] {
                    return rgb;
                }
                match named {
                    NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::Cursor => self.fg,
                    NamedColor::Background => self.bg,
                    NamedColor::DimForeground => dim(self.fg),
                    n if (n as usize) < 16 => self.ansi[n as usize],
                    // Dim variants follow the 16 named colors in order.
                    n => dim(self.ansi[(n as usize - NamedColor::DimBlack as usize) % 8]),
                }
            }
        }
    }

    /// Foreground and background of a cell after inverse/dim/hidden attributes.
    pub fn cell_colors(&self, cell: &Cell, overrides: &Colors) -> (Rgb, Rgb) {
        let mut fg = self.resolve(cell.fg, overrides);
        let mut bg = self.resolve(cell.bg, overrides);
        if cell.flags.contains(Flags::DIM) {
            fg = dim(fg);
        }
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cell.flags.contains(Flags::HIDDEN) {
            fg = bg;
        }
        (fg, bg)
    }
}
