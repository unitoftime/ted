//! Escape sequences that redraw a terminal on an empty one of the same size: its history
//! and screen, then its cursor and the modes programs set. How the host hands a terminal
//! to a ted attaching to it, the way tmux redraws a pane for a client.
//!
//! alacritty keeps the normal screen out of reach while a full-screen program shows the
//! alternate one, so a snapshot taken then has only the alternate screen; ted asks for
//! another once the program leaves it.

use std::fmt::Write as _;

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor};

/// Modes with a `CSI ? n h` / `CSI ? n l` switch, restored as the terminal has them.
const PRIVATE_MODES: &[(TermMode, u16)] = &[
    (TermMode::APP_CURSOR, 1),
    (TermMode::ORIGIN, 6),
    (TermMode::MOUSE_REPORT_CLICK, 1000),
    (TermMode::MOUSE_DRAG, 1002),
    (TermMode::MOUSE_MOTION, 1003),
    (TermMode::FOCUS_IN_OUT, 1004),
    (TermMode::UTF8_MOUSE, 1005),
    (TermMode::SGR_MOUSE, 1006),
    (TermMode::ALTERNATE_SCROLL, 1007),
    (TermMode::BRACKETED_PASTE, 2004),
];

/// Kitty keyboard protocol flags, by their bit in `CSI > flags u`.
const KEYBOARD_FLAGS: &[(TermMode, u8)] = &[
    (TermMode::DISAMBIGUATE_ESC_CODES, 1),
    (TermMode::REPORT_EVENT_TYPES, 2),
    (TermMode::REPORT_ALTERNATE_KEYS, 4),
    (TermMode::REPORT_ALL_KEYS_AS_ESC, 8),
    (TermMode::REPORT_ASSOCIATED_TEXT, 16),
];

/// Cell attributes with their SGR parameter.
const ATTRIBUTES: &[(Flags, &str)] = &[
    (Flags::BOLD, "1"),
    (Flags::DIM, "2"),
    (Flags::ITALIC, "3"),
    (Flags::UNDERLINE, "4"),
    (Flags::DOUBLE_UNDERLINE, "4:2"),
    (Flags::UNDERCURL, "4:3"),
    (Flags::DOTTED_UNDERLINE, "4:4"),
    (Flags::DASHED_UNDERLINE, "4:5"),
    (Flags::INVERSE, "7"),
    (Flags::HIDDEN, "8"),
    (Flags::STRIKEOUT, "9"),
];

const STYLE_FLAGS: Flags = Flags::BOLD
    .union(Flags::DIM)
    .union(Flags::ITALIC)
    .union(Flags::ALL_UNDERLINES)
    .union(Flags::INVERSE)
    .union(Flags::HIDDEN)
    .union(Flags::STRIKEOUT);

/// The pen a cell was written with: what SGR sets.
#[derive(Clone, Copy, PartialEq)]
struct Pen {
    fg: Color,
    bg: Color,
    flags: Flags,
}

impl Pen {
    const DEFAULT: Pen = Pen {
        fg: Color::Named(NamedColor::Foreground),
        bg: Color::Named(NamedColor::Background),
        flags: Flags::empty(),
    };

    fn of(cell: &Cell) -> Pen {
        Pen { fg: cell.fg, bg: cell.bg, flags: cell.flags & STYLE_FLAGS }
    }

    /// `CSI 0 ; ... m` switching any pen to this one.
    fn write(self, out: &mut String) {
        out.push_str("\x1b[0");
        for (flag, param) in ATTRIBUTES {
            if self.flags.contains(*flag) {
                let _ = write!(out, ";{}", param);
            }
        }
        color(out, self.fg, 30);
        color(out, self.bg, 40);
        out.push('m');
    }
}

/// The SGR parameters for `color` as a foreground (`base` 30) or background (`base` 40).
fn color(out: &mut String, color: Color, base: u8) {
    let _ = match color {
        Color::Named(named) => match named as usize {
            n @ 0..=7 => write!(out, ";{}", base as usize + n),
            n @ 8..=15 => write!(out, ";{}", base as usize + 60 + n - 8),
            _ => Ok(()),
        },
        Color::Indexed(index) => write!(out, ";{};5;{}", base + 8, index),
        Color::Spec(rgb) => write!(out, ";{};2;{};{};{}", base + 8, rgb.r, rgb.g, rgb.b),
    };
}

/// Draws `term`'s history (on the normal screen) and screen, cursor and modes.
pub fn snapshot<T: EventListener>(term: &Term<T>) -> Vec<u8> {
    let mode = *term.mode();
    let grid = term.grid();
    let mut out = String::new();
    if mode.contains(TermMode::ALT_SCREEN) {
        out.push_str("\x1b[?1049h");
    }
    let (top, bottom) = (grid.topmost_line().0, grid.bottommost_line().0);
    let mut pen = Pen::DEFAULT;
    for line in top..=bottom {
        let row = &grid[Line(line)];
        let wraps = row[Column(grid.columns() - 1)].flags.contains(Flags::WRAPLINE);
        // A wrapped line fills the row so the next one continues it, as it was written.
        let end = if wraps {
            grid.columns()
        } else {
            (0..grid.columns()).rev().find(|&col| !blank(&row[Column(col)])).map_or(0, |col| col + 1)
        };
        for col in 0..end {
            let cell = &row[Column(col)];
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            if Pen::of(cell) != pen {
                pen = Pen::of(cell);
                pen.write(&mut out);
            }
            out.push(cell.c);
            out.extend(cell.zerowidth().into_iter().flatten());
        }
        if !wraps && line < bottom {
            if pen != Pen::DEFAULT {
                pen = Pen::DEFAULT;
                out.push_str("\x1b[0m");
            }
            out.push_str("\r\n");
        }
    }

    let cursor = &grid.cursor;
    let _ = write!(out, "\x1b[{};{}H", cursor.point.line.0 + 1, cursor.point.column.0 + 1);
    Pen::of(&cursor.template).write(&mut out);
    for (flag, number) in PRIVATE_MODES {
        if mode.contains(*flag) {
            let _ = write!(out, "\x1b[?{}h", number);
        }
    }
    if !mode.contains(TermMode::SHOW_CURSOR) {
        out.push_str("\x1b[?25l");
    }
    if !mode.contains(TermMode::LINE_WRAP) {
        out.push_str("\x1b[?7l");
    }
    if mode.contains(TermMode::APP_KEYPAD) {
        out.push_str("\x1b=");
    }
    if mode.contains(TermMode::INSERT) {
        out.push_str("\x1b[4h");
    }
    if mode.contains(TermMode::LINE_FEED_NEW_LINE) {
        out.push_str("\x1b[20h");
    }
    let keyboard: u8 = KEYBOARD_FLAGS.iter().filter(|(flag, _)| mode.contains(*flag)).map(|(_, bit)| bit).sum();
    if keyboard != 0 {
        let _ = write!(out, "\x1b[>{}u", keyboard);
    }
    out.into_bytes()
}

/// Whether a cell shows nothing: a space on the default background, without underlines
/// or inverse video.
fn blank(cell: &Cell) -> bool {
    cell.c == ' '
        && cell.bg == Color::Named(NamedColor::Background)
        && !cell.flags.intersects(Flags::ALL_UNDERLINES | Flags::INVERSE)
        && cell.zerowidth().is_none()
}
