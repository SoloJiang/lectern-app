//! Display rows shared by the unified and side-by-side views, and word-level
//! changes within a pair of lines (docs/design/git.md「展示行」). Pure
//! computation, on any thread.

use std::ops::Range;

use gix::diff::blob::{Algorithm, Diff, InternedInput};

use crate::{FileDiff, FileTexts, SideText};

/// One display row. Line numbers are 0-based in each side's whole file.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Row {
    Context {
        old: u32,
        new: u32,
    },
    /// `pair`: the added line of the same hunk shown beside it in the
    /// side-by-side view.
    Removed {
        old: u32,
        pair: Option<u32>,
    },
    Added {
        new: u32,
        pair: Option<u32>,
    },
    /// Unchanged lines folded away; expanding takes the same ranges' text.
    Folded {
        old: Range<u32>,
        new: Range<u32>,
    },
}

/// The rows of a file: each hunk's removed lines then its added lines, the
/// i-th removed line paired with the i-th added one; `context` unchanged
/// lines around each hunk, the rest folded. Two hunks at most `2 * context`
/// lines apart share their context, as in git. A diff without hunks is one
/// fold. Sides that aren't `SideText::Text` take their length from the
/// hunks and the other side.
pub fn rows(diff: &FileDiff, texts: &FileTexts, context: u32) -> Vec<Row> {
    let known = |side: &SideText| match side {
        SideText::Text(text) => Some(text.line_count()),
        SideText::Absent => Some(0),
        _ => None,
    };
    let (last_old, last_new) = diff.hunks.last().map_or((0, 0), |h| (h.old.end, h.new.end));
    // The stretch after the last hunk is the same length on both sides.
    let (old_len, new_len) = match (known(&texts.old), known(&texts.new)) {
        (Some(o), Some(n)) => (o, n),
        (Some(o), None) => (o, last_new + o.saturating_sub(last_old)),
        (None, Some(n)) => (last_old + n.saturating_sub(last_new), n),
        (None, None) => (last_old, last_new),
    };
    let mut out = Vec::new();
    if diff.hunks.is_empty() {
        if old_len > 0 || new_len > 0 {
            out.push(Row::Folded {
                old: 0..old_len,
                new: 0..new_len,
            });
        }
        return out;
    }
    let (mut o, mut n) = (0, 0);
    for (i, hunk) in diff.hunks.iter().enumerate() {
        let shown = if i == 0 { Shown::End } else { Shown::Both };
        unchanged(&mut out, o..hunk.old.start, n, shown, context);
        let (removed, added) = (hunk.old.len() as u32, hunk.new.len() as u32);
        for k in 0..removed {
            out.push(Row::Removed {
                old: hunk.old.start + k,
                pair: (k < added).then(|| hunk.new.start + k),
            });
        }
        for k in 0..added {
            out.push(Row::Added {
                new: hunk.new.start + k,
                pair: (k < removed).then(|| hunk.old.start + k),
            });
        }
        (o, n) = (hunk.old.end, hunk.new.end);
    }
    let tail = old_len.saturating_sub(o).min(new_len.saturating_sub(n));
    unchanged(&mut out, o..o + tail, n, Shown::Start, context);
    out
}

/// Which ends of an unchanged stretch keep their context.
#[derive(Clone, Copy)]
enum Shown {
    /// Before the first hunk: only the end, next to the hunk.
    End,
    /// Between hunks.
    Both,
    /// After the last hunk: only the start.
    Start,
}

/// `old` (and the same number of lines from `new_start`) as context rows
/// and a fold.
fn unchanged(out: &mut Vec<Row>, old: Range<u32>, new_start: u32, shown: Shown, context: u32) {
    let len = old.len() as u32;
    let (head, tail) = match shown {
        Shown::End => (0, len.min(context)),
        Shown::Start => (len.min(context), 0),
        Shown::Both if len <= 2 * context => (len, 0),
        Shown::Both => (context, context),
    };
    let context_row = |k: u32| Row::Context {
        old: old.start + k,
        new: new_start + k,
    };
    out.extend((0..head).map(context_row));
    if head + tail < len {
        out.push(Row::Folded {
            old: old.start + head..old.end - tail,
            new: new_start + head..new_start + len - tail,
        });
    }
    out.extend((len - tail..len).map(context_row));
}

/// Lines longer than this get no word diff: the view highlights the whole line.
pub const INLINE_MAX_LINE_BYTES: usize = 1000;

/// The changed parts of a pair of lines, as byte ranges into each line.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct InlineChanges {
    pub old: Vec<Range<u32>>,
    pub new: Vec<Range<u32>>,
}

/// Word-level changes between a removed line and the added line paired with
/// it (without their newlines). None when either is longer than
/// [`INLINE_MAX_LINE_BYTES`]. Words are runs of ASCII letters, digits and
/// `_`; runs of whitespace; and every other character on its own (so CJK
/// text compares by character). Histogram diff over the words (ADR 0024).
pub fn inline_changes(old: &[u8], new: &[u8]) -> Option<InlineChanges> {
    if old.len() > INLINE_MAX_LINE_BYTES || new.len() > INLINE_MAX_LINE_BYTES {
        return None;
    }
    let (old_bounds, new_bounds) = (word_bounds(old), word_bounds(new));
    fn words<'l>(line: &'l [u8], bounds: &[usize]) -> Vec<&'l [u8]> {
        bounds.windows(2).map(|w| &line[w[0]..w[1]]).collect()
    }
    let (old_words, new_words) = (words(old, &old_bounds), words(new, &new_bounds));
    let mut input: InternedInput<&[u8]> = InternedInput::default();
    input.update_before(old_words.iter().copied());
    input.update_after(new_words.iter().copied());
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    diff.postprocess_no_heuristic(&input);
    let mut out = InlineChanges::default();
    let span = |bounds: &[usize], r: Range<u32>| {
        bounds[r.start as usize] as u32..bounds[r.end as usize] as u32
    };
    for hunk in diff.hunks() {
        if !hunk.before.is_empty() {
            out.old.push(span(&old_bounds, hunk.before));
        }
        if !hunk.after.is_empty() {
            out.new.push(span(&new_bounds, hunk.after));
        }
    }
    Some(out)
}

/// The start of every word, then `line.len()`.
fn word_bounds(line: &[u8]) -> Vec<usize> {
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut bounds = vec![0];
    let mut i = 0;
    while i < line.len() {
        let c = line[i];
        let len = if word(c) {
            line[i..].iter().take_while(|&&b| word(b)).count()
        } else if c.is_ascii_whitespace() {
            line[i..]
                .iter()
                .take_while(|b| b.is_ascii_whitespace())
                .count()
        } else {
            // One character: an ASCII byte or a UTF-8 sequence (a stray
            // byte on its own).
            let len = match c {
                0xC0..=0xDF => 2,
                0xE0..=0xEF => 3,
                0xF0..=0xF7 => 4,
                _ => 1,
            };
            let continuation = line[i + 1..]
                .iter()
                .take(len - 1)
                .take_while(|&&b| b & 0xC0 == 0x80)
                .count();
            1 + continuation
        };
        i += len;
        bounds.push(i);
    }
    bounds
}

#[cfg(test)]
mod tests;
