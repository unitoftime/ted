//! Encoding keys as the bytes a terminal program expects (xterm conventions), including
//! the kitty keyboard protocol's "disambiguate" level when a program turns it on.

use alacritty_terminal::term::TermMode;
use ted_core::{Key, KeyCode};

/// The xterm modifier parameter: 1 + shift + 2·alt + 4·ctrl.
fn modifier_param(key: Key, shift: bool) -> u8 {
    1 + shift as u8 + 2 * key.alt as u8 + 4 * key.ctrl as u8
}

/// The control character for Ctrl+`c`, as terminals send it.
fn ctrl_byte(c: char) -> Option<u8> {
    Some(match c.to_ascii_lowercase() {
        c @ 'a'..='z' => c as u8 & 0x1f,
        '@' | ' ' | '2' => 0,
        '[' | '3' => 27,
        '\\' | '4' => 28,
        ']' | '5' => 29,
        '^' | '6' => 30,
        '_' | '/' | '-' | '7' => 31,
        '?' | '8' => 127,
        _ => return None,
    })
}

pub fn encode(key: Key, mode: TermMode) -> Option<Vec<u8>> {
    let kitty = mode.contains(TermMode::DISAMBIGUATE_ESC_CODES);
    let has_mods = key.ctrl || key.alt || key.shift;
    let esc = |s: &str| Some(format!("\x1b{}", s).into_bytes());
    let csi_u = |code: u32, shift: bool| match modifier_param(key, shift) {
        1 => esc(&format!("[{}u", code)),
        m => esc(&format!("[{};{}u", code, m)),
    };

    // Cursor-style keys: `CSI 1;m X` with modifiers, SS3 in application cursor mode.
    let cursor = |final_char: char| {
        if has_mods {
            esc(&format!("[1;{}{}", modifier_param(key, key.shift), final_char))
        } else if mode.contains(TermMode::APP_CURSOR) {
            esc(&format!("O{}", final_char))
        } else {
            esc(&format!("[{}", final_char))
        }
    };
    // Tilde keys: `CSI n ~`, or `CSI n;m ~` with modifiers.
    let tilde = |n: u8| {
        if has_mods {
            esc(&format!("[{};{}~", n, modifier_param(key, key.shift)))
        } else {
            esc(&format!("[{}~", n))
        }
    };

    match key.code {
        KeyCode::Char(c) => {
            let shifted = c.is_ascii_uppercase();
            if kitty && (key.ctrl || key.alt) {
                return csi_u(c.to_ascii_lowercase() as u32, shifted);
            }
            let mut out = Vec::new();
            if key.alt {
                out.push(0x1b);
            }
            match ctrl_byte(c).filter(|_| key.ctrl) {
                Some(b) => out.push(b),
                None => out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
            }
            Some(out)
        }
        KeyCode::Enter | KeyCode::Tab | KeyCode::Backspace if kitty && has_mods => {
            let code = match key.code {
                KeyCode::Enter => 13,
                KeyCode::Tab => 9,
                _ => 127,
            };
            csi_u(code, key.shift)
        }
        KeyCode::Escape if kitty => csi_u(27, false),
        KeyCode::Enter => Some(if key.alt { b"\x1b\r".to_vec() } else { b"\r".to_vec() }),
        KeyCode::Tab => Some(b"\t".to_vec()),
        KeyCode::Backtab => esc("[Z"),
        KeyCode::Backspace => {
            let byte = if key.ctrl { 0x08 } else { 0x7f };
            Some(if key.alt { vec![0x1b, byte] } else { vec![byte] })
        }
        KeyCode::Escape => Some(vec![0x1b]),
        KeyCode::Up => cursor('A'),
        KeyCode::Down => cursor('B'),
        KeyCode::Right => cursor('C'),
        KeyCode::Left => cursor('D'),
        KeyCode::Home => cursor('H'),
        KeyCode::End => cursor('F'),
        KeyCode::Delete => tilde(3),
        KeyCode::PageUp => tilde(5),
        KeyCode::PageDown => tilde(6),
        KeyCode::F(n @ 1..=4) => {
            let final_char = (b'P' + n - 1) as char;
            if has_mods {
                esc(&format!("[1;{}{}", modifier_param(key, key.shift), final_char))
            } else {
                esc(&format!("O{}", final_char))
            }
        }
        KeyCode::F(n @ 5..=12) => tilde([15, 17, 18, 19, 20, 21, 23, 24][n as usize - 5]),
        KeyCode::F(_)
        | KeyCode::WheelUp
        | KeyCode::WheelDown
        | KeyCode::Mouse1
        | KeyCode::DoubleMouse1
        | KeyCode::DragMouse1 => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(keys: &str, mode: TermMode) -> Vec<u8> {
        encode(Key::parse(keys).unwrap(), mode).unwrap()
    }

    #[test]
    fn xterm_encodings() {
        let none = TermMode::empty();
        assert_eq!(enc("C-c", none), [3]);
        assert_eq!(enc("M-b", none), b"\x1bb");
        assert_eq!(enc("C-_", none), [31]);
        assert_eq!(enc("<up>", none), b"\x1b[A");
        assert_eq!(enc("<up>", TermMode::APP_CURSOR), b"\x1bOA");
        assert_eq!(enc("C-<left>", none), b"\x1b[1;5D");
        assert_eq!(enc("<delete>", none), b"\x1b[3~");
        assert_eq!(enc("DEL", none), [0x7f]);
        assert_eq!(enc("RET", none), b"\r");
    }

    #[test]
    fn kitty_disambiguates_modified_keys() {
        let kitty = TermMode::DISAMBIGUATE_ESC_CODES;
        assert_eq!(enc("S-RET", kitty), b"\x1b[13;2u");
        assert_eq!(enc("C-c", kitty), b"\x1b[99;5u");
        assert_eq!(enc("ESC", kitty), b"\x1b[27u");
        assert_eq!(enc("a", kitty), b"a");
        assert_eq!(enc("RET", kitty), b"\r");
    }
}
