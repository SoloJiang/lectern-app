//! Jumps in a large current text (docs/design/code_intel.md「大文件」, ADR
//! 0034): a text of 1–16 MiB is never parsed whole. Its identity and line
//! checkpoints (`LargeText`) are recorded once, by streaming it in 1 MiB
//! ranges through `git::BlobHasher` (the warming task, or the first jump);
//! a jump then reads only the checkpoint's 64 KiB around the target line
//! and parses only the window around the target, with an 8 ms deadline.
//! Past it, the name at the target goes on to steps 2 and 3 without a
//! binding. The text's other definitions come from its window units (step
//! 3, tier `CurrentFile`). An object's text is taken whole (an object can't
//! be read by ranges), unless a kept window parse covers the target.

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, SystemTime};

use lectern_git::BlobHasher;
use lectern_platform::{FileIdentity, FullRead, OpenFile, Opened, RelPath as SafeRel, SafeError};
use lectern_services::code_intel::{Position, QueryError, RefSyntax, TextOrigin, TextRef};
use lectern_services::project::BlobId;

use super::Snap;
use super::text::Content;
use crate::extract::{BACKGROUND_MAX_LEN, Def, Lang};
use crate::git_access::{blob_id, hash_blob, hash_kind};
use crate::index::chain::{HASH_RANGE, OPEN_MAX_LEN, racy_wait};
use crate::index::{Inner, lock};
use crate::site::{LocalBinding, ParseError, ParsedFile, Site};
use crate::store::Version;
use crate::window::{Base, ident, ident_end};

/// The checkpoints' spacing: the line at every 64 KiB.
pub const CHECKPOINT: u64 = 64 << 10;
/// A jump's window parse gives up after this long.
pub const WINDOW_DEADLINE: Duration = Duration::from_millis(8);
/// About this much before the target the window starts…
pub const BACK: usize = 8 << 10;
/// …at a safe position at most this much further back…
pub const BACK_MORE: usize = 4 << 10;
/// …and it ends this far past the target.
pub const AHEAD: usize = 4 << 10;
/// What a jump reads after a checkpoint: the 64 KiB in which its line
/// starts, and the window's margin past a target near their end.
const SPAN: u64 = CHECKPOINT + (AHEAD as u64) + (4 << 10);

/// A large text's identity and line checkpoints (「大文件」), recorded by
/// streaming it once; kept like the parses of current texts.
#[derive(Clone, Debug)]
pub(crate) struct LargeText {
    pub oid: BlobId,
    /// The worktree file's identity it was streamed under; None for an
    /// object's content (it can't change).
    pub identity: Option<FileIdentity>,
    /// The wall clock when the identity was taken: one taken within the
    /// racy window of the file's status change is not trusted.
    pub recorded: SystemTime,
    /// The bytes are the OID's content as they are (`Content::canonical`).
    pub canonical: bool,
    /// `lines[k]`: the line (from 0) of byte `k * CHECKPOINT`.
    pub lines: Arc<[u32]>,
}

impl LargeText {
    /// Where the start of `line` is looked for: the last checkpoint on an
    /// earlier line (its byte and line). `line` starts within the 64 KiB
    /// after it: the next checkpoint, if any, is on `line` or later. None
    /// for line 0, which starts at byte 0. Not the last checkpoint on
    /// `line` itself: `line` starts before that one (a line longer than
    /// 64 KiB, every line of a minified file).
    fn before_line(&self, line: u32) -> Option<(u64, u32)> {
        if line == 0 {
            return None;
        }
        let k = self.lines.partition_point(|l| *l < line).checked_sub(1)?;
        Some((k as u64 * CHECKPOINT, self.lines[k]))
    }

    /// Whether this record of a worktree file still stands for `file`:
    /// the same identity, taken out of the racy window.
    fn stands_for(&self, identity: FileIdentity) -> bool {
        self.identity == Some(identity) && racy_wait(self.recorded, identity.ctime).is_none()
    }
}

/// The line checkpoints of content streamed in pieces from offset 0.
#[derive(Default)]
struct Lines {
    lines: Vec<u32>,
    line: u32,
    at: u64,
}

impl Lines {
    fn update(&mut self, piece: &[u8]) {
        let mut i = 0;
        while i < piece.len() {
            if self.at.is_multiple_of(CHECKPOINT) {
                self.lines.push(self.line);
            }
            let take = ((CHECKPOINT - self.at % CHECKPOINT) as usize).min(piece.len() - i);
            self.line += memchr::memchr_iter(b'\n', &piece[i..i + take]).count() as u32;
            self.at += take as u64;
            i += take;
        }
    }
}

/// The window parse of a large text, kept with the parses of current
/// texts: the parse in the window's coordinates and where the window is.
pub(crate) struct WindowParse {
    pub parsed: Arc<ParsedFile>,
    pub base: Base,
}

impl WindowParse {
    /// The position in the window; None outside it: before its start, past
    /// the end of its line, or on a name the window's end cuts (the last
    /// line of a window is partial, and in a minified file the only one).
    fn local(&self, at: Position) -> Option<Position> {
        let local = local(self.base, at)?;
        let source = &**self.parsed.source();
        let mut start = 0;
        for _ in 0..local.line {
            start += memchr::memchr(b'\n', source.get(start..)?)? + 1;
        }
        let line_end = memchr::memchr(b'\n', &source[start..]).map_or(source.len(), |i| start + i);
        let offset = start + local.column as usize;
        (offset <= line_end && ident_end(source, offset).is_some()).then_some(local)
    }
}

/// What step 1 of a jump has of a large text: the site and the window's
/// definitions of its name, in the file's coordinates.
pub(crate) struct LargeSite {
    pub site: Site,
    pub defs: Vec<(Def, bool)>,
    pub canonical: bool,
}

/// The current text of a jump: whole (up to 1 MiB, parsed whole) or large.
pub(crate) enum CurrentText {
    Whole(Content<Arc<[u8]>>),
    Large(Box<LargeSite>),
}

/// A position of the window, in the file.
fn position(base: Base, at: Position) -> Position {
    base.position(at)
}

/// A position of the file, in the window; None before its start.
fn local(base: Base, at: Position) -> Option<Position> {
    let line = at.line.checked_sub(base.line)?;
    let column = if line == 0 {
        at.column.checked_sub(base.column)?
    } else {
        at.column
    };
    Some(Position { line, column })
}

fn binding(base: Base, mut b: LocalBinding) -> LocalBinding {
    b.at = position(base, b.at);
    b.byte += base.byte;
    b.def = b.def.map(|d| base.def(d));
    b
}

/// A site of the window, in the file.
fn site(base: Base, mut site: Site) -> Site {
    site.at = position(base, site.at);
    site.binding = site.binding.map(|b| binding(base, b));
    site.possible = site
        .possible
        .into_iter()
        .map(|b| binding(base, b))
        .collect();
    if let Some(d) = site.defines.as_mut() {
        d.def = base.def(d.def.clone());
    }
    site
}

/// The window's definitions named as `site`, in the file.
fn named_defs(w: &WindowParse, site: &Site) -> Vec<(Def, bool)> {
    w.parsed
        .defs_with_local()
        .filter(|(d, _)| *d.name == *site.name)
        .map(|(d, local)| (w.base.def(d.clone()), local))
        .collect()
}

/// Where a jump's window starts in `buf` (ADR 0034's first-screen window):
/// from about [`BACK`] before `target`, a safe position at most
/// [`BACK_MORE`] further back: a line beginning a statement at column 0,
/// else just after a `;` or `}`; else (a long literal or comment there)
/// just after the first `;` or `}` between there and the target; else the
/// first byte there that does not split an identifier.
pub(crate) fn safe_start(buf: &[u8], target: usize) -> usize {
    let from = target.saturating_sub(BACK);
    if from == 0 {
        return 0;
    }
    let lo = from.saturating_sub(BACK_MORE);
    let region = &buf[lo..from];
    for i in memchr::memrchr_iter(b'\n', region) {
        let start = lo + i + 1;
        let starts_statement = buf
            .get(start)
            .is_some_and(|b| !b.is_ascii_whitespace() && !matches!(b, b'}' | b')' | b']'));
        if starts_statement {
            return start;
        }
    }
    if let Some(i) = region.iter().rposition(|b| matches!(b, b';' | b'}')) {
        return lo + i + 1;
    }
    if let Some(i) = buf[from..target]
        .iter()
        .position(|b| matches!(b, b';' | b'}'))
    {
        return from + i + 1;
    }
    let mut at = from;
    while at < target && ident(buf[at - 1]) && ident(buf[at]) {
        at += 1;
    }
    at
}

/// The identifier at `at` in `bytes` (on it, or ending just before it: a
/// cursor right after a name); None when there is none.
pub(crate) fn ident_at(bytes: &[u8], at: usize) -> Option<Range<usize>> {
    let on = if bytes.get(at).is_some_and(|b| ident(*b)) {
        at
    } else if at > 0 && bytes.get(at - 1).is_some_and(|b| ident(*b)) {
        at - 1
    } else {
        return None;
    };
    let start = bytes[..on]
        .iter()
        .rposition(|b| !ident(*b))
        .map_or(0, |i| i + 1);
    let end = ident_end(bytes, on).unwrap_or(bytes.len());
    (!bytes[start].is_ascii_digit()).then_some(start..end)
}

/// Bytes of a large text from `start` (a byte of the file).
struct Around {
    buf: Arc<[u8]>,
    start: u64,
}

impl Around {
    /// Whether the bytes hold `range` of the file.
    fn covers(&self, range: &Range<u64>) -> bool {
        range.start >= self.start && range.end <= self.start + self.buf.len() as u64
    }
}

/// The start of `line` in the file, found in `around`, which holds the 64
/// KiB after `text`'s checkpoint before `line` ([`LargeText::before_line`]);
/// None when the file ends first (no such line).
fn line_start(text: &LargeText, around: &Around, line: u32) -> Option<u64> {
    let Some((cp, mut l)) = text.before_line(line) else {
        return Some(0);
    };
    let mut i = usize::try_from(cp.checked_sub(around.start)?).ok()?;
    while l < line {
        i += memchr::memchr(b'\n', around.buf.get(i..)?)? + 1;
        l += 1;
    }
    Some(around.start + i as u64)
}

/// What a jump reads around its target byte `at` (in a file of `len`
/// bytes): the window's margins before it and after it.
fn needed(at: u64, len: u64) -> Range<u64> {
    at.saturating_sub((BACK + BACK_MORE) as u64)..(at + AHEAD as u64).min(len)
}

/// The window around the target byte `at` (in the file; `around` holds
/// [`needed`] of it), on `line`, which starts at the file's byte
/// `line_start`: the window's range in `around` and where it starts in the
/// file.
fn window(around: &Around, at: u64, line_start: u64, line: u32) -> (Range<usize>, Base) {
    let buf = &around.buf;
    let target = (at - around.start) as usize;
    let ws = safe_start(buf, target);
    let we = (target + AHEAD).min(buf.len());
    let start = around.start + ws as u64;
    if start >= line_start {
        let base = Base {
            byte: start,
            line,
            column: (start - line_start) as u32,
        };
        return (ws..we, base);
    }
    // The window starts on an earlier line: `line_start` is inside the
    // bytes, after it.
    let ls = (line_start - around.start) as usize;
    let up = memchr::memchr_iter(b'\n', &buf[ws..ls]).count() as u32;
    let column = match memchr::memrchr(b'\n', &buf[..ws]) {
        Some(p) => Some(ws - p - 1),
        None if around.start == 0 => Some(ws),
        None => None,
    };
    match column {
        Some(c) => (
            ws..we,
            Base {
                byte: start,
                line: line - up,
                column: c as u32,
            },
        ),
        // That line starts before the bytes read: from the target's own
        // line.
        None => (
            ls..we,
            Base {
                byte: line_start,
                line,
                column: 0,
            },
        ),
    }
}

/// Tests: the checkpoints of `bytes` streamed in pieces of `piece` bytes.
#[cfg(test)]
pub(crate) fn checkpoints(bytes: &[u8], piece: usize) -> Vec<u32> {
    let mut lines = Lines::default();
    for chunk in bytes.chunks(piece) {
        lines.update(chunk);
    }
    lines.lines
}

#[cfg(test)]
thread_local! {
    /// Tests: the ranges a large text was read in on this thread (a
    /// harness runs its queries on the test's thread).
    pub(crate) static READS: std::cell::RefCell<Vec<Range<u64>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn record_read(_range: Range<u64>) {
    #[cfg(test)]
    READS.with_borrow_mut(|r| r.push(_range));
}

/// `[from, to)` of `file`, read while its identity is `identity`.
fn read(file: &OpenFile, identity: FileIdentity, from: u64, to: u64) -> Option<Vec<u8>> {
    record_read(from..to);
    let read = file.read_ranges(std::slice::from_ref(&(from..to))).ok()?;
    if read.meta.identity() != identity {
        return None;
    }
    read.pieces.into_iter().next()
}

impl Inner {
    /// Step 1's text (「跳转到定义」): read and verified whole up to 1 MiB
    /// (`read_text`'s rule), else, up to 16 MiB, only the window around
    /// `position` (「大文件」); over 16 MiB `TooLarge`.
    pub(crate) fn current_text(
        &self,
        snap: &Snap,
        at: &TextRef,
        lang: &Lang,
        position: Position,
        cancel: &AtomicBool,
    ) -> Result<CurrentText, QueryError> {
        let version = Version::of(lang);
        let now = self.watchdog.clock().now();
        // A kept window parse of this content around the target: nothing
        // read (the content is its OID's, as a whole parse kept under it).
        let kept = lock(&self.parse_cache).window(at.oid, &version, now);
        if let Some((w, canonical)) = kept
            && let Some(local) = w.local(position)
        {
            return self
                .window_site(&w, local, canonical)
                .map(|s| CurrentText::Large(Box::new(s)))
                .ok_or(QueryError::NoSymbol);
        }
        match self.open_text(snap, at)? {
            Source::Whole(content) => Ok(CurrentText::Whole(content)),
            Source::Object(bytes) => {
                // A worktree file's record serves when its bytes were the
                // content as they are: the same checkpoints.
                let text = match self.large_text(at.oid).filter(|t| t.canonical) {
                    Some(text) => text,
                    None => {
                        let text = self.object_text(at.oid, &bytes);
                        self.keep_large_text(text.clone());
                        text
                    }
                };
                let len = bytes.len() as u64;
                let around = Around {
                    buf: bytes,
                    start: 0,
                };
                let ls = line_start(&text, &around, position.line).ok_or(QueryError::NoSymbol)?;
                let target = (ls + u64::from(position.column)).min(len);
                self.parse_large(
                    lang,
                    at,
                    position,
                    text.canonical,
                    &around,
                    ls,
                    target,
                    cancel,
                )
                .map(|s| CurrentText::Large(Box::new(s)))
            }
            Source::File(file) => {
                let len = file.meta().size;
                let identity = file.identity();
                let text = match self.large_text(at.oid).filter(|t| t.stands_for(identity)) {
                    Some(text) => text,
                    // Not recorded yet (the warming did not finish), or the
                    // file changed since: streamed now, on the cold path.
                    None => {
                        let text = self.stream_text(snap, at, &file).ok_or(QueryError::Stale)?;
                        self.keep_large_text(text.clone());
                        text
                    }
                };
                // The 64 KiB after the checkpoint before the line, where the
                // line starts, with the window's margins: the window too,
                // unless the target is further along a long line.
                let cp = text.before_line(position.line).map_or(0, |(byte, _)| byte);
                let first = cp.saturating_sub((BACK + BACK_MORE) as u64)..(cp + SPAN).min(len);
                let read_range = |range: &Range<u64>| -> Result<Around, QueryError> {
                    let buf =
                        read(&file, identity, range.start, range.end).ok_or(QueryError::Stale)?;
                    Ok(Around {
                        buf: buf.into(),
                        start: range.start,
                    })
                };
                let around = read_range(&first)?;
                let ls = line_start(&text, &around, position.line).ok_or(QueryError::NoSymbol)?;
                let target = (ls + u64::from(position.column)).min(len);
                let need = needed(target, len);
                let around = if around.covers(&need) {
                    around
                } else {
                    read_range(&need)?
                };
                self.parse_large(
                    lang,
                    at,
                    position,
                    text.canonical,
                    &around,
                    ls,
                    target,
                    cancel,
                )
                .map(|s| CurrentText::Large(Box::new(s)))
            }
        }
    }

    /// The site and definitions of a kept window parse at `local`.
    fn window_site(&self, w: &WindowParse, local: Position, canonical: bool) -> Option<LargeSite> {
        let s = site(w.base, w.parsed.site_at(local)?);
        Some(LargeSite {
            defs: named_defs(w, &s),
            site: s,
            canonical,
        })
    }

    /// The window around the file's byte `target` (`position`, on a line
    /// starting at the byte `line_start`) in `around` parsed (8 ms) and
    /// kept; past the deadline, the name at the target alone.
    #[allow(
        clippy::too_many_arguments,
        reason = "one jump's text, its target and the bytes read around it"
    )]
    fn parse_large(
        &self,
        lang: &Lang,
        at: &TextRef,
        position: Position,
        canonical: bool,
        around: &Around,
        line_start: u64,
        target: u64,
        cancel: &AtomicBool,
    ) -> Result<LargeSite, QueryError> {
        let (range, base) = window(around, target, line_start, position.line);
        let clock = self.watchdog.clock().clone();
        let source: Arc<[u8]> = Arc::from(&around.buf[range]);
        let local_at = local(base, position).ok_or(QueryError::NoSymbol)?;
        let deadline = clock.now() + WINDOW_DEADLINE;
        // Whole (not `parse_at`): the kept window answers any site in it.
        let parsed = ParsedFile::parse(lang, source, &*clock, deadline, cancel);
        #[cfg(test)]
        let parsed = if self
            .window_parse_times_out
            .load(std::sync::atomic::Ordering::Acquire)
        {
            Err(ParseError::TimedOut)
        } else {
            parsed
        };
        match parsed {
            Ok(parsed) => {
                let w = WindowParse {
                    parsed: Arc::new(parsed),
                    base,
                };
                let found = self.window_site(&w, local_at, canonical);
                self.keep_window(at.oid, lang, w, canonical, clock.now());
                found.ok_or(QueryError::NoSymbol)
            }
            Err(ParseError::Unsupported) => Err(QueryError::UnsupportedLanguage),
            Err(ParseError::Cancelled) => Err(QueryError::Unreadable("cancelled".into())),
            // The name at the target goes on to steps 2 and 3, unbound.
            Err(ParseError::TimedOut) => {
                let i = (target - around.start) as usize;
                let name = ident_at(&around.buf, i).ok_or(QueryError::NoSymbol)?;
                let start = around.start + name.start as u64;
                let name =
                    std::str::from_utf8(&around.buf[name]).map_err(|_| QueryError::NoSymbol)?;
                Ok(LargeSite {
                    site: Site {
                        name: name.into(),
                        syntax: RefSyntax::Plain,
                        at: Position {
                            line: position.line,
                            column: start.saturating_sub(line_start) as u32,
                        },
                        binding: None,
                        possible: Vec::new(),
                        defines: None,
                    },
                    defs: Vec::new(),
                    canonical,
                })
            }
        }
    }

    /// Streams a worktree file in 1 MiB ranges: its OID (through
    /// `BlobHasher`), its identity unchanged across the ranges, and its
    /// line checkpoints. None when it is not `at.oid`'s content (also not
    /// after `canonical_oid`, the whole file read once for that).
    pub(crate) fn stream_text(
        &self,
        snap: &Snap,
        at: &TextRef,
        file: &OpenFile,
    ) -> Option<LargeText> {
        let identity = file.identity();
        let recorded = self.wall_now();
        let len = file.meta().size;
        let kind = hash_kind(&at.oid);
        let mut hasher = BlobHasher::new(kind, len);
        let mut lines = Lines::default();
        let mut from = 0;
        while from < len {
            let to = (from + HASH_RANGE).min(len);
            let piece = read(file, identity, from, to)?;
            if piece.len() as u64 != to - from {
                return None;
            }
            hasher.update(&piece);
            lines.update(&piece);
            from = to;
        }
        let raw = hasher.finish().map(|o| blob_id(&o));
        let canonical = if raw == Some(at.oid) {
            true
        } else {
            record_read(0..len);
            let bytes = match file.read_all(OPEN_MAX_LEN) {
                Ok(FullRead::File { bytes, meta }) if meta.identity() == identity => bytes,
                _ => return None,
            };
            if raw.is_none() && hash_blob(kind, &bytes) == at.oid {
                true
            } else if self.content_kind(snap, &at.path, &bytes, at.oid).is_some() {
                false
            } else {
                return None;
            }
        };
        Some(LargeText {
            oid: at.oid,
            identity: Some(identity),
            recorded,
            canonical,
            lines: lines.lines.into(),
        })
    }

    /// The recorded large text of `oid`, if kept; marks it used.
    fn large_text(&self, oid: BlobId) -> Option<LargeText> {
        let now = self.watchdog.clock().now();
        lock(&self.parse_cache).large(oid, now)
    }

    pub(crate) fn keep_large_text(&self, text: LargeText) {
        let now = self.watchdog.clock().now();
        lock(&self.parse_cache).put_large(text, now);
    }

    fn keep_window(&self, oid: BlobId, lang: &Lang, w: WindowParse, canonical: bool, now: Instant) {
        let version = Version::of(lang);
        let dropped = {
            let mut cache = lock(&self.parse_cache);
            let current = lock(&self.langs)
                .table
                .get(&lang.name().0)
                .is_some_and(|l| Version::of(l) == version);
            if !current {
                return;
            }
            cache.put_window(oid, version, w, canonical, now)
        };
        drop(dropped);
    }

    /// The text `at`, opened: read and verified whole up to 1 MiB
    /// ([`read_text`](Self::read_text)'s rule: the raw OID, or after the
    /// built-in conversions), else the open worktree file or the object's
    /// content; over 16 MiB `TooLarge`.
    fn open_text(&self, snap: &Snap, at: &TextRef) -> Result<Source, QueryError> {
        match &at.origin {
            // A dependency file is at most 1 MiB (ADR 0033): read whole.
            TextOrigin::Dependency(_) => self.read_text(snap, at).map(Source::Whole),
            TextOrigin::Object => {
                let tree = snap
                    .tree
                    .as_ref()
                    .ok_or_else(|| QueryError::Unreadable("no repository".into()))?;
                let bytes = match self.git.blob(tree, at.oid, OPEN_MAX_LEN) {
                    Ok(bytes) => bytes,
                    Err(lectern_git::GitError::TooLarge { .. }) => {
                        return Err(QueryError::TooLarge);
                    }
                    Err(error) => return Err(QueryError::Unreadable(error.to_string().into())),
                };
                Ok(if bytes.len() <= BACKGROUND_MAX_LEN {
                    Source::Whole(Content {
                        bytes,
                        canonical: true,
                    })
                } else {
                    Source::Object(bytes)
                })
            }
            TextOrigin::Worktree => {
                let root = snap
                    .root_of(at.worktree.as_ref())
                    .ok_or_else(|| QueryError::Unreadable("cannot open the worktree".into()))?;
                let rel = SafeRel::new(at.path.as_bytes()).ok_or(QueryError::Stale)?;
                crate::phases::count("file reads", 1);
                let file = match root.open_file(rel) {
                    Ok(Opened::File(file)) => file,
                    // Gone, or not a file any more: not that content.
                    Ok(_) | Err(SafeError::NotFound) => return Err(QueryError::Stale),
                    Err(error) => return Err(QueryError::Unreadable(error.to_string().into())),
                };
                let len = file.meta().size;
                if len > OPEN_MAX_LEN {
                    return Err(QueryError::TooLarge);
                }
                if len > BACKGROUND_MAX_LEN as u64 {
                    return Ok(Source::File(file));
                }
                let bytes = match file.read_all(BACKGROUND_MAX_LEN as u64) {
                    Ok(FullRead::File { bytes, .. }) => bytes,
                    // Grew since it was opened: not that content.
                    Ok(FullRead::TooLarge { .. }) => return Err(QueryError::Stale),
                    Err(error) => return Err(QueryError::Unreadable(error.to_string().into())),
                };
                match self.content_kind(snap, &at.path, &bytes, at.oid) {
                    Some(canonical) => Ok(Source::Whole(Content {
                        bytes: bytes.into(),
                        canonical,
                    })),
                    None => Err(QueryError::Stale),
                }
            }
        }
    }

    /// The record of an object's large content: its checkpoints.
    fn object_text(&self, oid: BlobId, bytes: &[u8]) -> LargeText {
        let mut lines = Lines::default();
        lines.update(bytes);
        LargeText {
            oid,
            identity: None,
            recorded: self.wall_now(),
            canonical: true,
            lines: lines.lines.into(),
        }
    }

    /// The warming of the text `at` (「当前文本的身份」): a text up to 1 MiB
    /// as it was ([`warm_parse`](Self::warm_parse): its whole parse kept),
    /// a large one's identity and checkpoints recorded, nothing parsed (a
    /// worktree file streamed, an object's content taken whole once).
    pub(crate) fn warm_text(self: &Arc<Self>, snap: &Snap, lang: &Lang, at: &TextRef) {
        let kept = self.large_text(at.oid);
        let usable = match at.origin {
            TextOrigin::Object => kept.is_some_and(|t| t.canonical),
            TextOrigin::Worktree => kept.is_some_and(|t| t.identity.is_some()),
            // Never large: its parse is kept whole.
            TextOrigin::Dependency(_) => false,
        };
        if usable {
            return;
        }
        match self.open_text(snap, at) {
            Ok(Source::Whole(text)) => self.keep_parse(lang, at.oid, text.bytes, text.canonical),
            Ok(Source::Object(bytes)) => {
                let text = self.object_text(at.oid, &bytes);
                self.keep_large_text(text);
            }
            Ok(Source::File(file)) => {
                if let Some(text) = self.stream_text(snap, at, &file) {
                    self.keep_large_text(text);
                }
            }
            Err(_) => {}
        }
    }
}

/// A text opened for step 1.
enum Source {
    /// Up to 1 MiB, read and verified.
    Whole(Content<Arc<[u8]>>),
    /// A large worktree file, not read yet.
    File(OpenFile),
    /// A large object's content.
    Object(Arc<[u8]>),
}
