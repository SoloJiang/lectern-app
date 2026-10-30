//! Review notes: the pure parts (docs/design/review.md「审阅意见」). The
//! excerpt kept with a note; the engine's commands and the annotation log's
//! records are in `engine::notes` and `log`.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use lectern_services::changes::SideText;
use lectern_services::review::{DiffRow, DiffSide, Excerpt, FileRows, NoteAnchor};

/// Lines kept before and after the anchor line.
const CONTEXT_LINES: usize = 2;
/// How far from the anchor line, in bytes, each direction looks.
const WINDOW: usize = 4096;
/// A kept line is cut to this many bytes, on a character boundary.
pub(crate) const LINE_BYTES: usize = 160;

/// The text of the anchor's side in `rows` when it is the content the note
/// was written on: the side's OID is the anchor's and its text is
/// retained. Nothing is scanned.
pub(crate) fn anchored_text<'a>(rows: &'a FileRows, anchor: &NoteAnchor) -> Option<&'a [u8]> {
    let side = match anchor.side {
        DiffSide::Old => &rows.old,
        DiffSide::New => &rows.new,
    };
    match &side.text {
        SideText::Text(bytes) if side.oid == Some(anchor.oid) => Some(bytes),
        _ => None,
    }
}

/// The nearest line at or before `line` of the rows' side whose byte
/// offset the rows know: where the search for `line` starts.
pub(crate) fn hint(rows: &FileRows, side: DiffSide, line: u32) -> Option<(u32, u64)> {
    let mut best: Option<(u32, u64)> = None;
    for row in rows.rows.iter() {
        let known = match (side, row) {
            (DiffSide::Old, DiffRow::Context { old, old_byte, .. })
            | (DiffSide::Old, DiffRow::Removed { old, old_byte, .. }) => (*old, *old_byte),
            (DiffSide::New, DiffRow::Context { new, new_byte, .. })
            | (DiffSide::New, DiffRow::Added { new, new_byte, .. }) => (*new, *new_byte),
            (DiffSide::Old, DiffRow::Folded { old, old_byte, .. }) => (old.start, *old_byte),
            (DiffSide::New, DiffRow::Folded { new, new_byte, .. }) => (new.start, *new_byte),
            _ => continue,
        };
        if let (l, Some(byte)) = known
            && l <= line
            && best.is_none_or(|(b, _)| l > b)
        {
            best = Some((l, byte));
            if l == line {
                break;
            }
        }
    }
    best
}

/// The lines around `line` (0-based) of `text`: up to two before and two
/// after, each cut to [`LINE_BYTES`], none read further than 4 KiB from the
/// anchor line's start. `hint` is a line and its byte offset known at or
/// before `line` (`hint()` of the rows), otherwise the lines are counted
/// from the start of the text. None when the text has no such line.
/// Bytes that are not UTF-8 become U+FFFD.
pub(crate) fn excerpt(text: &[u8], line: u32, hint: Option<(u32, u64)>) -> Option<Excerpt> {
    let (mut at, mut pos) = match hint {
        Some((l, byte)) if l <= line && byte <= text.len() as u64 => (l, byte as usize),
        _ => (0, 0),
    };
    while at < line {
        pos += memchr::memchr(b'\n', &text[pos..])? + 1;
        at += 1;
    }
    if pos >= text.len() {
        return None;
    }
    let start = pos;

    // Before: each previous line ends at the newline before `cur`; its
    // start must lie within the window.
    let floor = start.saturating_sub(WINDOW);
    let mut before: Vec<Arc<str>> = Vec::new();
    let mut cur = start;
    while before.len() < CONTEXT_LINES && cur > 0 {
        let end = cur - 1;
        let begin = match memchr::memrchr(b'\n', &text[floor..end]) {
            Some(i) => floor + i + 1,
            None if floor == 0 => 0,
            None => break,
        };
        before.push(clip(&text[begin..end], true));
        cur = begin;
    }
    before.reverse();

    // The anchor line and the lines after it.
    let ceiling = start.saturating_add(WINDOW).min(text.len());
    let mut lines = before;
    let first = line - lines.len() as u32;
    let mut pos = Some(start);
    for _ in 0..=CONTEXT_LINES {
        let Some(from) = pos.filter(|p| *p < text.len() && *p <= ceiling) else {
            break;
        };
        let end = memchr::memchr(b'\n', &text[from..ceiling]).map(|i| from + i);
        let content_end = end.unwrap_or_else(|| text.len().min(from + LINE_BYTES + 4));
        lines.push(clip(&text[from..content_end], end.is_some()));
        // The anchor line must end inside the window for the lines after it
        // to be found.
        pos = end.map(|e| e + 1);
    }
    Some(Excerpt {
        first,
        lines: lines.into(),
    })
}

/// A line cut to [`LINE_BYTES`] on a character boundary. `whole`: the slice
/// is the line up to its newline, so a `\r` before it is not text.
fn clip(line: &[u8], whole: bool) -> Arc<str> {
    let line = match line {
        [rest @ .., b'\r'] if whole => rest,
        _ => line,
    };
    // Four bytes more than kept: a character cut by the slice's end is
    // replaced after the cut point, and the cut drops it.
    let line = &line[..line.len().min(LINE_BYTES + 4)];
    let mut text = String::from_utf8_lossy(line).into_owned();
    if text.len() > LINE_BYTES {
        let mut cut = LINE_BYTES;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    Arc::from(text)
}
