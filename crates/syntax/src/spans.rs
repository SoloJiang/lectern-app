//! Highlight tokens and the per-line store of a highlighted range
//! (docs/design/file_reader.md「`syntax` 的公开项」「语言与区间」).

use std::fmt;
use std::ops::Range;

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

/// Builds a [`WindowSpans`] from non-overlapping, ascending segments of the
/// text `[start, start + text.len())`, splitting them at line ends. The
/// `\n` of a line is never part of a token.
pub(crate) struct SpansBuilder<'t> {
    text: &'t [u8],
    start: u64,
    /// The current line: its start and the offset of its `\n` (or the end
    /// of the text), relative to `text`.
    line_start: usize,
    line_end: usize,
    line_starts: Vec<u32>,
    offsets: Vec<u32>,
    tokens: Vec<Token>,
}

impl<'t> SpansBuilder<'t> {
    pub(crate) fn new(text: &'t [u8], start: u64) -> SpansBuilder<'t> {
        SpansBuilder {
            text,
            start,
            line_start: 0,
            line_end: line_end(text, 0),
            line_starts: Vec::new(),
            offsets: vec![0],
            tokens: Vec::new(),
        }
    }

    /// Adds the segment `[from, to)` (relative to the text). Segments come
    /// in ascending order and do not overlap.
    pub(crate) fn push(&mut self, from: usize, to: usize, kind: TokenKind) {
        let to = to.min(self.text.len());
        let mut from = from;
        while from < to {
            while self.line_end < from {
                self.next_line();
            }
            let end = to.min(self.line_end);
            if end > from {
                self.token(from, end, kind);
            }
            if to <= self.line_end {
                break;
            }
            from = self.line_end + 1;
        }
    }

    fn next_line(&mut self) {
        self.line_start = self.line_end + 1;
        self.line_end = line_end(self.text, self.line_start);
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
            end: self.start + self.text.len() as u64,
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
