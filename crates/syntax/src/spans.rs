//! Highlight tokens and the per-line store of a highlighted range
//! (docs/design/file_reader.md「`syntax` 的公开项」「语言与区间」).

use std::fmt;
use std::ops::Range;

use crate::change::{self, Format, TextEdit};

/// What a highlight query's capture names come down to. `ui` maps each
/// kind to a system colour; the view never defines colours itself.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TokenKind {
    Keyword,
    String,
    Number,
    Comment,
    Function,
    Type,
    Constant,
    Property,
    Variable,
    Operator,
    Punctuation,
    Tag,
    Attribute,
    Escape,
    Heading,
    Link,
    Emphasis,
    Other,
}

impl TokenKind {
    /// Every kind, in discriminant order.
    pub const ALL: [TokenKind; 18] = [
        TokenKind::Keyword,
        TokenKind::String,
        TokenKind::Number,
        TokenKind::Comment,
        TokenKind::Function,
        TokenKind::Type,
        TokenKind::Constant,
        TokenKind::Property,
        TokenKind::Variable,
        TokenKind::Operator,
        TokenKind::Punctuation,
        TokenKind::Tag,
        TokenKind::Attribute,
        TokenKind::Escape,
        TokenKind::Heading,
        TokenKind::Link,
        TokenKind::Emphasis,
        TokenKind::Other,
    ];

    fn from_u8(value: u8) -> TokenKind {
        TokenKind::ALL
            .get(usize::from(value))
            .copied()
            .unwrap_or(TokenKind::Other)
    }
}

/// One coloured range of a line: the start within the line (32 bits), the
/// length (24 bits; longer ranges are split into several tokens) and the
/// kind (8 bits), in 8 bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Token(u64);

impl Token {
    /// The longest length one token holds.
    pub const MAX_LEN: u32 = (1 << 24) - 1;

    /// None when `len` is 0 or over [`Token::MAX_LEN`].
    pub fn new(start: u32, len: u32, kind: TokenKind) -> Option<Token> {
        (len != 0 && len <= Token::MAX_LEN)
            .then(|| Token(u64::from(start) | (u64::from(len) << 32) | ((kind as u64) << 56)))
    }

    pub fn start(self) -> u32 {
        self.0 as u32
    }

    pub fn len(self) -> u32 {
        ((self.0 >> 32) as u32) & Token::MAX_LEN
    }

    /// Always false: a token covers at least one byte.
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    pub fn end(self) -> u32 {
        self.start() + self.len()
    }

    pub fn kind(self) -> TokenKind {
        TokenKind::from_u8((self.0 >> 56) as u8)
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}@{}+{}", self.kind(), self.start(), self.len())
    }
}

/// Appends the tokens of `[start, start + len)` on one line: one token, or
/// several when `len` is over [`Token::MAX_LEN`].
pub(crate) fn push_tokens(out: &mut Vec<Token>, start: u32, len: u32, kind: TokenKind) {
    let (mut start, mut len) = (start, len);
    while len > 0 {
        let take = len.min(Token::MAX_LEN);
        out.extend(Token::new(start, take, kind));
        start += take;
        len -= take;
    }
}

/// The tokens of the text `[start, end)`, stored by line. A line is named
/// by the byte offset of its start, not by its number (line numbers are only
/// estimates until the background scan). Only lines with tokens are stored.
pub struct WindowSpans {
    start: u64,
    end: u64,
    /// The starts of the lines with tokens, relative to `start`, ascending.
    line_starts: Box<[u32]>,
    /// `tokens[offsets[i]..offsets[i + 1]]` are the tokens of line `i`;
    /// one more entry than `line_starts`.
    offsets: Box<[u32]>,
    tokens: Box<[Token]>,
    exact: bool,
}

impl WindowSpans {
    pub fn range(&self) -> Range<u64> {
        self.start..self.end
    }

    /// The tokens of the line starting at `line_start`, starts relative to
    /// it; empty when the line has none or is not in this range.
    pub fn line(&self, line_start: u64) -> &[Token] {
        let Some(relative) = line_start
            .checked_sub(self.start)
            .filter(|_| line_start < self.end)
            .and_then(|r| u32::try_from(r).ok())
        else {
            return &[];
        };
        match self.line_starts.binary_search(&relative) {
            Ok(i) => &self.tokens[self.offsets[i] as usize..self.offsets[i + 1] as usize],
            Err(_) => &[],
        }
    }

    /// The lines with tokens, in order: each line's start (absolute) and
    /// its tokens.
    pub fn lines(&self) -> impl Iterator<Item = (u64, &[Token])> + '_ {
        self.line_starts.iter().enumerate().map(|(i, &start)| {
            (
                self.start + u64::from(start),
                &self.tokens[self.offsets[i] as usize..self.offsets[i + 1] as usize],
            )
        })
    }

    /// True when the range's start was confirmed by the syntax tree (whole
    /// files, windows of a chain cut by the tree); false for the first
    /// screen's heuristic window, which is replaced later.
    pub fn exact(&self) -> bool {
        self.exact
    }

    /// The memory this holds, for the LRU's accounting.
    pub fn bytes(&self) -> usize {
        size_of::<WindowSpans>()
            + size_of_val::<[u32]>(&self.line_starts)
            + size_of_val::<[u32]>(&self.offsets)
            + size_of_val::<[Token]>(&self.tokens)
    }
}

impl fmt::Debug for WindowSpans {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WindowSpans")
            .field("range", &self.range())
            .field("lines", &self.line_starts.len())
            .field("tokens", &self.tokens.len())
            .field("exact", &self.exact)
            .finish()
    }
}

/// Edit mode: the spans of a whole buffer (from 0) carried across edits.
impl WindowSpans {
    /// No tokens over a text of `len` bytes.
    pub(crate) fn empty(len: u64) -> WindowSpans {
        ByLine::new(len, false).finish()
    }

    /// These spans of the text `old` moved to the text `new`, which `edit`
    /// made of it: the lines before the edit stay, the lines after it move
    /// by its shift, and of the lines it touches only the tokens wholly
    /// before or after it are kept (they are parsed again later). Work in
    /// proportion to the lines with tokens; no parse.
    pub(crate) fn shifted(&self, edit: TextEdit, old: &[u8], new: &[u8]) -> WindowSpans {
        let start = edit.start.min(old.len());
        let old_end = edit.old_end.clamp(start, old.len());
        let new_end = edit.new_end.clamp(start, new.len());
        let first = change::line_start(old, start) as u64;
        let last = change::line_start(old, old_end) as u64;
        let landing = change::line_start(new, new_end) as u64;
        let (start, old_end, new_end) = (start as u64, old_end as u64, new_end as u64);
        let mut out = ByLine::new(new.len() as u64, self.exact);
        for (line, tokens) in self.lines() {
            if line < first {
                out.line(line, tokens.iter().copied());
            } else if line > last {
                out.line(line - old_end + new_end, tokens.iter().copied());
            } else {
                if line == first {
                    let head = tokens
                        .iter()
                        .copied()
                        .filter(|t| line + u64::from(t.end()) <= start);
                    out.line(first, head);
                }
                if line == last {
                    // After the edit: the same bytes at the new end, on the
                    // line that holds it in the new text.
                    let tail = tokens.iter().filter_map(|t| {
                        let at = line + u64::from(t.start());
                        let moved = (at >= old_end).then(|| at - old_end + new_end - landing)?;
                        Token::new(u32::try_from(moved).ok()?, t.len(), t.kind())
                    });
                    out.line(landing, tail);
                }
            }
        }
        out.finish()
    }

    /// The spans of a text of `len` bytes: `inner`'s lines in `region` (both
    /// ends line starts, or the text's end), `outer`'s elsewhere.
    pub(crate) fn splice(
        outer: Option<&WindowSpans>,
        inner: &WindowSpans,
        region: Range<u64>,
        len: u64,
    ) -> WindowSpans {
        let mut out = ByLine::new(len, true);
        let outer: Vec<(u64, &[Token])> = outer.map(|o| o.lines().collect()).unwrap_or_default();
        for &(line, tokens) in outer.iter().filter(|(line, _)| *line < region.start) {
            out.line(line, tokens.iter().copied());
        }
        for (line, tokens) in inner.lines().filter(|(line, _)| region.contains(line)) {
            out.line(line, tokens.iter().copied());
        }
        for &(line, tokens) in outer.iter().filter(|(line, _)| *line >= region.end) {
            out.line(line, tokens.iter().copied());
        }
        out.finish()
    }

    /// The reading view's spans of a file's raw `bytes` (from 0) as spans of
    /// the buffer's text, which leaves out the BOM and the `\r` of each
    /// `\r\n`: every line moves back by what it lost before it, tokens are
    /// cut where the line's text ends.
    pub(crate) fn decoded(&self, bytes: &[u8], format: Format, len: u64) -> WindowSpans {
        let bom = if format.bom { 3 } else { 0 };
        let mut out = ByLine::new(len, self.exact);
        // The line breaks before `at`, counted from the last line on.
        let (mut at, mut breaks) = (0usize, 0u64);
        for (line, tokens) in self.lines() {
            let line = line as usize;
            if line > bytes.len() {
                break;
            }
            breaks += memchr::memchr_iter(b'\n', &bytes[at..line]).count() as u64;
            at = line;
            let lost = if format.crlf { breaks } else { 0 };
            // The line's text in the raw bytes, without its `\r\n`.
            let mut end = change::line_end(bytes, line);
            if bytes[..end].ends_with(b"\n") {
                end -= 1;
            }
            if format.crlf && bytes[..end].ends_with(b"\r") {
                end -= 1;
            }
            let from = if line == 0 { bom } else { 0 };
            let width = (end - line) as u64;
            let start = (line as u64 + from).saturating_sub(bom + lost);
            let kept = tokens.iter().filter_map(|t| {
                let (a, b) = (
                    u64::from(t.start()).max(from),
                    u64::from(t.end()).min(width),
                );
                let len = u32::try_from(b.checked_sub(a).filter(|n| *n > 0)?).ok()?;
                Token::new(u32::try_from(a - from).ok()?, len, t.kind())
            });
            out.line(start, kept);
        }
        out.finish()
    }
}

/// Collects lines with tokens in ascending order into a [`WindowSpans`]
/// from 0; a line given twice in a row gets the tokens of both, and a line
/// with none is left out.
struct ByLine {
    len: u64,
    exact: bool,
    line_starts: Vec<u32>,
    offsets: Vec<u32>,
    tokens: Vec<Token>,
}

impl ByLine {
    fn new(len: u64, exact: bool) -> ByLine {
        ByLine {
            len,
            exact,
            line_starts: Vec::new(),
            offsets: vec![0],
            tokens: Vec::new(),
        }
    }

    fn line(&mut self, start: u64, tokens: impl Iterator<Item = Token>) {
        let Ok(start) = u32::try_from(start) else {
            return;
        };
        let before = self.tokens.len();
        self.tokens.extend(tokens);
        if self.tokens.len() == before {
            return;
        }
        if self.line_starts.last() == Some(&start) {
            *self.offsets.last_mut().expect("one more offset than lines") =
                self.tokens.len() as u32;
        } else {
            self.line_starts.push(start);
            self.offsets.push(self.tokens.len() as u32);
        }
    }

    fn finish(self) -> WindowSpans {
        WindowSpans {
            start: 0,
            end: self.len,
            line_starts: self.line_starts.into_boxed_slice(),
            offsets: self.offsets.into_boxed_slice(),
            tokens: self.tokens.into_boxed_slice(),
            exact: self.exact,
        }
    }
}

/// The text a [`SpansBuilder`] splits at line ends, which need not be
/// contiguous in memory (a chunked source, a window with padding around it).
pub(crate) trait Lines {
    fn len(&self) -> usize;
    /// The offset of the first `\n` at or after `from`, or [`Lines::len`].
    fn line_end(&self, from: usize) -> usize;
}

impl Lines for &[u8] {
    fn len(&self) -> usize {
        <[u8]>::len(self)
    }

    fn line_end(&self, from: usize) -> usize {
        line_end(self, from)
    }
}

/// Builds a [`WindowSpans`] from non-overlapping, ascending segments of the
/// text `[start, start + text.len())`, splitting them at line ends. The
/// `\n` of a line is never part of a token. The text is passed to each
/// call, so a builder can wait between the slices of a sliced parse.
pub(crate) struct SpansBuilder {
    start: u64,
    len: usize,
    /// The current line: its start and the offset of its `\n` (or the end
    /// of the text), relative to the text.
    line_start: usize,
    line_end: usize,
    line_starts: Vec<u32>,
    offsets: Vec<u32>,
    tokens: Vec<Token>,
}

impl SpansBuilder {
    pub(crate) fn new(text: &dyn Lines, start: u64) -> SpansBuilder {
        SpansBuilder {
            start,
            len: text.len(),
            line_start: 0,
            line_end: text.line_end(0),
            line_starts: Vec::new(),
            offsets: vec![0],
            tokens: Vec::new(),
        }
    }

    /// Adds the segment `[from, to)` (relative to the text). Segments come
    /// in ascending order and do not overlap.
    pub(crate) fn push(&mut self, text: &dyn Lines, from: usize, to: usize, kind: TokenKind) {
        self.add(text, from, to, kind, false);
    }

    /// Adds the rest of a segment whose start was added before and cut at
    /// `from`: the tokens come out as if the segment had been added whole.
    pub(crate) fn push_rest(&mut self, text: &dyn Lines, from: usize, to: usize, kind: TokenKind) {
        self.add(text, from, to, kind, true);
    }

    fn add(&mut self, text: &dyn Lines, from: usize, to: usize, kind: TokenKind, rest: bool) {
        let to = to.min(self.len);
        let mut from = from;
        let mut rest = rest;
        while from < to {
            while self.line_end < from {
                self.next_line(text);
            }
            let end = to.min(self.line_end);
            if end > from {
                if !(rest && self.join(from, end, kind)) {
                    self.token(from, end, kind);
                }
                rest = false;
            }
            if to <= self.line_end {
                break;
            }
            from = self.line_end + 1;
        }
    }

    fn next_line(&mut self, text: &dyn Lines) {
        self.line_start = self.line_end + 1;
        self.line_end = text.line_end(self.line_start).min(self.len);
    }

    /// Extends the last token over `[from, to)` when it is on this line,
    /// ends at `from` and has this kind (up to [`Token::MAX_LEN`]; the rest
    /// becomes new tokens, as [`push_tokens`] splits). False otherwise.
    fn join(&mut self, from: usize, to: usize, kind: TokenKind) -> bool {
        let (Ok(line), Ok(start)) = (
            u32::try_from(self.line_start),
            u32::try_from(from - self.line_start),
        ) else {
            return false;
        };
        let Some(last) = self.tokens.last_mut() else {
            return false;
        };
        if self.line_starts.last() != Some(&line) || last.end() != start || last.kind() != kind {
            return false;
        }
        let Ok(len) = u32::try_from(to - from) else {
            return false;
        };
        let take = len.min(Token::MAX_LEN - last.len());
        if let Some(joined) = Token::new(last.start(), last.len() + take, kind) {
            *last = joined;
        }
        push_tokens(&mut self.tokens, start + take, len - take, kind);
        true
    }

    fn token(&mut self, from: usize, to: usize, kind: TokenKind) {
        // A line over 4 GiB cannot be indexed by u32; such text never gets
        // here (tier A is at most 1 MiB), and the tokens are dropped.
        let (Ok(line), Ok(start), Ok(len)) = (
            u32::try_from(self.line_start),
            u32::try_from(from - self.line_start),
            u32::try_from(to - from),
        ) else {
            return;
        };
        if self.line_starts.last() != Some(&line) {
            if !self.line_starts.is_empty() {
                self.offsets.push(self.tokens.len() as u32);
            }
            self.line_starts.push(line);
        }
        push_tokens(&mut self.tokens, start, len, kind);
    }

    pub(crate) fn finish(mut self, exact: bool) -> WindowSpans {
        if !self.line_starts.is_empty() {
            self.offsets.push(self.tokens.len() as u32);
        }
        WindowSpans {
            start: self.start,
            end: self.start + self.len as u64,
            line_starts: self.line_starts.into_boxed_slice(),
            offsets: self.offsets.into_boxed_slice(),
            tokens: self.tokens.into_boxed_slice(),
            exact,
        }
    }
}

/// The offset of the first `\n` at or after `from`, or the text's length.
fn line_end(text: &[u8], from: usize) -> usize {
    text.get(from..)
        .and_then(|rest| memchr::memchr(b'\n', rest))
        .map_or(text.len(), |i| from + i)
}
