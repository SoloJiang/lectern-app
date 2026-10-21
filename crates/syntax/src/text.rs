//! Text storage, the line index, line checkpoints and pages
//! (docs/design/file_reader.md「文本」「文本与显示行」, ADR 0034「文本存储与定位」).
//!
//! The line index is built in one pass that can be fed in pieces
//! ([`IndexBuilder`]): [`Text::from_bytes`] feeds the whole content at once,
//! tier C's streaming scan feeds 1 MiB reads. Both give the same index, the
//! same line checkpoints and the same facts (encoding, line endings).

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

use lectern_cache::{Compression, Entry, Key, Namespace};
use lectern_services::project::BlobId;

use crate::cells::{self, Unit};
use crate::highlight::Source;
use crate::limits::{CHECKPOINT_STRIDE, LONG_LINE, PAGE};

/// Lines between two entries of the line index.
pub(crate) const LINE_BLOCK: u64 = 1024;
/// A long line has a cell mark every 4 KiB.
pub(crate) const CELL_STRIDE: u64 = 4 << 10;

/// The full content of at most 16 MiB: immutable bytes and a line index with
/// an entry every 1024 lines. The bytes are one shared buffer, so content
/// that already is an `Arc<[u8]>` (an object database blob the review holds,
/// ADR 0042) is not copied.
pub struct Text {
    bytes: Arc<[u8]>,
    index: LineIndex,
}

impl Text {
    /// Copies the bytes once and builds the line index (16 MiB in a few
    /// milliseconds). Blocking; background threads only.
    pub fn from_bytes(bytes: Vec<u8>) -> Text {
        Text::shared(Arc::from(bytes))
    }

    /// The same over a buffer someone else holds too: no copy.
    pub fn shared(bytes: Arc<[u8]>) -> Text {
        Text::scanned(bytes).0
    }

    /// The text and its line checkpoints, from the same pass.
    pub(crate) fn scanned(bytes: Arc<[u8]>) -> (Text, LineCheckpoints) {
        let mut builder = IndexBuilder::new();
        builder.feed(&bytes);
        let scanned = builder.finish();
        let checkpoints = scanned.checkpoints();
        (
            Text {
                bytes,
                index: scanned.index,
            },
            checkpoints,
        )
    }

    pub fn len(&self) -> u64 {
        self.bytes.len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn index(&self) -> &LineIndex {
        &self.index
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The shared buffer.
    pub fn shared_bytes(&self) -> &Arc<[u8]> {
        &self.bytes
    }

    /// The memory this holds, for the LRU's accounting: the bytes (even when
    /// shared: the LRU keeps them alive) and the index.
    pub fn bytes(&self) -> usize {
        size_of::<Text>() + self.bytes.len() + self.index.bytes()
    }
}

impl Source for Text {
    fn len(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn chunk(&self, at: u64) -> Option<&[u8]> {
        usize::try_from(at).ok().and_then(|at| self.bytes.get(at..))
    }
}

/// A line longer than [`LONG_LINE`]: folded into display rows. Every 4 KiB
/// a mark records the cells before the first character boundary at or past
/// that offset, so finding a display row scans at most 4 KiB whatever the
/// line's length.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LongLine {
    pub start: u64,
    /// The end of what is shown: the `\n`, or the `\r` before it.
    pub end: u64,
    /// The next line's start; None for the last line (no line break after
    /// it).
    pub next: Option<u64>,
    pub cells: u64,
    /// `(offset, cells before it)`, ascending.
    marks: Box<[(u64, u64)]>,
}

impl LongLine {
    /// Display rows at `cols` cells: a character starting at cell `c` is on
    /// row `c / cols` (a wide character may stick out of its row by a cell).
    pub fn rows(&self, cols: u32) -> u64 {
        self.cells.div_ceil(u64::from(cols.max(1))).max(1)
    }

    /// The bytes of display row `k`. None when they are not in memory.
    pub(crate) fn row_range(&self, src: &dyn Source, k: u64, cols: u32) -> Option<Range<u64>> {
        let cols = u64::from(cols.max(1));
        let start = self.first_at_cell(src, k * cols)?;
        let end = self.first_at_cell(src, (k + 1) * cols)?;
        Some(start..end.max(start))
    }

    /// The display row holding `byte`.
    pub(crate) fn row_of(&self, src: &dyn Source, byte: u64, cols: u32) -> Option<u64> {
        let byte = byte.clamp(self.start, self.end);
        let i = self.marks.partition_point(|&(off, _)| off <= byte);
        let (from, cells) = if i == 0 {
            (self.start, 0)
        } else {
            self.marks[i - 1]
        };
        let (_, cells) = walk(src, from, cells, self.end, |pos, _| pos >= byte)?;
        Some((cells / u64::from(cols.max(1))).min(self.rows(cols) - 1))
    }

    /// The first unit whose start cell is at least `target`, or the end.
    fn first_at_cell(&self, src: &dyn Source, target: u64) -> Option<u64> {
        if target == 0 {
            return Some(self.start);
        }
        if target >= self.cells {
            return Some(self.end);
        }
        let i = self.marks.partition_point(|&(_, c)| c < target);
        let (from, cells) = if i == 0 {
            (self.start, 0)
        } else {
            self.marks[i - 1]
        };
        walk(src, from, cells, self.end, |_, c| c >= target).map(|(pos, _)| pos)
    }

    fn bytes(&self) -> usize {
        size_of::<LongLine>() + size_of_val::<[(u64, u64)]>(&self.marks)
    }
}

/// Walks the units of `[from, end)`, `from` at `cells`, until `stop(pos,
/// cells)` holds before a unit: that position and its cells, or the end's.
/// None when bytes are not in memory.
pub(crate) fn walk(
    src: &dyn Source,
    from: u64,
    cells: u64,
    end: u64,
    mut stop: impl FnMut(u64, u64) -> bool,
) -> Option<(u64, u64)> {
    let (mut pos, mut cells) = (from, cells);
    while pos < end {
        if stop(pos, cells) {
            return Some((pos, cells));
        }
        let (u, n) = unit_at(src, pos, end)?;
        cells += cells::width(u, cells);
        pos += n as u64;
    }
    Some((end, cells))
}

/// The unit at `pos`, not reaching past `end`; None when `pos` is not in
/// memory. A sequence crossing the end of a chunk (a page) is put together.
pub(crate) fn unit_at(src: &dyn Source, pos: u64, end: u64) -> Option<(Unit, usize)> {
    let limit = (end - pos).min(4) as usize;
    let chunk = src.chunk(pos).filter(|c| !c.is_empty())?;
    if chunk.len() >= limit {
        return Some(cells::unit(&chunk[..limit.max(1).min(chunk.len())]));
    }
    let mut buf = [0u8; 4];
    let mut have = chunk.len();
    buf[..have].copy_from_slice(chunk);
    while have < limit {
        match src.chunk(pos + have as u64).filter(|c| !c.is_empty()) {
            Some(next) => {
                let take = next.len().min(limit - have);
                buf[have..have + take].copy_from_slice(&next[..take]);
                have += take;
            }
            None => break,
        }
    }
    Some(cells::unit(&buf[..have]))
}

/// Encoding and line-ending facts of content, from its line index.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Facts {
    pub nul: bool,
    /// A byte that does not start a valid UTF-8 sequence.
    pub invalid: bool,
    pub bom: bool,
    /// Lines ending in `\r\n` and in a bare `\n`.
    pub crlf: u64,
    pub lf: u64,
}

impl Facts {
    pub(crate) fn is_text(&self) -> bool {
        !self.nul && !self.invalid
    }

    pub(crate) fn mixed_line_endings(&self) -> bool {
        self.crlf > 0 && self.lf > 0
    }
}

/// The line index: the start of every 1024th line, the long lines (over
/// 4 KiB) with their cell marks, the widest line in cells, the line count.
/// Lines end at `\n`; content with `n` line breaks has `n + 1` lines.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LineIndex {
    len: u64,
    line_count: u64,
    /// `block_starts[k]` is the start of line `k × 1024`.
    block_starts: Box<[u64]>,
    longs: Box<[LongLine]>,
    widest_cells: u32,
    facts: Facts,
}

impl LineIndex {
    pub fn line_count(&self) -> u64 {
        self.line_count
    }

    /// The start of line `line` (the content's end past the last line): an
    /// index entry, then at most 1023 line breaks.
    pub fn line_start(&self, text: &Text, line: u64) -> u64 {
        self.line_start_in(text, line).unwrap_or(self.len)
    }

    /// The line holding `byte`.
    pub fn line_of(&self, text: &Text, byte: u64) -> u64 {
        self.line_of_in(text, byte)
            .unwrap_or(self.line_count.saturating_sub(1))
    }

    pub fn has_long_line(&self) -> bool {
        !self.longs.is_empty()
    }

    pub fn widest_cells(&self) -> u32 {
        self.widest_cells
    }

    /// The long lines, by start.
    pub fn long_lines(&self) -> &[LongLine] {
        &self.longs
    }

    pub(crate) fn facts(&self) -> Facts {
        self.facts
    }

    /// The long line starting at `start`.
    pub(crate) fn long_at(&self, start: u64) -> Option<&LongLine> {
        self.longs
            .binary_search_by_key(&start, |l| l.start)
            .ok()
            .map(|i| &self.longs[i])
    }

    /// The long line holding `byte` (its `\r` and `\n` included).
    pub(crate) fn long_holding(&self, byte: u64) -> Option<&LongLine> {
        let i = self.longs.partition_point(|l| l.start <= byte);
        let long = self.longs.get(i.checked_sub(1)?)?;
        (byte < long.next.unwrap_or(u64::MAX)).then_some(long)
    }

    /// Display rows beyond one per line, at `cols`.
    pub(crate) fn extra_rows(&self, cols: u32) -> u64 {
        self.longs.iter().map(|l| l.rows(cols) - 1).sum()
    }

    /// The last index entry at or before `byte`: its line and start.
    pub(crate) fn block_before(&self, byte: u64) -> (u64, u64) {
        let i = self
            .block_starts
            .partition_point(|&s| s <= byte)
            .saturating_sub(1);
        (
            i as u64 * LINE_BLOCK,
            self.block_starts.get(i).copied().unwrap_or(0),
        )
    }

    /// [`line_start`](Self::line_start) over any source; None when the
    /// bytes it needs are not in memory.
    pub(crate) fn line_start_in(&self, src: &dyn Source, line: u64) -> Option<u64> {
        if line >= self.line_count {
            return Some(self.len);
        }
        let block = (line / LINE_BLOCK) as usize;
        let mut at = *self.block_starts.get(block)?;
        let mut skip = line % LINE_BLOCK;
        while skip > 0 {
            let chunk = src.chunk(at).filter(|c| !c.is_empty())?;
            let mut found = None;
            for i in memchr::memchr_iter(b'\n', chunk) {
                skip -= 1;
                if skip == 0 {
                    found = Some(i);
                    break;
                }
            }
            match found {
                Some(i) => return Some(at + i as u64 + 1),
                None => at += chunk.len() as u64,
            }
        }
        Some(at)
    }

    /// [`line_of`](Self::line_of) over any source.
    pub(crate) fn line_of_in(&self, src: &dyn Source, byte: u64) -> Option<u64> {
        let byte = byte.min(self.len);
        let (mut line, mut at) = self.block_before(byte);
        while at < byte {
            let chunk = src.chunk(at).filter(|c| !c.is_empty())?;
            let take = chunk.len().min((byte - at) as usize);
            line += memchr::memchr_iter(b'\n', &chunk[..take]).count() as u64;
            at += take as u64;
        }
        Some(line)
    }

    /// The memory this holds.
    pub fn bytes(&self) -> usize {
        size_of::<LineIndex>()
            + size_of_val::<[u64]>(&self.block_starts)
            + self.longs.iter().map(LongLine::bytes).sum::<usize>()
    }
}

/// What a pass over content gives: the line index and the line
/// checkpoints.
pub(crate) struct Scanned {
    pub index: LineIndex,
    /// `checkpoints[k]` is the number of line breaks before byte
    /// `k × 64 KiB`, for `k × 64 KiB ≤ len`.
    pub checkpoints: Box<[u32]>,
}

impl Scanned {
    pub(crate) fn checkpoints(&self) -> LineCheckpoints {
        LineCheckpoints {
            len: self.index.len,
            lines: self.checkpoints.clone(),
            line_count: self.index.line_count,
            has_long_line: self.index.has_long_line(),
            widest_cells: self.index.widest_cells,
        }
    }
}

/// Builds the line index in one pass over content fed in pieces.
pub(crate) struct IndexBuilder {
    /// The offset of the next byte fed (the tail held back included).
    at: u64,
    /// Up to 3 bytes at the end of the last piece that may start a UTF-8
    /// sequence the next piece completes.
    tail: Vec<u8>,
    newlines: u64,
    line_start: u64,
    cells: u64,
    last_cr: bool,
    marks: Vec<(u64, u64)>,
    next_mark: u64,
    longs: Vec<LongLine>,
    block_starts: Vec<u64>,
    checkpoints: Vec<u32>,
    next_checkpoint: u64,
    widest: u64,
    facts: Facts,
}

impl IndexBuilder {
    pub(crate) fn new() -> IndexBuilder {
        IndexBuilder {
            at: 0,
            tail: Vec::new(),
            newlines: 0,
            line_start: 0,
            cells: 0,
            last_cr: false,
            marks: Vec::new(),
            next_mark: CELL_STRIDE,
            longs: Vec::new(),
            block_starts: vec![0],
            checkpoints: Vec::new(),
            next_checkpoint: 0,
            widest: 0,
            facts: Facts::default(),
        }
    }

    /// The next piece of the content.
    pub(crate) fn feed(&mut self, piece: &[u8]) {
        let base = self.at;
        if base == 0 && piece.starts_with(&[0xEF, 0xBB, 0xBF]) {
            self.facts.bom = true;
        }
        let mut start = 0;
        if !self.tail.is_empty() {
            // The units starting in the held-back tail, decoded with the
            // first bytes of this piece.
            let held = self.tail.len();
            let mut buf = std::mem::take(&mut self.tail);
            buf.extend_from_slice(&piece[..piece.len().min(4)]);
            if buf.len() <= 3 && held_back(&buf) == buf.len() {
                // Still one incomplete sequence: wait for the next piece.
                self.tail = buf;
                self.at = base + piece.len() as u64;
                return;
            }
            let tail_at = base - held as u64;
            let mut p = 0;
            while p < held {
                if buf[p] == b'\n' {
                    break;
                }
                let (u, n) = cells::unit(&buf[p..]);
                self.unit(u, tail_at + p as u64);
                p += n;
            }
            start = p - held;
        }
        let end = piece.len() - held_back(piece);
        let mut from = start;
        for nl in memchr::memchr_iter(b'\n', &piece[start..end]) {
            let nl = start + nl;
            self.segment(&piece[from..nl], base + from as u64);
            self.end_line(base + nl as u64);
            from = nl + 1;
        }
        self.segment(&piece[from..end], base + from as u64);
        self.tail.extend_from_slice(&piece[end..]);
        self.at = base + piece.len() as u64;
    }

    /// The bytes of the current line in `[abs, abs + bytes.len())`.
    fn segment(&mut self, bytes: &[u8], abs: u64) {
        if bytes.is_empty() {
            return;
        }
        if memchr::memchr(0, bytes).is_some() {
            self.facts.nul = true;
        }
        if bytes.is_ascii() && memchr::memchr(b'\t', bytes).is_none() {
            // One cell per byte, control characters too.
            let end = abs + bytes.len() as u64;
            while self.next_mark < end {
                // At or after the segment's start: a sequence that crossed
                // the mark ended at `abs`.
                let at = self.next_mark.max(abs);
                self.marks.push((at, self.cells + (at - abs)));
                self.next_mark += CELL_STRIDE;
            }
            self.cells += bytes.len() as u64;
            self.last_cr = bytes.last() == Some(&b'\r');
            return;
        }
        let mut p = 0;
        while p < bytes.len() {
            let (u, n) = cells::unit(&bytes[p..]);
            self.unit(u, abs + p as u64);
            p += n;
        }
    }

    fn unit(&mut self, u: Unit, at: u64) {
        if at >= self.next_mark {
            self.marks.push((at, self.cells));
            self.next_mark += CELL_STRIDE;
        }
        if u == Unit::Invalid {
            self.facts.invalid = true;
        }
        self.cells += cells::width(u, self.cells);
        self.last_cr = u == Unit::Control(b'\r');
    }

    fn end_line(&mut self, newline: u64) {
        let (end, cells) = self.shown_end(newline);
        if self.last_cr {
            self.facts.crlf += 1;
        } else {
            self.facts.lf += 1;
        }
        self.close_line(end, cells, Some(newline + 1));
        while self.next_checkpoint <= newline {
            self.checkpoints.push(self.newlines as u32);
            self.next_checkpoint += CHECKPOINT_STRIDE;
        }
        self.newlines += 1;
        self.line_start = newline + 1;
        self.cells = 0;
        self.last_cr = false;
        self.marks.clear();
        self.next_mark = self.line_start + CELL_STRIDE;
        if self.newlines.is_multiple_of(LINE_BLOCK) {
            self.block_starts.push(self.line_start);
        }
    }

    /// The end of the current line's shown part and its cells: a `\r`
    /// before the end is not shown.
    fn shown_end(&self, end: u64) -> (u64, u64) {
        if self.last_cr {
            (end - 1, self.cells - 1)
        } else {
            (end, self.cells)
        }
    }

    fn close_line(&mut self, end: u64, cells: u64, next: Option<u64>) {
        self.widest = self.widest.max(cells);
        if end - self.line_start > LONG_LINE {
            let marks: Vec<(u64, u64)> = self
                .marks
                .iter()
                .copied()
                .filter(|&(off, _)| off < end)
                .collect();
            self.longs.push(LongLine {
                start: self.line_start,
                end,
                next,
                cells,
                marks: marks.into(),
            });
        }
    }

    pub(crate) fn finish(mut self) -> Scanned {
        let tail = std::mem::take(&mut self.tail);
        let tail_at = self.at - tail.len() as u64;
        let mut p = 0;
        while p < tail.len() {
            let (u, n) = cells::unit(&tail[p..]);
            self.unit(u, tail_at + p as u64);
            p += n;
        }
        let len = self.at;
        let (end, cells) = self.shown_end(len);
        self.close_line(end, cells, None);
        while self.next_checkpoint <= len {
            self.checkpoints.push(self.newlines as u32);
            self.next_checkpoint += CHECKPOINT_STRIDE;
        }
        Scanned {
            index: LineIndex {
                len,
                line_count: self.newlines + 1,
                block_starts: self.block_starts.into(),
                longs: self.longs.into(),
                widest_cells: u32::try_from(self.widest).unwrap_or(u32::MAX),
                facts: self.facts,
            },
            checkpoints: self.checkpoints.into(),
        }
    }
}

/// How many bytes at the end of `piece` may start a sequence the next piece
/// completes: a lead byte in the last three with too few continuation bytes
/// after it.
fn held_back(piece: &[u8]) -> usize {
    for back in 1..=piece.len().min(3) {
        let b = piece[piece.len() - back];
        if b & 0xC0 == 0x80 {
            continue;
        }
        let need = match b {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return 0,
        };
        return if need > back { back } else { 0 };
    }
    0
}

/// Line checkpoints (ADR 0034): the line number at every 64 KiB, stored in
/// `syntax.lines` by the content's OID (the raw bytes as a git blob), only
/// for content of at least 1 MiB
/// ([`CHECKPOINT_MIN`](crate::limits::CHECKPOINT_MIN)).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LineCheckpoints {
    /// The content's length: the first screen compares it with the file's
    /// size before using checkpoints found by a candidate OID.
    pub len: u64,
    /// `lines[k]` is the number of line breaks before byte `k × 64 KiB`.
    pub lines: Box<[u32]>,
    pub line_count: u64,
    pub has_long_line: bool,
    pub widest_cells: u32,
}

impl LineCheckpoints {
    /// The cache key: the OID's bytes.
    pub fn key(oid: &BlobId) -> Key {
        Key::new().bytes(oid.as_bytes())
    }

    /// The last checkpoint before line `line` starts: its byte and the
    /// line that byte is on. The line starts at most 64 KiB after it.
    pub fn before_line(&self, line: u32) -> (u64, u32) {
        let k = self.lines.partition_point(|&l| l < line).saturating_sub(1);
        (
            k as u64 * CHECKPOINT_STRIDE,
            self.lines.get(k).copied().unwrap_or(0),
        )
    }
}

impl Entry for LineCheckpoints {
    const NAMESPACE: Namespace = Namespace {
        name: "syntax.lines",
        version: 1,
        compression: Compression::None,
    };

    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.len.to_le_bytes());
        out.extend_from_slice(&self.line_count.to_le_bytes());
        out.extend_from_slice(&self.widest_cells.to_le_bytes());
        out.push(u8::from(self.has_long_line));
        for line in &self.lines {
            out.extend_from_slice(&line.to_le_bytes());
        }
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let (head, rest) = bytes.split_at_checked(21)?;
        let len = u64::from_le_bytes(head[0..8].try_into().ok()?);
        let line_count = u64::from_le_bytes(head[8..16].try_into().ok()?);
        let widest_cells = u32::from_le_bytes(head[16..20].try_into().ok()?);
        let has_long_line = match head[20] {
            0 => false,
            1 => true,
            _ => return None,
        };
        if rest.len() % 4 != 0 || (rest.len() / 4) as u64 != len / CHECKPOINT_STRIDE + 1 {
            return None;
        }
        let lines: Box<[u32]> = rest
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        if lines.first() != Some(&0) || lines.windows(2).any(|w| w[0] > w[1]) {
            return None;
        }
        Some(LineCheckpoints {
            len,
            lines,
            line_count,
            has_long_line,
            widest_cells,
        })
    }
}

/// The pages of a file over 16 MiB that are in memory (tier C): 64 KiB
/// pages read from the document's `OpenFile`, at most
/// [`PAGE_LRU`](crate::limits::PAGE_LRU) of them. Immutable: reading more
/// pages makes a new `Pages`, which shares the pages it keeps.
#[derive(Clone, Default)]
pub struct Pages {
    len: u64,
    /// By page number.
    pages: BTreeMap<u64, Arc<[u8]>>,
}

impl Pages {
    pub(crate) fn new(len: u64) -> Pages {
        Pages {
            len,
            pages: BTreeMap::new(),
        }
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether page `page` is in memory.
    pub fn has(&self, page: u64) -> bool {
        self.pages.contains_key(&page)
    }

    /// The pages in memory.
    pub fn count(&self) -> usize {
        self.pages.len()
    }

    /// The memory the pages hold.
    pub fn bytes(&self) -> usize {
        self.pages.values().map(|p| p.len()).sum()
    }

    pub(crate) fn insert(&mut self, page: u64, bytes: Arc<[u8]>) {
        self.pages.insert(page, bytes);
    }

    pub(crate) fn without(&mut self, page: u64) {
        self.pages.remove(&page);
    }
}

impl Source for Pages {
    fn len(&self) -> u64 {
        self.len
    }

    /// Only the pages in memory.
    fn chunk(&self, at: u64) -> Option<&[u8]> {
        let page = self.pages.get(&(at / PAGE))?;
        page.get((at % PAGE) as usize..)
    }
}
