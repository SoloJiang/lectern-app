//! What a byte sequence shows as and how many character cells it takes
//! (docs/design/file_reader.md「文本与显示行」). The line index and the
//! display rows decode the same way, so a row's folding and its text agree:
//!
//! - valid UTF-8 shows as is, in `unicode-width` cells (a wide character
//!   takes two, a combining mark none);
//! - a tab takes the cells to the next multiple of [`TAB`];
//! - another control character (`\0`–`\x1f`, `\x7f`) shows as its symbol
//!   from the U+2400 block, one cell; a `\r` right before a line's `\n` is
//!   not shown at all;
//! - every byte that does not start a valid sequence shows as U+FFFD, one
//!   cell.

use std::borrow::Cow;

use unicode_width::UnicodeWidthChar;

/// Tab stops every 4 cells (not read from `.editorconfig`, 「已知限制」).
pub(crate) const TAB: u64 = 4;

/// One decoded unit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Unit {
    Char(char),
    /// A control character other than tab.
    Control(u8),
    /// A byte that does not start a valid UTF-8 sequence.
    Invalid,
}

/// The unit at the start of `bytes` (not empty) and its length. A sequence
/// cut by the end of `bytes` is `Invalid` (one byte): callers give at least
/// four bytes when they have them.
pub(crate) fn unit(bytes: &[u8]) -> (Unit, usize) {
    let b = bytes[0];
    if b < 0x80 {
        return match b {
            b'\t' => (Unit::Char('\t'), 1),
            0..=0x1f | 0x7f => (Unit::Control(b), 1),
            _ => (Unit::Char(char::from(b)), 1),
        };
    }
    let len = match b {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return (Unit::Invalid, 1),
    };
    match bytes
        .get(..len)
        .and_then(|s| std::str::from_utf8(s).ok())
        .and_then(|s| s.chars().next())
    {
        Some(c) => (Unit::Char(c), len),
        None => (Unit::Invalid, 1),
    }
}

/// The cells a unit takes when it starts at cell `at` of its line.
pub(crate) fn width(unit: Unit, at: u64) -> u64 {
    match unit {
        Unit::Char('\t') => TAB - at % TAB,
        Unit::Char(c) => c.width().unwrap_or(1) as u64,
        Unit::Control(_) | Unit::Invalid => 1,
    }
}

/// What a unit shows as.
fn shown(unit: Unit) -> char {
    match unit {
        Unit::Char(c) => c,
        Unit::Control(0x7f) => '\u{2421}',
        Unit::Control(b) => char::from_u32(0x2400 + u32::from(b)).unwrap_or('\u{fffd}'),
        Unit::Invalid => '\u{fffd}',
    }
}

/// Whether `bytes` show as themselves: valid UTF-8 with no control
/// character but tab.
pub(crate) fn is_plain(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_ok()
        && !bytes.iter().any(|&b| (b < 0x20 && b != b'\t') || b == 0x7f)
}

/// The text `bytes` show as, and, when it differs from the bytes, the map
/// from each unit's byte offset to its offset in the text (ascending pairs,
/// ending with the lengths of both).
pub(crate) fn display(bytes: &[u8]) -> (Cow<'_, str>, Option<Vec<(u32, u32)>>) {
    if is_plain(bytes) {
        // Checked just above.
        return (
            Cow::Borrowed(std::str::from_utf8(bytes).unwrap_or_default()),
            None,
        );
    }
    let mut text = String::with_capacity(bytes.len() + 8);
    let mut map = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let (u, len) = unit(&bytes[at..]);
        map.push((at as u32, text.len() as u32));
        text.push(shown(u));
        at += len;
    }
    map.push((bytes.len() as u32, text.len() as u32));
    (Cow::Owned(text), Some(map))
}

/// A byte offset into the shown text, through `display`'s map: an offset
/// inside a unit maps to the unit's start.
pub(crate) fn remap(map: &[(u32, u32)], at: u32) -> u32 {
    match map.binary_search_by_key(&at, |&(b, _)| b) {
        Ok(i) => map[i].1,
        Err(0) => 0,
        Err(i) => map[i - 1].1,
    }
}
