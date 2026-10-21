//! The first screen's reads and window (docs/design/file_reader.md「首屏窗口」,
//! ADR 0034「首屏路径的准备工作」). Pure computation on the bytes given: no
//! IO, no line index, nothing scanned from the file's start. Any thread.
//!
//! Every step is bounded: the target's line start is looked for at most
//! [`LONG_LINE`] back; the visible part is at most 64 KiB; the safe start
//! only looks at the bytes before the target that the first read brought
//! (at most 64 KiB); encoding and binary checks only look at the file's
//! first 8 KiB and the window.

use std::ops::Range;

use crate::lang::Rules;
use crate::limits::{LONG_LINE, TIER_A_MAX};

/// The file's head: encoding, BOM and binary checks.
const HEAD: u64 = 8 << 10;
/// The first read around the target: the safe start is looked for in the
/// 64 KiB before it; the visible part (at most 64 KiB) and the 4 KiB after
/// it fit in the 68 KiB after it.
const AROUND_BEFORE: u64 = 64 << 10;
const AROUND_AFTER: u64 = 68 << 10;
/// With a line checkpoint the target line is in the 64 KiB after it, so
/// the read reaches 132 KiB past the checkpoint (platform design「按行定位」).
const CHECKPOINT_AFTER: u64 = 132 << 10;
/// The visible part's limit.
const VISIBLE_MAX: usize = 64 << 10;
/// What a long line counts as in the visible part.
const LONG_ROW: usize = 8 << 10;
/// The window starts at a safe position about this far before the visible
/// part...
const SAFE_BACK: u64 = 8 << 10;
/// ...looked for at most this much further back (column-0 lines).
const SAFE_SCAN: usize = 4 << 10;
/// The window ends this far after the visible part.
const WINDOW_AFTER: u64 = 4 << 10;
/// The longest JSON key a safe start takes.
const JSON_KEY_MAX: usize = 256;

/// The first screen's one `read_ranges` for a target byte: `[0, min(8 KiB,
/// size))` and `[t − 64 KiB, t + 68 KiB)`, cut at 0 and `size` (platform
/// design「首屏的一次调用」); one range `[0, size)` when `size ≤ 1 MiB`.
/// Overlapping ranges are left to `read_ranges` to merge.
pub fn first_read(size: u64, target: u64) -> Vec<Range<u64>> {
    two_ranges(size, target, AROUND_AFTER)
}

/// With only a line number and a line checkpoint `c` (the last one before
/// the line): `[0, 8 KiB)` and `[c − 64 KiB, c + 132 KiB)`, cut the same
/// way (platform design「按行定位」).
pub fn checkpoint_read(size: u64, checkpoint: u64) -> Vec<Range<u64>> {
    two_ranges(size, checkpoint, CHECKPOINT_AFTER)
}

fn two_ranges(size: u64, at: u64, after: u64) -> Vec<Range<u64>> {
    if size <= TIER_A_MAX {
        #[expect(
            clippy::single_range_in_vec_init,
            reason = "one range of the first read, not a list of its offsets"
        )]
        return vec![0..size];
    }
    let at = at.min(size);
    vec![
        0..HEAD.min(size),
        at.saturating_sub(AROUND_BEFORE)..(at + after).min(size),
    ]
}

/// Where the first screen opens.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Byte(u64),
    /// A line number counted from a line checkpoint: `checkpoint_line` is
    /// the number of line breaks before `checkpoint_byte`.
    LineFrom {
        checkpoint_byte: u64,
        checkpoint_line: u32,
        line: u32,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Encoding {
    Utf8,
    /// UTF-8 with a byte order mark: the BOM counts in offsets and is not
    /// shown; tree-sitter skips it at the start of a parse.
    Utf8Bom,
    /// A NUL in the file's head or the window, or not UTF-8: plain text.
    NotText,
}

/// What a JSON window parsed alone needs around it (ADR 0034): a window
/// starts at an object member, so the containers it closes are opened in
/// front of it (`{`, `[`, with an empty key `"":` inside an object) and the
/// ones still open at its end are closed after it. Empty for other
/// languages and for a window at the file's start.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct JsonPad {
    pub before: Box<[u8]>,
    pub after: Box<[u8]>,
}

impl JsonPad {
    pub fn is_empty(&self) -> bool {
        self.before.is_empty() && self.after.is_empty()
    }
}

/// The first screen: what is visible and the window parsed for it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Plan {
    pub encoding: Encoding,
    /// The start of the target's line; the target itself when it is in a
    /// line longer than [`LONG_LINE`] whose start was not found (the line's
    /// display rows are folded from here until the full read).
    pub target_line_start: u64,
    /// At most `rows / 3` lines before the target's line and `rows` lines
    /// from it, at most 64 KiB; a long line counts as 8 KiB.
    pub visible: Range<u64>,
    /// From a safe start to 4 KiB past the visible part (a JSON window ends
    /// after the last whole value instead, when that is past the visible
    /// part). None when no safe start is in the bytes read or the text is
    /// [`Encoding::NotText`]: the first screen is plain text.
    pub window: Option<Range<u64>>,
    pub json_pad: JsonPad,
}

/// Plans the first screen from the bytes the first read brought: `head` is
/// the file's first `min(8 KiB, size)` bytes, `around` the bytes from
/// `around_start` (one read range, holding the target). `rows` is how many
/// rows the viewport holds. Looks at no byte outside the two slices.
pub fn plan(
    rules: Rules,
    head: &[u8],
    around: &[u8],
    around_start: u64,
    size: u64,
    target: Target,
    rows: u32,
) -> Plan {
    let file_start = around_start == 0;
    let around_end = around_start + around.len() as u64;
    let at_file_end = around_end >= size;
    let rows = rows.max(1) as usize;
    let t = match target {
        Target::Byte(byte) => (byte.clamp(around_start, around_end) - around_start) as usize,
        Target::LineFrom {
            checkpoint_byte,
            checkpoint_line,
            line,
        } => line_from(
            around,
            (checkpoint_byte.clamp(around_start, around_end) - around_start) as usize,
            checkpoint_line,
            line,
        ),
    };
    let (line_start, long) = line_start_of(around, t, file_start);

    // The visible part: `rows` lines from the target's line, then up to
    // `rows / 3` before it.
    let mut end = line_start;
    if long {
        end = (line_start + LONG_ROW).min(around.len());
    } else {
        for _ in 0..rows {
            if end >= around.len() {
                break;
            }
            let limit = (end + LONG_LINE as usize + 1).min(around.len());
            match memchr::memchr(b'\n', &around[end..limit]) {
                Some(i) if end + i + 1 - line_start > VISIBLE_MAX => break,
                Some(i) => end += i + 1,
                None => {
                    end = if limit - end > LONG_LINE as usize {
                        (end + LONG_ROW).min(around.len())
                    } else {
                        // The file's last line, or the read's end.
                        limit
                    };
                    break;
                }
            }
        }
    }
    let mut start = line_start;
    if !long {
        for _ in 0..rows / 3 {
            if start == 0 {
                break;
            }
            let newline = start - 1;
            let lo = newline.saturating_sub(LONG_LINE as usize);
            let prev = match memchr::memrchr(b'\n', &around[lo..newline]) {
                Some(i) => lo + i + 1,
                None if lo == 0 && file_start => 0,
                // A long line, or one that starts before the bytes read.
                None => break,
            };
            if end - prev > VISIBLE_MAX {
                break;
            }
            start = prev;
        }
    }
    let visible = around_start + start as u64..around_start + end as u64;

    // The window.
    let before = visible.start.saturating_sub(SAFE_BACK).max(around_start);
    let mut window = safe_start(rules, around, around_start, before)
        .filter(|&s| s <= visible.start)
        .map(|s| s..(visible.end + WINDOW_AFTER).min(around_end));
    let checked = window.as_ref().unwrap_or(&visible);
    let checked =
        &around[(checked.start - around_start) as usize..(checked.end - around_start) as usize];
    let encoding = if !is_text(head) || !is_text(checked) {
        Encoding::NotText
    } else if head.starts_with(&[0xEF, 0xBB, 0xBF]) {
        Encoding::Utf8Bom
    } else {
        Encoding::Utf8
    };
    let mut pad = JsonPad::default();
    if encoding == Encoding::NotText {
        window = None;
    } else if let (Rules::Json, Some(w)) = (rules, window.as_mut()) {
        let bytes = &around[(w.start - around_start) as usize..(w.end - around_start) as usize];
        // A window to the file's end is parsed whole.
        if !(at_file_end && w.end == around_end) {
            let len = json_trim(bytes, (visible.end - w.start) as usize);
            w.end = w.start + len as u64;
        }
        let len = (w.end - w.start) as usize;
        pad = json_pad(&bytes[..len], w.start > 0);
    }
    Plan {
        encoding,
        target_line_start: around_start + line_start as u64,
        visible,
        window,
        json_pad: pad,
    }
}

/// The start of line `line`, counted from the checkpoint at `c` (relative
/// to `around`). The read's end when the line is past it.
fn line_from(around: &[u8], c: usize, checkpoint_line: u32, line: u32) -> usize {
    if line > checkpoint_line {
        let k = (line - checkpoint_line) as usize;
        memchr::memchr_iter(b'\n', &around[c..])
            .nth(k - 1)
            .map_or(around.len(), |i| c + i + 1)
    } else {
        let k = (checkpoint_line - line) as usize + 1;
        memchr::memrchr_iter(b'\n', &around[..c])
            .nth(k - 1)
            .map_or(0, |i| i + 1)
    }
}

/// The start of the line holding `t`, looked for at most [`LONG_LINE`]
/// back; `(t, true)` when the line is longer.
fn line_start_of(around: &[u8], t: usize, file_start: bool) -> (usize, bool) {
    let lo = t.saturating_sub(LONG_LINE as usize);
    match memchr::memrchr(b'\n', &around[lo..t]) {
        Some(i) => (lo + i + 1, false),
        None if lo == 0 && file_start => (0, false),
        None => (t, true),
    }
}

/// No NUL and valid UTF-8, except a sequence cut at either end of the
/// slice.
fn is_text(bytes: &[u8]) -> bool {
    if memchr::memchr(0, bytes).is_some() {
        return false;
    }
    let lead = bytes
        .iter()
        .take(3)
        .take_while(|b| (0x80..0xC0).contains(*b))
        .count();
    match std::str::from_utf8(&bytes[lead..]) {
        Ok(_) => true,
        Err(error) => error.error_len().is_none(),
    }
}

/// A safe start for a window parsed alone (ADR 0034), at or about
/// `before`, in `bytes` (the file's bytes from `base`):
///
/// - `Code`: the closest line in the 4 KiB before `before` that starts at
///   column 0 and is not a closing bracket or a comment continuation
///   (`*`); else the least indented such line there; else (no line starts
///   there: one long line) just after the last `;` or `}`; else a column-0
///   line further back.
/// - `Json`: the closest object member `,"key":` (the key at most 256 bytes,
///   no backslash), at its key.
/// - `Markup`: just after the closest blank line.
///
/// The file's start when the bytes reach it and nothing else is found;
/// None otherwise: the first screen is plain text. Only looks at `bytes`.
pub fn safe_start(rules: Rules, bytes: &[u8], base: u64, before: u64) -> Option<u64> {
    let file_start = base == 0;
    let b = before.saturating_sub(base).min(bytes.len() as u64) as usize;
    if file_start && b == 0 {
        return Some(0);
    }
    let found = match rules {
        Rules::Code => code_start(bytes, b, file_start),
        Rules::Json => json_start(bytes, b),
        Rules::Markup => markup_start(bytes, b, file_start),
    };
    found.map(|r| base + r as u64).or(file_start.then_some(0))
}

/// A byte a statement at column 0 can start with.
fn starts_statement(b: u8) -> bool {
    !b.is_ascii_whitespace() && !matches!(b, b'}' | b')' | b']' | b'*')
}

fn code_start(bytes: &[u8], b: usize, file_start: bool) -> Option<usize> {
    let lo = b.saturating_sub(SAFE_SCAN);
    let line_starts = memchr::memrchr_iter(b'\n', &bytes[lo..b])
        .map(|i| lo + i + 1)
        .chain((lo == 0 && file_start).then_some(0));
    let mut any_line = false;
    // (indent, line start) of the least indented candidate, closest first.
    let mut fallback: Option<(usize, usize)> = None;
    for p in line_starts {
        any_line = true;
        let rest = &bytes[p..];
        let indent = rest
            .iter()
            .take_while(|c| matches!(c, b' ' | b'\t'))
            .count();
        let Some(&c) = rest.get(indent) else {
            continue;
        };
        if indent == 0 && starts_statement(c) {
            return Some(p);
        }
        if starts_statement(c) && fallback.is_none_or(|(least, _)| indent < least) {
            fallback = Some((indent, p));
        }
    }
    if let Some((_, p)) = fallback {
        return Some(p);
    }
    if !any_line {
        // One long line: after the last `;` or `}`.
        if let Some(i) = memchr::memrchr2(b';', b'}', &bytes[..b]) {
            return Some(i + 1);
        }
    }
    memchr::memrchr_iter(b'\n', &bytes[..lo])
        .map(|i| i + 1)
        .find(|&p| bytes.get(p).is_some_and(|&c| starts_statement(c)))
}

fn json_start(bytes: &[u8], b: usize) -> Option<usize> {
    memchr::memrchr_iter(b',', &bytes[..b]).find_map(|i| member_at(bytes, i + 1))
}

/// The key's opening quote when `"key":` follows `from` after whitespace.
fn member_at(bytes: &[u8], from: usize) -> Option<usize> {
    let skip_ws = |at: usize| {
        at + bytes[at..]
            .iter()
            .take_while(|c| c.is_ascii_whitespace())
            .count()
    };
    let quote = skip_ws(from);
    if bytes.get(quote) != Some(&b'"') {
        return None;
    }
    let key_end = (quote + 2 + JSON_KEY_MAX).min(bytes.len());
    let close = quote + 1 + memchr::memchr2(b'"', b'\\', &bytes[quote + 1..key_end])?;
    if bytes[close] != b'"' {
        return None;
    }
    (bytes.get(skip_ws(close + 1)) == Some(&b':')).then_some(quote)
}

fn markup_start(bytes: &[u8], b: usize, file_start: bool) -> Option<usize> {
    memchr::memrchr_iter(b'\n', &bytes[..b]).find_map(|q| {
        // Is the line ending at `q` blank?
        let mut k = q;
        while k > 0 && matches!(bytes[k - 1], b' ' | b'\t' | b'\r') {
            k -= 1;
        }
        let blank = if k == 0 {
            file_start
        } else {
            bytes[k - 1] == b'\n'
        };
        blank.then_some(q + 1)
    })
}

/// How much of a JSON window (`bytes`, starting outside a string) to
/// parse: up to the last whole value (after a closing bracket, or before a
/// comma) when that is at or past `at_least`, else all of it.
fn json_trim(bytes: &[u8], at_least: usize) -> usize {
    let mut cut = None;
    let mut scan = Scan::default();
    for (i, &c) in bytes.iter().enumerate() {
        if scan.step(c) {
            match c {
                b'}' | b']' => cut = Some(i + 1),
                b',' => cut = Some(i),
                _ => {}
            }
        }
    }
    cut.filter(|&c| c >= at_least).unwrap_or(bytes.len())
}

/// The last comma outside a string in `bytes` (starting outside a string):
/// where a JSON window of the chain is cut.
pub(crate) fn json_last_comma(bytes: &[u8]) -> Option<usize> {
    let mut last = None;
    let mut scan = Scan::default();
    for (i, &c) in bytes.iter().enumerate() {
        if scan.step(c) && c == b',' {
            last = Some(i);
        }
    }
    last
}

/// The pad a JSON window (`bytes`, starting outside a string) needs to
/// parse alone. `inside`: the window starts inside some container (at a
/// member or element, not at the file's start); the kind of each container
/// around it comes from the window itself: a `:` at its level makes it an
/// object, its closing bracket says what it was, and elements without keys
/// make an array.
pub(crate) fn json_pad(bytes: &[u8], inside: bool) -> JsonPad {
    // The containers: opened in the window (explicit, by their closing
    // bracket), and around it (implicit, innermost first; None while the
    // kind is not known).
    let mut explicit: Vec<u8> = Vec::new();
    let mut implicit: Vec<Option<u8>> = if inside { vec![None] } else { Vec::new() };
    // Whether the last implicit level has anything in it yet.
    let mut content = false;
    let mut scan = Scan::default();
    for &c in bytes {
        if !scan.step(c) {
            continue;
        }
        match c {
            b'{' => explicit.push(b'}'),
            b'[' => explicit.push(b']'),
            b'}' | b']' => {
                if explicit.pop().is_none() && inside {
                    if let Some(level @ None) = implicit.last_mut() {
                        *level = Some(c);
                    }
                    implicit.push(None);
                    content = false;
                }
            }
            b':' if explicit.is_empty() => {
                if let Some(level @ None) = implicit.last_mut() {
                    *level = Some(b'}');
                }
            }
            b' ' | b'\t' | b'\r' | b'\n' => {}
            _ if explicit.is_empty() => content = true,
            _ => {}
        }
    }
    // The outermost implicit level is open unless it is an empty one after
    // the last closing bracket, which is not needed.
    let mut outer_open = !implicit.is_empty();
    if let Some(level @ None) = implicit.last_mut() {
        if content {
            // Elements without keys: an array.
            *level = Some(b']');
        } else {
            implicit.pop();
            outer_open = false;
        }
    }
    let kinds: Vec<u8> = implicit.iter().map(|k| k.unwrap_or(b'}')).collect();

    let mut before = Vec::new();
    for (i, &kind) in kinds.iter().enumerate().rev() {
        if kinds.get(i + 1) == Some(&b'}') {
            before.extend_from_slice(b"\"\":");
        }
        before.push(if kind == b'}' { b'{' } else { b'[' });
    }
    let mut after = Vec::new();
    if scan.in_string {
        after.push(b'"');
    }
    after.extend(explicit.iter().rev());
    // The levels inside the outermost were closed in the window.
    if let (true, Some(&last)) = (outer_open, kinds.last()) {
        after.push(last);
    }
    JsonPad {
        before: before.into(),
        after: after.into(),
    }
}

/// A JSON scanner's string state.
#[derive(Default)]
struct Scan {
    in_string: bool,
    escaped: bool,
}

impl Scan {
    /// True when `c` is outside a string (the quote that opens one counts as
    /// outside, the one that closes it as inside).
    fn step(&mut self, c: u8) -> bool {
        if self.in_string {
            if self.escaped {
                self.escaped = false;
            } else if c == b'\\' {
                self.escaped = true;
            } else if c == b'"' {
                self.in_string = false;
            }
            false
        } else {
            if c == b'"' {
                self.in_string = true;
            }
            true
        }
    }
}
