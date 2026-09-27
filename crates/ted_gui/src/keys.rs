//! Translates winit keyboard events into ted `KeyEvent`s.
//!
//! With Ctrl or Alt held we use the physical key's US-layout character: platforms report
//! control characters or composed symbols for those combinations, while keymaps want
//! `C-x` / `M-<`. Shift stays a modifier; the core folds it into the character.

use winit::keyboard::{Key, KeyCode as Physical, NamedKey, PhysicalKey};

use ted_core::{KeyCode, KeyEvent, Modifiers};

const PHYSICAL_CHARS: &[(Physical, char)] = &[
    (Physical::KeyA, 'a'),
    (Physical::KeyB, 'b'),
    (Physical::KeyC, 'c'),
    (Physical::KeyD, 'd'),
    (Physical::KeyE, 'e'),
    (Physical::KeyF, 'f'),
    (Physical::KeyG, 'g'),
    (Physical::KeyH, 'h'),
    (Physical::KeyI, 'i'),
    (Physical::KeyJ, 'j'),
    (Physical::KeyK, 'k'),
    (Physical::KeyL, 'l'),
    (Physical::KeyM, 'm'),
    (Physical::KeyN, 'n'),
    (Physical::KeyO, 'o'),
    (Physical::KeyP, 'p'),
    (Physical::KeyQ, 'q'),
    (Physical::KeyR, 'r'),
    (Physical::KeyS, 's'),
    (Physical::KeyT, 't'),
    (Physical::KeyU, 'u'),
    (Physical::KeyV, 'v'),
    (Physical::KeyW, 'w'),
    (Physical::KeyX, 'x'),
    (Physical::KeyY, 'y'),
    (Physical::KeyZ, 'z'),
    (Physical::Digit0, '0'),
    (Physical::Digit1, '1'),
    (Physical::Digit2, '2'),
    (Physical::Digit3, '3'),
    (Physical::Digit4, '4'),
    (Physical::Digit5, '5'),
    (Physical::Digit6, '6'),
    (Physical::Digit7, '7'),
    (Physical::Digit8, '8'),
    (Physical::Digit9, '9'),
    (Physical::Numpad0, '0'),
    (Physical::Numpad1, '1'),
    (Physical::Numpad2, '2'),
    (Physical::Numpad3, '3'),
    (Physical::Numpad4, '4'),
    (Physical::Numpad5, '5'),
    (Physical::Numpad6, '6'),
    (Physical::Numpad7, '7'),
    (Physical::Numpad8, '8'),
    (Physical::Numpad9, '9'),
    (Physical::Minus, '-'),
    (Physical::Equal, '='),
    (Physical::BracketLeft, '['),
    (Physical::BracketRight, ']'),
    (Physical::Backslash, '\\'),
    (Physical::Semicolon, ';'),
    (Physical::Quote, '\''),
    (Physical::Backquote, '`'),
    (Physical::Comma, ','),
    (Physical::Period, '.'),
    (Physical::Slash, '/'),
    (Physical::NumpadDivide, '/'),
    (Physical::Space, ' '),
];

/// Keys identified physically regardless of modifiers or layout quirks.
const PHYSICAL_NAMED: &[(Physical, KeyCode)] = &[
    (Physical::Backspace, KeyCode::Backspace),
    (Physical::Enter, KeyCode::Enter),
    (Physical::NumpadEnter, KeyCode::Enter),
    (Physical::Delete, KeyCode::Delete),
];

const NAMED: &[(NamedKey, KeyCode)] = &[
    (NamedKey::Enter, KeyCode::Enter),
    (NamedKey::Backspace, KeyCode::Backspace),
    (NamedKey::Tab, KeyCode::Tab),
    (NamedKey::Escape, KeyCode::Escape),
    (NamedKey::Space, KeyCode::Char(' ')),
    (NamedKey::ArrowUp, KeyCode::Up),
    (NamedKey::ArrowDown, KeyCode::Down),
    (NamedKey::ArrowLeft, KeyCode::Left),
    (NamedKey::ArrowRight, KeyCode::Right),
    (NamedKey::Home, KeyCode::Home),
    (NamedKey::End, KeyCode::End),
    (NamedKey::PageUp, KeyCode::PageUp),
    (NamedKey::PageDown, KeyCode::PageDown),
    (NamedKey::Delete, KeyCode::Delete),
    (NamedKey::F1, KeyCode::F(1)),
    (NamedKey::F2, KeyCode::F(2)),
    (NamedKey::F3, KeyCode::F(3)),
    (NamedKey::F4, KeyCode::F(4)),
    (NamedKey::F5, KeyCode::F(5)),
    (NamedKey::F6, KeyCode::F(6)),
    (NamedKey::F7, KeyCode::F(7)),
    (NamedKey::F8, KeyCode::F(8)),
    (NamedKey::F9, KeyCode::F(9)),
    (NamedKey::F10, KeyCode::F(10)),
    (NamedKey::F11, KeyCode::F(11)),
    (NamedKey::F12, KeyCode::F(12)),
];

fn lookup<K: PartialEq, V: Copy>(table: &[(K, V)], key: &K) -> Option<V> {
    table.iter().find(|(k, _)| k == key).map(|(_, v)| *v)
}

pub fn translate(physical: PhysicalKey, logical: &Key, text: Option<&str>, mods: Modifiers) -> Option<KeyEvent> {
    let code = physical_code(physical, mods).or_else(|| logical_code(logical, text))?;
    Some(KeyEvent::new(code, mods))
}

fn physical_code(physical: PhysicalKey, mods: Modifiers) -> Option<KeyCode> {
    let PhysicalKey::Code(code) = physical else {
        return None;
    };
    if let Some(named) = lookup(PHYSICAL_NAMED, &code) {
        return Some(named);
    }
    if mods.ctrl || mods.alt {
        return lookup(PHYSICAL_CHARS, &code).map(KeyCode::Char);
    }
    None
}

fn logical_code(logical: &Key, text: Option<&str>) -> Option<KeyCode> {
    match logical {
        Key::Named(named) => lookup(NAMED, named),
        Key::Character(s) => s.chars().next().filter(|c| !c.is_control()).map(KeyCode::Char),
        _ => text?.chars().next().filter(|c| !c.is_control()).map(KeyCode::Char),
    }
}
