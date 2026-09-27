//! Keys: raw events delivered by frontends, and the normalized `Key` that keymaps store.
//!
//! Normalization folds Shift into the character for printable keys (`S-,` becomes `<`,
//! `S-z` becomes `Z`) so a binding written as `M-<` matches however the frontend reports it.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    Char(char),
    Enter,
    Backspace,
    Tab,
    Backtab,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
    F(u8),
    WheelUp,
    WheelDown,
    /// Primary button pressed (`<mouse-1>`), pressed again quickly (`<double-mouse-1>`), or
    /// moved while held (`<drag-mouse-1>`). Their commands receive `Arg::Point`.
    Mouse1,
    DoubleMouse1,
    DragMouse1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Modifiers {
    pub const NONE: Self = Self { ctrl: false, alt: false, shift: false };
    pub const CTRL: Self = Self { ctrl: true, alt: false, shift: false };
    pub const ALT: Self = Self { ctrl: false, alt: true, shift: false };
    pub const SHIFT: Self = Self { ctrl: false, alt: false, shift: true };
}

/// A key press as reported by a frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub modifiers: Modifiers,
}

impl KeyEvent {
    pub fn new(code: KeyCode, modifiers: Modifiers) -> Self {
        Self { code, modifiers }
    }

    pub fn plain(code: KeyCode) -> Self {
        Self::new(code, Modifiers::NONE)
    }

    pub fn plain_char(ch: char) -> Self {
        Self::plain(KeyCode::Char(ch))
    }

    pub fn ctrl(ch: char) -> Self {
        Self::new(KeyCode::Char(ch), Modifiers::CTRL)
    }

    pub fn alt(ch: char) -> Self {
        Self::new(KeyCode::Char(ch), Modifiers::ALT)
    }

    pub fn shift(code: KeyCode) -> Self {
        Self::new(code, Modifiers::SHIFT)
    }
}

/// A normalized key, as stored in keymaps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
    /// Only ever set for non-character keys; characters carry shift in the char itself.
    pub shift: bool,
}

const US_UNSHIFTED: &str = "`1234567890-=[]\\;',./";
const US_SHIFTED: &str = "~!@#$%^&*()_+{}|:\"<>?";

fn shift_char(ch: char) -> char {
    if ch.is_ascii_lowercase() {
        return ch.to_ascii_uppercase();
    }
    US_UNSHIFTED.chars().position(|c| c == ch).and_then(|i| US_SHIFTED.chars().nth(i)).unwrap_or(ch)
}

impl Key {
    pub fn new(code: KeyCode, ctrl: bool, alt: bool, shift: bool) -> Self {
        let (code, shift) = match code {
            KeyCode::Char(ch) if shift => (KeyCode::Char(shift_char(ch)), false),
            KeyCode::Char(_) => (code, false),
            KeyCode::Tab if shift => (KeyCode::Backtab, false),
            KeyCode::Backtab => (KeyCode::Backtab, false),
            _ => (code, shift),
        };
        Self { code, ctrl, alt, shift }
    }

    pub fn from_event(ev: KeyEvent) -> Self {
        Self::new(ev.code, ev.modifiers.ctrl, ev.modifiers.alt, ev.modifiers.shift)
    }

    /// Whether this is a mouse button or wheel "key".
    pub fn is_mouse(&self) -> bool {
        matches!(
            self.code,
            KeyCode::WheelUp | KeyCode::WheelDown | KeyCode::Mouse1 | KeyCode::DoubleMouse1 | KeyCode::DragMouse1
        )
    }

    /// The character this key types, if it is a plain printable key.
    pub fn printable(&self) -> Option<char> {
        match self.code {
            KeyCode::Char(ch) if !self.ctrl && !self.alt => Some(ch),
            _ => None,
        }
    }

    /// Shift-translation fallback: the unshifted key to try when this one is unbound.
    /// Emacs does the same, which is why `S-<right>` reaches `forward-char`.
    pub fn unshifted(&self) -> Option<Key> {
        match self.code {
            KeyCode::Char(ch) if ch.is_ascii_uppercase() && (self.ctrl || self.alt) => {
                Some(Key { code: KeyCode::Char(ch.to_ascii_lowercase()), ..*self })
            }
            KeyCode::Char(_) => None,
            _ if self.shift => Some(Key { shift: false, ..*self }),
            _ => None,
        }
    }

    /// Parses a single key in Emacs notation, e.g. `C-x`, `M-<`, `S-<right>`, `RET`.
    pub fn parse(token: &str) -> Result<Key, String> {
        let (mut ctrl, mut alt, mut shift) = (false, false, false);
        let mut rest = token;
        loop {
            if rest.len() > 2 && rest.as_bytes()[1] == b'-' {
                match rest.as_bytes()[0] {
                    b'C' => ctrl = true,
                    b'M' => alt = true,
                    b'S' => shift = true,
                    _ => break,
                }
                rest = &rest[2..];
            } else {
                break;
            }
        }

        let code = match rest {
            "RET" => KeyCode::Enter,
            "TAB" => KeyCode::Tab,
            "SPC" => KeyCode::Char(' '),
            "DEL" => KeyCode::Backspace,
            "ESC" => KeyCode::Escape,
            "<up>" => KeyCode::Up,
            "<down>" => KeyCode::Down,
            "<left>" => KeyCode::Left,
            "<right>" => KeyCode::Right,
            "<home>" => KeyCode::Home,
            "<end>" => KeyCode::End,
            "<prior>" => KeyCode::PageUp,
            "<next>" => KeyCode::PageDown,
            "<delete>" => KeyCode::Delete,
            "<backtab>" => KeyCode::Backtab,
            "<wheel-up>" => KeyCode::WheelUp,
            "<wheel-down>" => KeyCode::WheelDown,
            "<mouse-1>" => KeyCode::Mouse1,
            "<double-mouse-1>" => KeyCode::DoubleMouse1,
            "<drag-mouse-1>" => KeyCode::DragMouse1,
            _ => {
                if let Some(n) = rest.strip_prefix("<f").and_then(|r| r.strip_suffix('>')) {
                    KeyCode::F(n.parse().map_err(|_| format!("Invalid function key '{}'", token))?)
                } else {
                    let mut chars = rest.chars();
                    match (chars.next(), chars.next()) {
                        (Some(ch), None) => KeyCode::Char(ch),
                        _ => return Err(format!("Invalid key '{}'", token)),
                    }
                }
            }
        };
        Ok(Key::new(code, ctrl, alt, shift))
    }

    /// Parses a whitespace-separated key sequence, e.g. `C-x C-f`.
    pub fn parse_seq(seq: &str) -> Result<Vec<Key>, String> {
        let keys = seq.split_whitespace().map(Key::parse).collect::<Result<Vec<_>, _>>()?;
        if keys.is_empty() {
            return Err("Empty key sequence".to_string());
        }
        Ok(keys)
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("C-")?;
        }
        if self.alt {
            f.write_str("M-")?;
        }
        if self.shift {
            f.write_str("S-")?;
        }
        match self.code {
            KeyCode::Char(' ') => f.write_str("SPC"),
            KeyCode::Char(ch) => write!(f, "{}", ch),
            KeyCode::Enter => f.write_str("RET"),
            KeyCode::Tab => f.write_str("TAB"),
            KeyCode::Backspace => f.write_str("DEL"),
            KeyCode::Escape => f.write_str("ESC"),
            KeyCode::Up => f.write_str("<up>"),
            KeyCode::Down => f.write_str("<down>"),
            KeyCode::Left => f.write_str("<left>"),
            KeyCode::Right => f.write_str("<right>"),
            KeyCode::Home => f.write_str("<home>"),
            KeyCode::End => f.write_str("<end>"),
            KeyCode::PageUp => f.write_str("<prior>"),
            KeyCode::PageDown => f.write_str("<next>"),
            KeyCode::Delete => f.write_str("<delete>"),
            KeyCode::Backtab => f.write_str("<backtab>"),
            KeyCode::F(n) => write!(f, "<f{}>", n),
            KeyCode::WheelUp => f.write_str("<wheel-up>"),
            KeyCode::WheelDown => f.write_str("<wheel-down>"),
            KeyCode::Mouse1 => f.write_str("<mouse-1>"),
            KeyCode::DoubleMouse1 => f.write_str("<double-mouse-1>"),
            KeyCode::DragMouse1 => f.write_str("<drag-mouse-1>"),
        }
    }
}

pub fn format_seq(keys: &[Key]) -> String {
    keys.iter().map(|k| k.to_string()).collect::<Vec<_>>().join(" ")
}
