//! Review notes: the pure parts (docs/design/review.md「审阅意见」). The
//! excerpt kept with a note, and the prompt a batch is sent as ([`prompt`]);
//! the engine's commands and the annotation log's records are in
//! `engine::notes` and `log`.

pub mod prompt;

#[cfg(test)]
mod tests;

use std::collections::VecDeque;
use std::sync::Arc;

use lectern_services::changes::SideText;
use lectern_services::review::{
    DiffRow, DiffSide, Excerpt, ExcerptGap, FileRows, NoteAnchor, limits,
};

/// Lines kept before and after the anchor line.
const CONTEXT_LINES: usize = 2;
/// Lines kept at each end of a range of more than 8 lines.
const EDGE: usize = 4;
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

/// A line of the text: `from..to` without its end, and whether `to` is
/// the line's newline (so that a `\r` before it is not text).
#[derive(Clone, Copy)]
struct Span {
    from: usize,
    to: usize,
    whole: bool,
}

/// The lines of the range `line..=last` (0-based, both ends included) of
/// `text`, with the lines around it ("命令" of the design):
///
/// | lines in the range | kept |
/// |---|---|
/// | 1 to 4 | two before, the range, two after |
/// | 5 to 8 | the range |
/// | more than 8 | its first 4 and its last 4, with a gap between |
///
/// Each line is cut to [`LINE_BYTES`]; the lines before and after are not
/// looked for further than 4 KiB from the range's first and last line.
/// `hint` is a line and its byte offset known at or before `line`
/// (`hint()` of the rows), otherwise the lines are counted from the start
/// of the text. A `last` before `line` is `line`; a range that runs past
/// the end of the text ends at its last line (the gap counts the lines
/// that were found). None when the text has no line `line`. Bytes that are
/// not UTF-8 become U+FFFD.
pub(crate) fn excerpt(
    text: &[u8],
    line: u32,
    last: u32,
    hint: Option<(u32, u64)>,
) -> Option<Excerpt> {
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
    let want = u64::from(last.max(line) - line) + 1;

    // The range: the first `MAX` lines and the last `EDGE` of it, as spans.
    let mut head: Vec<Span> = Vec::new();
    let mut tail: VecDeque<Span> = VecDeque::new();
    let mut count: u64 = 0;
    let mut keep = |span: Span, count: &mut u64| {
        if head.len() < limits::EXCERPT_LINES {
            head.push(span);
        }
        if tail.len() == EDGE {
            tail.pop_front();
        }
        tail.push_back(span);
        *count += 1;
    };
    let mut cur = start;
    let mut ended = false;
    // Every line of the range but the last: nothing limits how far its
    // end is.
    while count + 1 < want {
        match memchr::memchr(b'\n', &text[cur..]) {
            Some(i) => {
                let span = Span {
                    from: cur,
                    to: cur + i,
                    whole: true,
                };
                keep(span, &mut count);
                cur += i + 1;
                if cur >= text.len() {
                    ended = true;
                    break;
                }
            }
            None => {
                // The text ends in this line.
                let span = Span {
                    from: cur,
                    to: text.len().min(cur + LINE_BYTES + 4),
                    whole: false,
                };
                keep(span, &mut count);
                ended = true;
                break;
            }
        }
    }

    // The last line of the range and, for a short range, the lines after
    // it: all of them must end inside the window.
    let context = !ended && count < EDGE as u64;
    let mut after: Vec<Span> = Vec::new();
    if !ended {
        let ceiling = cur.saturating_add(WINDOW).min(text.len());
        let mut pos = Some(cur);
        for k in 0..=if context { CONTEXT_LINES } else { 0 } {
            let Some(from) = pos.filter(|p| *p < text.len() && *p <= ceiling) else {
                break;
            };
            let end = memchr::memchr(b'\n', &text[from..ceiling]).map(|i| from + i);
            let span = Span {
                from,
                to: end.unwrap_or_else(|| text.len().min(from + LINE_BYTES + 4)),
                whole: end.is_some(),
            };
            if k == 0 {
                keep(span, &mut count);
            } else {
                after.push(span);
            }
            // The last line of the range must end inside the window for
            // the lines after it to be found.
            pos = end.map(|e| e + 1);
        }
    }

    // The lines before the range, only for a short one: each previous line
    // ends at the newline before `cur`; its start must lie within the
    // window.
    let mut before: Vec<Span> = Vec::new();
    if count <= EDGE as u64 {
        let floor = start.saturating_sub(WINDOW);
        let mut cur = start;
        while before.len() < CONTEXT_LINES && cur > 0 {
            let end = cur - 1;
            let begin = match memchr::memrchr(b'\n', &text[floor..end]) {
                Some(i) => floor + i + 1,
                None if floor == 0 => 0,
                None => break,
            };
            before.push(Span {
                from: begin,
                to: end,
                whole: true,
            });
            cur = begin;
        }
        before.reverse();
    }

    let first = line - before.len() as u32;
    let (range, gap): (Vec<Span>, Option<ExcerptGap>) = if count <= limits::EXCERPT_LINES as u64 {
        (head, None)
    } else {
        let mut both = head[..EDGE].to_vec();
        both.extend(tail);
        let gap = ExcerptGap {
            at: EDGE as u32,
            lines: u32::try_from(count - limits::EXCERPT_LINES as u64).unwrap_or(u32::MAX),
        };
        (both, Some(gap))
    };
    let lines: Arc<[Arc<str>]> = before
        .iter()
        .chain(&range)
        .chain(&after)
        .map(|s| clip(&text[s.from..s.to], s.whole))
        .collect();
    Some(Excerpt { first, lines, gap })
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
