//! Three-way merge of text by lines (docs/design/git.md「三方合并」), for
//! the reader's save after the file changed on disk (docs/design/
//! file_reader.md「编辑」). Pure: no IO, callable on any thread.
//!
//! diff3 from two line diffs against the base, both with gix's imara-diff
//! (`gix::diff::blob`, Histogram). The result follows `git merge-file`'s
//! default level: identical changes on both sides are taken once, a
//! conflict is narrowed to the lines where the two sides differ, and two
//! conflicts are joined when at most three lines, or only lines without an
//! ASCII letter or digit, separate them.

use std::ops::Range;

use gix::diff::blob::{Algorithm, Diff, InternedInput, sources::byte_lines};

/// The labels after `<<<<<<<` and `>>>>>>>`. They are file content, not
/// interface strings: the reader passes `editor` and `disk`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Markers<'a> {
    pub ours: &'a str,
    pub theirs: &'a str,
}

/// The merged bytes and where their conflicts are.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Merge3 {
    pub bytes: Vec<u8>,
    /// Byte ranges in `bytes`, in order: each from the start of a
    /// `<<<<<<<` line to the end of its `>>>>>>>` line, line ending
    /// included. Empty when the merge is clean.
    pub conflicts: Vec<Range<usize>>,
}

impl Merge3 {
    pub fn is_clean(&self) -> bool {
        self.conflicts.is_empty()
    }
}

/// Lines end after `\n` (a `\r` before it belongs to the line); a last line
/// without one is a line, and differs from the same text with one, as in
/// the hunks. Where both sides changed the same lines differently, the
/// conflict holds `<<<<<<< {ours}`, the lines of `ours`, `=======`, the
/// lines of `theirs` and `>>>>>>> {theirs}` (no base section). A side whose
/// last line in a conflict has no `\n` gets a line ending added there, so
/// every marker starts a line. A conflict's markers end in `\r\n` when the
/// lines they can be told from do (the line before the conflict on each
/// side, the base's first line), otherwise in `\n`. Pure; any thread.
pub fn merge3(base: &[u8], ours: &[u8], theirs: &[u8], markers: &Markers<'_>) -> Merge3 {
    if ours == theirs || base == theirs {
        return clean(ours);
    }
    if base == ours {
        return clean(theirs);
    }
    let files = Files {
        base: Lines::new(base),
        ours: Lines::new(ours),
        theirs: Lines::new(theirs),
    };
    let items = join_conflicts(&files, &join_same(chunks(&files)));
    write(&files, &items, markers)
}

fn clean(bytes: &[u8]) -> Merge3 {
    Merge3 {
        bytes: bytes.to_vec(),
        conflicts: Vec::new(),
    }
}

/// One file's bytes and the start of each line, then its length.
struct Lines<'a> {
    bytes: &'a [u8],
    starts: Vec<usize>,
}

impl<'a> Lines<'a> {
    fn new(bytes: &'a [u8]) -> Lines<'a> {
        let mut starts = vec![0];
        starts.extend(
            bytes
                .iter()
                .enumerate()
                .filter(|(_, b)| **b == b'\n')
                .map(|(i, _)| i + 1),
        );
        if starts.last() != Some(&bytes.len()) {
            starts.push(bytes.len());
        }
        Lines { bytes, starts }
    }

    fn len(&self) -> u32 {
        u32::try_from(self.starts.len() - 1).unwrap_or(u32::MAX)
    }

    fn byte_range(&self, lines: &Range<u32>) -> Range<usize> {
        self.starts[lines.start as usize]..self.starts[lines.end as usize]
    }

    fn slice(&self, lines: &Range<u32>) -> &'a [u8] {
        &self.bytes[self.byte_range(lines)]
    }

    /// Whether line `n` ends in `\r\n` (true), `\n` (false), or neither can
    /// be told (None): a file without lines, or a single line without a
    /// line ending. A last line without one takes the line before it.
    fn crlf(&self, n: u32) -> Option<bool> {
        if n >= self.len() {
            return None;
        }
        let line = self.slice(&(n..n + 1));
        if line.ends_with(b"\n") {
            Some(line.ends_with(b"\r\n"))
        } else if n > 0 {
            self.crlf(n - 1)
        } else {
            None
        }
    }
}

struct Files<'a> {
    base: Lines<'a>,
    ours: Lines<'a>,
    theirs: Lines<'a>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Ours,
    Theirs,
}

/// The merge as a sequence of pieces. Line ranges are in the file the piece
/// is copied from; `Same` is in `ours` (the same lines are in every file it
/// came from).
#[derive(Clone, PartialEq, Eq, Debug)]
enum Item {
    /// Lines both sides keep: unchanged from the base, or common to the
    /// two sides inside a changed region.
    Same(Range<u32>),
    /// A change taken from one side: only that side changed, or both made
    /// the same change (taken from `ours`).
    Taken(Side, Range<u32>),
    Conflict {
        ours: Range<u32>,
        theirs: Range<u32>,
    },
}

/// The Histogram line diff of `before` and `after`, hunks slid down as far
/// as they go (no indent heuristic): `(before, after)` line ranges.
fn line_diff(before: &[u8], after: &[u8]) -> Vec<(Range<u32>, Range<u32>)> {
    let input = InternedInput::new(byte_lines(before), byte_lines(after));
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    diff.postprocess_no_heuristic(&input);
    diff.hunks().map(|h| (h.before, h.after)).collect()
}

/// diff3: walks the base → ours and base → theirs hunks together. Hunks of
/// the two sides that overlap or touch in the base (one ends where the
/// other starts, two insertions at one point) form one changed region,
/// extended for as long as another hunk starts inside or at its end.
fn chunks(files: &Files<'_>) -> Vec<Item> {
    let to_ours = line_diff(files.base.bytes, files.ours.bytes);
    let to_theirs = line_diff(files.base.bytes, files.theirs.bytes);
    let mut items = Vec::new();
    let (mut i, mut j) = (0, 0);
    // The next base line not yet placed, and the line of each side that
    // corresponds to it (outside hunks, the sides have the base's lines).
    let (mut base_at, mut ours_at, mut theirs_at) = (0u32, 0u32, 0u32);
    while i < to_ours.len() || j < to_theirs.len() {
        let start = match (to_ours.get(i), to_theirs.get(j)) {
            (Some(o), Some(t)) => o.0.start.min(t.0.start),
            (Some(o), None) => o.0.start,
            (None, Some(t)) => t.0.start,
            (None, None) => unreachable!(),
        };
        let same = start - base_at;
        if same > 0 {
            items.push(Item::Same(ours_at..ours_at + same));
        }
        ours_at += same;
        theirs_at += same;
        let mut end = start;
        // The last hunk of each side in the region.
        let (mut last_ours, mut last_theirs) = (None, None);
        loop {
            if let Some(h) = to_ours.get(i).filter(|h| h.0.start <= end) {
                end = end.max(h.0.end);
                last_ours = Some(h);
                i += 1;
            } else if let Some(h) = to_theirs.get(j).filter(|h| h.0.start <= end) {
                end = end.max(h.0.end);
                last_theirs = Some(h);
                j += 1;
            } else {
                break;
            }
        }
        // A side's region ends where its last hunk ends, plus the base
        // lines after that hunk up to the region's end.
        let side_end = |at: u32, last: Option<&(Range<u32>, Range<u32>)>| match last {
            Some((b, s)) => s.end + (end - b.end),
            None => at + (end - start),
        };
        let ours = ours_at..side_end(ours_at, last_ours);
        let theirs = theirs_at..side_end(theirs_at, last_theirs);
        match (last_ours.is_some(), last_theirs.is_some()) {
            (true, false) => items.push(Item::Taken(Side::Ours, ours.clone())),
            (false, true) => items.push(Item::Taken(Side::Theirs, theirs.clone())),
            _ if files.ours.slice(&ours) == files.theirs.slice(&theirs) => {
                items.push(Item::Taken(Side::Ours, ours.clone()));
            }
            _ => refine(files, ours.clone(), theirs.clone(), &mut items),
        }
        base_at = end;
        ours_at = ours.end;
        theirs_at = theirs.end;
    }
    let rest = files.base.len() - base_at;
    if rest > 0 {
        items.push(Item::Same(ours_at..ours_at + rest));
    }
    items
}

/// Splits a conflicting region by diffing its two sides: lines they share
/// become `Same`, each hunk of that diff a conflict.
fn refine(files: &Files<'_>, ours: Range<u32>, theirs: Range<u32>, items: &mut Vec<Item>) {
    let hunks = line_diff(files.ours.slice(&ours), files.theirs.slice(&theirs));
    let mut at = ours.start;
    for (o, t) in hunks {
        let (o, t) = (
            o.start + ours.start..o.end + ours.start,
            t.start + theirs.start..t.end + theirs.start,
        );
        if o.start > at {
            items.push(Item::Same(at..o.start));
        }
        at = o.end;
        items.push(Item::Conflict { ours: o, theirs: t });
    }
    if ours.end > at {
        items.push(Item::Same(at..ours.end));
    }
}

/// Joins adjacent `Same` pieces (an unchanged stretch followed by the lines
/// a refined region starts with), so a gap between two conflicts is one
/// piece.
fn join_same(items: Vec<Item>) -> Vec<Item> {
    let mut out: Vec<Item> = Vec::with_capacity(items.len());
    for item in items {
        match (out.last_mut(), &item) {
            (Some(Item::Same(prev)), Item::Same(next)) if prev.end == next.start => {
                prev.end = next.end;
            }
            _ => out.push(item),
        }
    }
    out
}

/// At most this many lines between two conflicts join them.
const JOIN_LINES: u32 = 3;

/// Joins two conflicts with only `Same` lines between them when those are
/// at most three lines or none of them has an ASCII letter or digit
/// (`git merge-file`'s default level); the joined conflict holds those
/// lines on both sides. A taken change between two conflicts keeps them
/// apart.
fn join_conflicts(files: &Files<'_>, items: &[Item]) -> Vec<Item> {
    let mut out: Vec<Item> = Vec::with_capacity(items.len());
    for item in items {
        if let Item::Conflict { ours, theirs } = item {
            let joined = match out.as_slice() {
                [.., Item::Conflict { .. }, Item::Same(gap)]
                    if gap.end - gap.start <= JOIN_LINES
                        || !files.ours.slice(gap).iter().any(u8::is_ascii_alphanumeric) =>
                {
                    out.pop();
                    true
                }
                [.., Item::Conflict { .. }] => true,
                _ => false,
            };
            if joined {
                let Some(Item::Conflict {
                    ours: prev_ours,
                    theirs: prev_theirs,
                }) = out.last_mut()
                else {
                    unreachable!()
                };
                prev_ours.end = ours.end;
                prev_theirs.end = theirs.end;
                continue;
            }
        }
        out.push(item.clone());
    }
    out
}

fn write(files: &Files<'_>, items: &[Item], markers: &Markers<'_>) -> Merge3 {
    let capacity = files.ours.bytes.len().max(files.theirs.bytes.len());
    let mut bytes = Vec::with_capacity(capacity);
    let mut conflicts = Vec::new();
    for item in items {
        match item {
            Item::Same(lines) | Item::Taken(Side::Ours, lines) => {
                bytes.extend_from_slice(files.ours.slice(lines));
            }
            Item::Taken(Side::Theirs, lines) => bytes.extend_from_slice(files.theirs.slice(lines)),
            Item::Conflict { ours, theirs } => {
                let eol: &[u8] = if crlf_markers(files, ours, theirs) {
                    b"\r\n"
                } else {
                    b"\n"
                };
                let start = bytes.len();
                marker(&mut bytes, b"<<<<<<<", markers.ours, eol);
                side(&mut bytes, files.ours.slice(ours), eol);
                marker(&mut bytes, b"=======", "", eol);
                side(&mut bytes, files.theirs.slice(theirs), eol);
                marker(&mut bytes, b">>>>>>>", markers.theirs, eol);
                conflicts.push(start..bytes.len());
            }
        }
    }
    Merge3 { bytes, conflicts }
}

fn marker(bytes: &mut Vec<u8>, sign: &[u8], label: &str, eol: &[u8]) {
    bytes.extend_from_slice(sign);
    if !label.is_empty() {
        bytes.push(b' ');
        bytes.extend_from_slice(label.as_bytes());
    }
    bytes.extend_from_slice(eol);
}

/// One side of a conflict; a last line without `\n` gets `eol`.
fn side(bytes: &mut Vec<u8>, lines: &[u8], eol: &[u8]) {
    bytes.extend_from_slice(lines);
    if !lines.is_empty() && !lines.ends_with(b"\n") {
        bytes.extend_from_slice(eol);
    }
}

/// Whether a conflict's markers end in `\r\n`: the line before the conflict
/// in `ours` and in `theirs` (their first line when the conflict starts the
/// file) and the base's first line vote, each with its own line ending; a
/// file whose ending can't be told doesn't vote. `\r\n` when every vote is
/// `\r\n` and there is at least one, so a file that is all CRLF stays all
/// CRLF.
fn crlf_markers(files: &Files<'_>, ours: &Range<u32>, theirs: &Range<u32>) -> bool {
    let before = |r: &Range<u32>| r.start.saturating_sub(1);
    let votes = [
        files.ours.crlf(before(ours)),
        files.theirs.crlf(before(theirs)),
        files.base.crlf(0),
    ];
    let mut votes = votes.into_iter().flatten().peekable();
    votes.peek().is_some() && votes.all(|crlf| crlf)
}

#[cfg(test)]
mod tests;
