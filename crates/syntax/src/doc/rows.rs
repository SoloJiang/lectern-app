//! Display rows (docs/design/file_reader.md「文本与显示行」): what a
//! snapshot shows from a row position on. Every call does work in
//! proportion to the rows asked for: a line's end is looked for at most
//! 4 KiB on, a long line's display row is found from its cell marks.
//!
//! Lines up to 4 KiB are one display row each. A longer line is folded by
//! character cells: a character starting at cell `c` is on display row
//! `c / cols`. Before the line index exists (the first screen) a long line
//! is folded from the row position given, which then stands for the line's
//! start; moving back across such a line steps `cols` bytes at a time.

use std::borrow::Cow;
use std::ops::Range;

use crate::cells;
use crate::highlight::Source;
use crate::limits::LONG_LINE;
use crate::spans::Token;
use crate::text::{LineIndex, LongLine, unit_at};

use super::content::slice;
use super::{DocSnapshot, LineNo, Row, RowPos};

/// One line as the rows see it.
struct Line<'a> {
    start: u64,
    /// The end of what is shown (a final `\r` is not).
    end: u64,
    /// The next line's start; None for the last line.
    next: Option<u64>,
    kind: Kind<'a>,
}

enum Kind<'a> {
    /// One display row.
    Short,
    /// Folded with the index's cell marks.
    Indexed(&'a LongLine),
    /// Folded from `start` without marks: the end is not known.
    Unindexed,
}

/// One display row.
struct RowSpan {
    bytes: Range<u64>,
    /// The position after this row; None after the last row.
    next: Option<RowPos>,
}

impl DocSnapshot {
    fn src(&self) -> Option<&dyn Source> {
        self.content.source()
    }

    fn index(&self) -> Option<&LineIndex> {
        self.content.index()
    }

    /// At most `n` display rows from `from` on, long lines folded at `cols`
    /// cells. A row whose bytes are not in memory is [`Row::Pending`], and
    /// the rows end there.
    pub fn rows(&self, from: RowPos, n: usize, cols: u32) -> Vec<Row<'_>> {
        let cols = cols.max(1);
        let mut out = Vec::with_capacity(n.min(1024));
        let Some(src) = self.src() else {
            return out;
        };
        let mut pos = from;
        let mut line = self.line_number(pos.line_start);
        while out.len() < n {
            if pos.line_start > self.size || (pos.line_start == self.size && self.size > 0) {
                // Past the last line (a final line break ends it).
                if !(pos.line_start == self.size && self.ends_with_newline()) {
                    break;
                }
            }
            let Some(row) = self.row_span(src, pos, cols) else {
                out.push(Row::Pending { pos });
                break;
            };
            match self.text_row(src, pos, line, row.bytes.clone()) {
                Some(text) => out.push(text),
                None => {
                    out.push(Row::Pending { pos });
                    break;
                }
            }
            let Some(next) = row.next else {
                break;
            };
            if next.line_start != pos.line_start {
                line = next_line_no(line);
            }
            pos = next;
        }
        out
    }

    /// `from` moved by `delta` display rows (wheel, keyboard); it stops at
    /// either end and where the bytes are not in memory.
    pub fn advance(&self, from: RowPos, delta: i64, cols: u32) -> RowPos {
        let cols = cols.max(1);
        let Some(src) = self.src() else {
            return from;
        };
        let mut pos = from;
        if delta >= 0 {
            for _ in 0..delta {
                match self.row_span(src, pos, cols).and_then(|r| r.next) {
                    Some(next) if self.has_row(next) => pos = next,
                    _ => break,
                }
            }
        } else {
            for _ in 0..delta.unsigned_abs() {
                match self.prev(src, pos, cols) {
                    Some(prev) => pos = prev,
                    None => break,
                }
            }
        }
        pos
    }

    /// The scroll bar dragged: the display row holding the byte at
    /// `fraction` of the file.
    pub fn at_fraction(&self, fraction: f64, cols: u32) -> RowPos {
        let fraction = if fraction.is_finite() {
            fraction.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.pos_of_byte((self.size as f64 * fraction) as u64, cols)
    }

    /// The display row holding `byte` (a find match, an anchor).
    pub fn pos_of_byte(&self, byte: u64, cols: u32) -> RowPos {
        let cols = cols.max(1);
        let byte = byte.min(self.size);
        let Some(src) = self.src() else {
            return RowPos::default();
        };
        if let Some(index) = self.index() {
            if let Some(long) = index.long_holding(byte) {
                let sub = long
                    .row_of(src, byte, cols)
                    .unwrap_or((byte - long.start) / u64::from(cols));
                return RowPos {
                    line_start: long.start,
                    sub_row: u32::try_from(sub).unwrap_or(u32::MAX),
                };
            }
            let start = index
                .line_of_in(src, byte)
                .and_then(|line| index.line_start_in(src, line))
                .unwrap_or_else(|| index.block_before(byte).1);
            return RowPos {
                line_start: start,
                sub_row: 0,
            };
        }
        RowPos {
            line_start: line_start_back(src, byte).unwrap_or(byte),
            sub_row: 0,
        }
    }

    /// The display rows in all, for the scroll bar: the lines plus the rows
    /// long lines add (estimated before the line index).
    pub fn total_rows(&self, cols: u32) -> u64 {
        match self.index() {
            Some(index) => index.line_count() + index.extra_rows(cols.max(1)),
            None => self.lines.get(),
        }
    }

    fn ends_with_newline(&self) -> bool {
        self.size > 0
            && self
                .src()
                .and_then(|s| s.chunk(self.size - 1))
                .is_some_and(|c| c.first() == Some(&b'\n'))
    }

    /// Whether `pos` names a row (not past the last line).
    fn has_row(&self, pos: RowPos) -> bool {
        pos.line_start < self.size || (pos.line_start == self.size && self.ends_with_newline())
    }

    fn line<'s>(&'s self, src: &dyn Source, start: u64) -> Option<Line<'s>> {
        if let Some(long) = self.index().and_then(|i| i.long_at(start)) {
            return Some(Line {
                start,
                end: long.end,
                next: long.next,
                kind: Kind::Indexed(long),
            });
        }
        let limit = (start + LONG_LINE + 1).min(self.size);
        let mut at = start;
        let mut last = None;
        while at < limit {
            let chunk = src.chunk(at).filter(|c| !c.is_empty())?;
            let take = chunk.len().min((limit - at) as usize);
            if let Some(i) = memchr::memchr(b'\n', &chunk[..take]) {
                let newline = at + i as u64;
                let cr = if i > 0 {
                    chunk[i - 1] == b'\r'
                } else {
                    last == Some(b'\r')
                };
                let end = if cr && newline > start {
                    newline - 1
                } else {
                    newline
                };
                return Some(Line {
                    start,
                    end,
                    next: Some(newline + 1),
                    kind: Kind::Short,
                });
            }
            last = chunk[..take].last().copied();
            at += take as u64;
        }
        if at >= self.size && self.size - start <= LONG_LINE {
            let end = if last == Some(b'\r') && self.size > start {
                self.size - 1
            } else {
                self.size
            };
            return Some(Line {
                start,
                end,
                next: None,
                kind: Kind::Short,
            });
        }
        Some(Line {
            start,
            end: self.size,
            next: None,
            kind: Kind::Unindexed,
        })
    }

    fn row_span(&self, src: &dyn Source, pos: RowPos, cols: u32) -> Option<RowSpan> {
        if pos.line_start >= self.size {
            // The empty line after a final line break.
            return Some(RowSpan {
                bytes: self.size..self.size,
                next: None,
            });
        }
        let line = self.line(src, pos.line_start)?;
        let to_next = |next: Option<u64>| {
            next.map(|n| RowPos {
                line_start: n,
                sub_row: 0,
            })
        };
        match line.kind {
            Kind::Short => Some(RowSpan {
                bytes: line.start..line.end,
                next: to_next(line.next),
            }),
            Kind::Indexed(long) => {
                let rows = long.rows(cols);
                let k = u64::from(pos.sub_row).min(rows - 1);
                let bytes = long.row_range(src, k, cols)?;
                let next = if k + 1 < rows {
                    Some(RowPos {
                        line_start: pos.line_start,
                        sub_row: k as u32 + 1,
                    })
                } else {
                    to_next(line.next)
                };
                Some(RowSpan { bytes, next })
            }
            Kind::Unindexed => {
                let (bytes, end) = fold(src, line.start, u64::from(pos.sub_row), cols, self.size)?;
                let next = match end {
                    Some(next) => to_next(next),
                    None => Some(RowPos {
                        line_start: pos.line_start,
                        sub_row: pos.sub_row.saturating_add(1),
                    }),
                };
                Some(RowSpan { bytes, next })
            }
        }
    }

    fn prev(&self, src: &dyn Source, pos: RowPos, cols: u32) -> Option<RowPos> {
        if pos.sub_row > 0 {
            return Some(RowPos {
                line_start: pos.line_start,
                sub_row: pos.sub_row - 1,
            });
        }
        let ls = pos.line_start.min(self.size);
        if ls == 0 {
            return None;
        }
        let real_start = src.chunk(ls - 1)?.first() == Some(&b'\n');
        if let Some(index) = self.index()
            && let Some(long) = index.long_holding(ls - 1)
            && long.start < ls
        {
            return Some(RowPos {
                line_start: long.start,
                sub_row: u32::try_from(long.rows(cols) - 1).unwrap_or(u32::MAX),
            });
        }
        if !real_start {
            // Inside a long line folded without marks.
            return Some(RowPos {
                line_start: ls.saturating_sub(u64::from(cols)),
                sub_row: 0,
            });
        }
        let end = ls - 1;
        if end == 0 {
            return Some(RowPos::default());
        }
        match line_start_back(src, end) {
            Some(start) => Some(RowPos {
                line_start: start,
                sub_row: 0,
            }),
            None => {
                let index = self.index();
                let start = index
                    .and_then(|i| i.line_of_in(src, end))
                    .and_then(|line| index?.line_start_in(src, line))
                    .unwrap_or(end.saturating_sub(u64::from(cols)));
                Some(RowPos {
                    line_start: start,
                    sub_row: 0,
                })
            }
        }
    }

    /// The line number of `start`'s line.
    pub(crate) fn line_number(&self, start: u64) -> LineNo {
        if let (Some(index), Some(src)) = (self.index(), self.src())
            && let Some(line) = index.line_of_in(src, start)
        {
            return LineNo::Exact(line);
        }
        if let Some(pieces) = self.content.pieces() {
            let (byte, line, exact) = pieces.base;
            if let Some(n) = newlines_between(pieces, byte.min(start), byte.max(start)) {
                let line = if start >= byte {
                    line + n
                } else {
                    line.saturating_sub(n)
                };
                return if exact {
                    LineNo::Exact(line)
                } else {
                    LineNo::Estimated(line)
                };
            }
            return LineNo::Estimated((start as f64 * pieces.density) as u64);
        }
        LineNo::Estimated(0)
    }

    fn text_row<'s>(
        &'s self,
        src: &'s dyn Source,
        pos: RowPos,
        line: LineNo,
        bytes: Range<u64>,
    ) -> Option<Row<'s>> {
        // A BOM is not shown.
        let shown = if bytes.start == 0
            && slice(src, 0..3.min(self.size)).is_some_and(|b| *b == [0xEF, 0xBB, 0xBF])
        {
            3.min(bytes.end)..bytes.end
        } else {
            bytes.clone()
        };
        let raw = slice(src, shown.clone())?;
        let (text, map) = match raw {
            Cow::Borrowed(b) => cells::display(b),
            Cow::Owned(v) => {
                let (text, map) = cells::display(&v);
                (Cow::Owned(text.into_owned()), map)
            }
        };
        let tokens = self.tokens(pos.line_start, shown.clone(), map.as_deref());
        Some(Row::Text {
            pos,
            line,
            bytes,
            text,
            tokens: Cow::Owned(tokens),
            matches: self.matches(shown),
        })
    }

    /// The tokens meeting `row` (bytes of the line starting at
    /// `line_start`), relative to the row's shown text.
    fn tokens(&self, line_start: u64, row: Range<u64>, map: Option<&[(u32, u32)]>) -> Vec<Token> {
        let mut out = Vec::new();
        let Some(spans) = self.spans.iter().find(|s| s.range().contains(&row.start)) else {
            return out;
        };
        let key = line_start.max(spans.range().start);
        for token in spans.line(key) {
            let start = key + u64::from(token.start());
            let end = key + u64::from(token.end());
            let (a, b) = (start.max(row.start), end.min(row.end));
            if a >= b {
                continue;
            }
            let (mut a, mut b) = ((a - row.start) as u32, (b - row.start) as u32);
            if let Some(map) = map {
                a = cells::remap(map, a);
                b = cells::remap(map, b);
            }
            out.extend(Token::new(a, b.saturating_sub(a), token.kind()));
        }
        out
    }

    fn matches(&self, row: Range<u64>) -> &[Range<u64>] {
        let Some(find) = &self.find else {
            return &[];
        };
        let ranges = &find.ranges;
        let from = ranges.partition_point(|r| r.end <= row.start);
        let to = ranges.partition_point(|r| r.start < row.end.max(row.start + 1));
        &ranges[from..to.max(from)]
    }
}

fn next_line_no(line: LineNo) -> LineNo {
    match line {
        LineNo::Exact(n) => LineNo::Exact(n + 1),
        LineNo::Estimated(n) => LineNo::Estimated(n + 1),
    }
}

/// The start of the line holding `byte`, looking back at most 4 KiB; None
/// when no line start is found there or the bytes are not in memory.
fn line_start_back(src: &dyn Source, byte: u64) -> Option<u64> {
    if byte == 0 {
        return Some(0);
    }
    let lo = byte.saturating_sub(LONG_LINE);
    let bytes = slice(src, lo..byte)?;
    match memchr::memrchr(b'\n', &bytes) {
        Some(i) => Some(lo + i as u64 + 1),
        None if lo == 0 => Some(0),
        None => None,
    }
}

/// The line breaks in `[from, to)` when the pieces hold all of it.
fn newlines_between(pieces: &super::Pieces, from: u64, to: u64) -> Option<u64> {
    let bytes = slice(pieces, from..to)?;
    Some(memchr::memchr_iter(b'\n', &bytes).count() as u64)
}

/// Display row `k` of the long line folded from `start` without marks: its
/// bytes and, when it is the line's last row, the next line's start (None
/// at the file's end). None when the bytes are not in memory.
fn fold(
    src: &dyn Source,
    start: u64,
    k: u64,
    cols: u32,
    size: u64,
) -> Option<(Range<u64>, Option<Option<u64>>)> {
    let cols = u64::from(cols);
    let (row_start, row_end) = (k * cols, (k + 1) * cols);
    let (mut pos, mut cell) = (start, 0u64);
    let mut begin = None;
    while pos < size {
        let first = src.chunk(pos).and_then(|c| c.first().copied())?;
        if first == b'\n'
            || (first == b'\r'
                && src.chunk(pos + 1).and_then(|c| c.first().copied()) == Some(b'\n'))
        {
            let next = if first == b'\n' { pos + 1 } else { pos + 2 };
            let begin = begin.unwrap_or(pos);
            return Some((begin..pos, Some(Some(next))));
        }
        if begin.is_none() && cell >= row_start {
            begin = Some(pos);
        }
        if cell >= row_end {
            return Some((begin.unwrap_or(pos)..pos, None));
        }
        let (u, n) = unit_at(src, pos, size)?;
        cell += cells::width(u, cell);
        pos += n as u64;
    }
    Some((begin.unwrap_or(size)..size, Some(None)))
}
