//! `Input` keys and modifiers in libghostty-vt's terms.

use lectern_services::terminal::{Key, KeyAction, Mods};
use libghostty_vt::key;

/// The physical key for `key`. Characters map to the US-layout key that
/// produces them unshifted; other characters are `Unidentified` and encode
/// through their text.
pub(crate) fn physical(key: Key) -> key::Key {
    use key::Key as K;
    match key {
        Key::Char(c) => match c.to_ascii_lowercase() {
            c @ 'a'..='z' => LETTERS[(c as u8 - b'a') as usize],
            c @ '0'..='9' => DIGITS[(c as u8 - b'0') as usize],
            '`' => K::Backquote,
            '\\' => K::Backslash,
            '[' => K::BracketLeft,
            ']' => K::BracketRight,
            ',' => K::Comma,
            '=' => K::Equal,
            '-' => K::Minus,
            '.' => K::Period,
            '\'' => K::Quote,
            ';' => K::Semicolon,
            '/' => K::Slash,
            _ => K::Unidentified,
        },
        Key::Enter => K::Enter,
        Key::Tab => K::Tab,
        Key::Backspace => K::Backspace,
        Key::Escape => K::Escape,
        Key::Space => K::Space,
        Key::Up => K::ArrowUp,
        Key::Down => K::ArrowDown,
        Key::Left => K::ArrowLeft,
        Key::Right => K::ArrowRight,
        Key::Home => K::Home,
        Key::End => K::End,
        Key::PageUp => K::PageUp,
        Key::PageDown => K::PageDown,
        Key::Insert => K::Insert,
        Key::Delete => K::Delete,
        Key::F(n) => match n {
            1..=20 => FUNCTION[usize::from(n - 1)],
            _ => K::Unidentified,
        },
    }
}

/// The character the key produces without Shift, for the kitty protocol.
pub(crate) fn unshifted(key: Key) -> Option<char> {
    match key {
        Key::Char(c) => Some(c),
        Key::Space => Some(' '),
        _ => None,
    }
}

pub(crate) fn mods(mods: Mods) -> key::Mods {
    let mut out = key::Mods::empty();
    for (on, bit) in [
        (mods.shift, key::Mods::SHIFT),
        (mods.control, key::Mods::CTRL),
        (mods.option, key::Mods::ALT),
        (mods.command, key::Mods::SUPER),
    ] {
        if on {
            out |= bit;
        }
    }
    out
}

pub(crate) fn action(action: KeyAction) -> key::Action {
    match action {
        KeyAction::Press => key::Action::Press,
        KeyAction::Repeat => key::Action::Repeat,
        KeyAction::Release => key::Action::Release,
    }
}

const LETTERS: [key::Key; 26] = {
    use key::Key as K;
    [
        K::A,
        K::B,
        K::C,
        K::D,
        K::E,
        K::F,
        K::G,
        K::H,
        K::I,
        K::J,
        K::K,
        K::L,
        K::M,
        K::N,
        K::O,
        K::P,
        K::Q,
        K::R,
        K::S,
        K::T,
        K::U,
        K::V,
        K::W,
        K::X,
        K::Y,
        K::Z,
    ]
};

const DIGITS: [key::Key; 10] = {
    use key::Key as K;
    [
        K::Digit0,
        K::Digit1,
        K::Digit2,
        K::Digit3,
        K::Digit4,
        K::Digit5,
        K::Digit6,
        K::Digit7,
        K::Digit8,
        K::Digit9,
    ]
};

const FUNCTION: [key::Key; 20] = {
    use key::Key as K;
    [
        K::F1,
        K::F2,
        K::F3,
        K::F4,
        K::F5,
        K::F6,
        K::F7,
        K::F8,
        K::F9,
        K::F10,
        K::F11,
        K::F12,
        K::F13,
        K::F14,
        K::F15,
        K::F16,
        K::F17,
        K::F18,
        K::F19,
        K::F20,
    ]
};
