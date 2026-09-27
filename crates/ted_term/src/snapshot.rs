//! View mode: a frozen copy of the scrollback and screen as ordinary buffer text, with the
//! terminal's colors kept as faces so it looks the same while you search and copy.

use std::collections::HashMap;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::Term;
use alacritty_terminal::vte::ansi::Rgb;
use ted_core::{Face, FaceId, Faces, StyledText};

use crate::palette::{color, Palette};
use crate::session::Listener;

/// A snapshot line and its styled spans (start, end, face) in chars.
type StyledLine = (String, Vec<(usize, usize, FaceId)>);

pub struct Snapshot {
    pub text: StyledText,
    /// The terminal cursor as (line, column) in `text`.
    pub cursor: (usize, usize),
    /// The line of `text` shown at the top of the terminal's screen.
    pub screen_top: usize,
}

fn pack(c: Rgb) -> u32 {
    (c.r as u32) << 16 | (c.g as u32) << 8 | c.b as u32
}

/// A face for a terminal style, registered on first use and shared afterwards.
fn face_for(faces: &mut Faces, palette: &Palette, fg: Rgb, bg: Rgb, flags: Flags) -> Option<FaceId> {
    let face = Face {
        fg: (fg != palette.fg).then(|| color(fg)),
        bg: (bg != palette.bg).then(|| color(bg)),
        bold: flags.contains(Flags::BOLD),
        italic: flags.contains(Flags::ITALIC),
        underline: flags.intersects(Flags::ALL_UNDERLINES),
        underline_color: None,
    };
    if face == Face::default() {
        return None;
    }
    let name = format!(
        "term:{:02x}{:02x}{:02x}:{:02x}{:02x}{:02x}:{}{}{}",
        fg.r, fg.g, fg.b, bg.r, bg.g, bg.b, face.bold as u8, face.italic as u8, face.underline as u8
    );
    Some(faces.id(&name).unwrap_or_else(|| faces.register(&name, face)))
}

pub fn take(term: &Term<Listener>, faces: &mut Faces) -> Snapshot {
    let palette = Palette::from_faces(faces);
    let grid = term.grid();
    let (top, bottom) = (grid.topmost_line().0, grid.bottommost_line().0);
    let cursor_line = (grid.cursor.point.line.0 - top) as usize;

    let mut cache: HashMap<(u32, u32, u16), Option<FaceId>> = HashMap::new();
    let mut lines: Vec<StyledLine> = Vec::new();
    for line in top..=bottom {
        let row = &grid[Line(line)];
        let (mut text, mut spans) = (String::new(), Vec::new());
        let (mut styled_len, mut at) = (0, 0);
        let mut current: Option<(usize, FaceId)> = None;
        for col in 0..grid.columns() {
            let cell = &row[Column(col)];
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let (fg, bg) = palette.cell_colors(cell, term.colors());
            let flags = cell.flags & (Flags::BOLD | Flags::ITALIC | Flags::ALL_UNDERLINES);
            let face = if fg == palette.fg && bg == palette.bg && flags.is_empty() {
                None
            } else {
                let key = (pack(fg), pack(bg), flags.bits());
                *cache.entry(key).or_insert_with(|| face_for(faces, &palette, fg, bg, flags))
            };
            match (current, face) {
                (Some((_, f)), Some(g)) if f == g => {}
                _ => {
                    if let Some((start, f)) = current.take() {
                        spans.push((start, at, f));
                    }
                    current = face.map(|f| (at, f));
                }
            }
            text.push(if cell.flags.contains(Flags::HIDDEN) { ' ' } else { cell.c });
            at += 1;
            if face.is_some() || cell.c != ' ' {
                styled_len = at;
            }
        }
        if let Some((start, f)) = current {
            spans.push((start, at, f));
        }
        // Drop unstyled trailing blanks; styled ones (a colored status bar) stay.
        let text: String = text.chars().take(styled_len).collect();
        lines.push((text, spans));
    }
    while lines.len() > cursor_line + 1 && lines.last().is_some_and(|(t, s)| t.is_empty() && s.is_empty()) {
        lines.pop();
    }

    let mut text = StyledText::new();
    for (line, spans) in lines {
        let range = text.push(&line, None);
        for (start, end, face) in spans {
            text.style(range.start + start..range.start + end.min(range.len()), face);
        }
        text.push("\n", None);
    }
    let screen_top = (-top - grid.display_offset() as i32).max(0) as usize;
    Snapshot { text, cursor: (cursor_line, grid.cursor.point.column.0), screen_top }
}
