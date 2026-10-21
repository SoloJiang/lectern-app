//! A document's tasks (docs/design/file_reader.md「数据流与线程」):
//!
//! - the first screen (`Interactive`): open, one `read_ranges` (or the
//!   object content someone holds), `window::plan`, the window's parse with
//!   the 8 ms deadline;
//! - the full read (`Background`): `read_all` up to 16 MiB into a `Text`, or
//!   tier C's streaming scan in 1 MiB reads; the raw bytes' OID, the line
//!   index, the line checkpoints into `syntax.lines`;
//! - tier A's whole-file parse (`Interactive`, 8 ms slices);
//! - the visible windows of tiers B and C (the chain's, one on each side,
//!   `chain`) and tier C's pages (`Interactive`), on scrolling;
//! - find (`Interactive`, 1 MiB blocks).
//!
//! Every task holds a `BusyGuard` and checks its load (and the document's
//! cancel flag) before it publishes anything. The rest of the window chain
//! is batch work (`chain`).

use std::ops::Range;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use lectern_executor::Priority;
use lectern_git::{BlobHasher, GitError, Oid};
use lectern_platform::{FileIdentity, FullRead, OpenFile, Opened, RelPath, SafeError, SafeRoot};
use lectern_services::code_intel::{DEPENDENCY_EXTENSIONS, DepRoot, TextOrigin};
use lectern_services::project::{BlobId, ProjectKey};
use lectern_services::reader::{Anchor, ReadTarget};

use crate::find::{BLOCK, FindQuery, FindResults, Finder, find_in};
use crate::git_access::blob_id;
use crate::highlight::{self, Budget, FullParse, Outcome, Source};
use crate::lang::Rules;
use crate::limits::{
    CHECKPOINT_MIN, FIRST_WINDOW_DEADLINE, OBJECT_MAX, PAGE, SLICE, TIER_A_MAX, TIER_B_MAX,
};
use crate::lru::{Class, LruKey, LruValue, SpansOf, WHOLE};
use crate::spans::WindowSpans;
use crate::text::{IndexBuilder, LineIndex, Text};
use crate::util::{log, yield_now};
use crate::window::{self, Encoding, Target};

use super::chain::{self, Computed, Want};
use super::content::{Content, PageCache, Paged, Piece, Pieces, slice};
use super::engine::{Doc, DocMut, EngineInner};
use super::{
    DocState, Editable, Highlighting, LineCount, NotEditable, ReadError, RowPos, Tier, watch,
};

/// A visible window of tiers B and C parsed alone (before the chain gets
/// there): one call, not resumable.
const SCROLL_WINDOW_BUDGET: Duration = Duration::from_millis(50);
/// The streaming scan's reads.
pub(crate) const SCAN_BLOCK: u64 = 1 << 20;
/// Tier C reads the pages from 64 KiB before the top to 192 KiB after it.
const PAGES_BEFORE: u64 = 64 << 10;
const PAGES_AFTER: u64 = 192 << 10;
/// The file's head the plan looks at.
const HEAD: u64 = 8 << 10;
/// The bytes a visible window's plan reads around the top.
const AROUND_BEFORE: u64 = 64 << 10;
const AROUND_AFTER: u64 = 68 << 10;

/// What the first screen found.
pub(crate) enum First {
    Failed(DocState),
    Shown(Box<Shown>),
}

pub(crate) struct Shown {
    size: u64,
    identity: Option<FileIdentity>,
    /// What the full read continues from.
    rest: Rest,
    pieces: Arc<Pieces>,
    /// Its line start is the plan's target line start.
    pub(crate) anchor: RowPos,
    pub(crate) window: Option<Arc<WindowSpans>>,
    /// The window's parse stopped for the yield probe (the prefetch).
    pub(crate) yielded: bool,
    whole: Option<Arc<WindowSpans>>,
    /// The verified OID `whole` was found by.
    whole_from: Option<BlobId>,
    highlight: Highlighting,
    lang: Option<&'static str>,
    lines: u64,
    oid: Option<BlobId>,
    relocate: bool,
}

/// What the full read starts from.
pub(crate) enum Rest {
    File(OpenFile),
    Memory(Arc<[u8]>),
    Text(Arc<Text>),
}

/// Opens the content again after it changed on disk, at `at`. The caller's
/// `expect` described the content it saw, which is gone: this load and the
/// later ones use no `expect` (the byte anchor's OID check, the line
/// checkpoints' candidate).
pub(crate) fn reload(engine: &Arc<EngineInner>, doc: &Arc<Doc>, at: Anchor) {
    if doc.target.origin == TextOrigin::Worktree {
        doc.lock().expect = None;
    }
    start(engine, doc, at);
}

/// Starts a new load: cancels the last one's tasks (a find in flight
/// included) and reads the first screen at `at`.
pub(crate) fn start(engine: &Arc<EngineInner>, doc: &Arc<Doc>, at: Anchor) {
    let (load, cancel, rows, expect) = {
        let mut st = doc.lock();
        st.cancel.store(true, Ordering::SeqCst);
        st.cancel = Arc::new(AtomicBool::new(false));
        st.load += 1;
        st.at = at;
        st.window_busy = false;
        st.window_again = false;
        st.pages_busy = false;
        // The chain is of the last load's content.
        st.chain = None;
        // A find of the last load publishes nothing more; the query runs
        // again in the new content.
        st.find.cancel.store(true, Ordering::SeqCst);
        st.find.cancel = Arc::new(AtomicBool::new(false));
        st.find.generation += 1;
        st.find.results = None;
        st.find.waiting = st.find.query.is_some();
        if st.state == DocState::Ready && st.editable != Editable::No(NotEditable::ReadOnlySource) {
            st.editable = Editable::No(NotEditable::Reloading);
        }
        (st.load, st.cancel.clone(), st.viewport.rows, st.expect)
    };
    let (engine2, doc2) = (engine.clone(), doc.clone());
    let spawned = engine.spawn(Priority::Interactive, async move {
        if doc2.stale(&doc2.lock(), load) {
            return;
        }
        let first = first_screen(
            &engine2,
            &doc2.target,
            &doc2.root,
            at,
            expect,
            rows,
            first_budget(&cancel),
            Class::Opened,
        );
        apply_first(&engine2, &doc2, load, first);
    });
    if !spawned {
        log(format_args!(
            "open before the engine started: the document stays empty"
        ));
    }
}

fn io_error(error: impl std::fmt::Display) -> DocState {
    DocState::Unreadable(ReadError::Io(error.to_string().into()))
}

/// The first screen's window budget: the 8 ms deadline, no yield.
pub(crate) fn first_budget(cancel: &AtomicBool) -> Budget<'_> {
    Budget {
        budget: FIRST_WINDOW_DEADLINE,
        cancel,
        yield_probe: None,
    }
}

/// The first screen of `target`, on the calling thread; its window is
/// parsed with `budget` ([`first_budget`] when opening, the prefetch's
/// otherwise). What it finds in the LRU is used as `class`: `Opened` when
/// the user opens, `Background` for the prefetch, which must not make its
/// own entries the user's.
#[expect(
    clippy::too_many_arguments,
    reason = "the target, where it opens, the window's budget and the LRU class"
)]
pub(crate) fn first_screen(
    engine: &EngineInner,
    target: &ReadTarget,
    root: &Arc<Path>,
    at: Anchor,
    expect: Option<BlobId>,
    rows: u32,
    budget: Budget<'_>,
    class: Class,
) -> First {
    let langs = engine.languages.clone();
    let lang = langs.for_path(&target.path);
    let opened = match &target.origin {
        TextOrigin::Worktree => worktree_file(engine, target, root),
        TextOrigin::Dependency(dep) => dependency_file(engine, target, root, &dep.root),
        TextOrigin::Object => object(engine, target, class).map(|rest| {
            let size = match &rest {
                Rest::Memory(b) => b.len() as u64,
                Rest::Text(t) => t.len(),
                Rest::File(_) => 0,
            };
            (rest, size, None)
        }),
    };
    let (rest, size, identity) = match opened {
        Ok(opened) => opened,
        Err(state) => return First::Failed(state),
    };
    // The OID of the content just opened, when it is known for sure: an
    // object's is `expect`; a file's is what an earlier read of this very
    // identity found. Only this one finds spans in the LRU.
    let verified = match target.origin {
        TextOrigin::Object => target.expect,
        TextOrigin::Worktree | TextOrigin::Dependency(_) => {
            identity.and_then(|id| engine.known_oid(root, &target.path, id))
        }
    };
    // Line checkpoints may also come by the caller's OID: their length is
    // checked, and the full read places the line again anyway.
    let candidate = verified.or(expect);
    let (ranges, plan_target, relocate) = match at {
        Anchor::Start => (window::first_read(size, 0), Target::Byte(0), false),
        Anchor::Byte { byte, .. } => {
            let byte = byte.min(size);
            (window::first_read(size, byte), Target::Byte(byte), false)
        }
        Anchor::Line { line, .. } if size <= TIER_A_MAX => (
            window::first_read(size, 0),
            Target::LineFrom {
                checkpoint_byte: 0,
                checkpoint_line: 0,
                line,
            },
            false,
        ),
        Anchor::Line { line, .. } => {
            let checkpoints = candidate
                .and_then(|oid| engine.checkpoints(&target.project, &oid))
                .filter(|c| c.len == size);
            match checkpoints {
                Some(c) => {
                    let (byte, at_line) = c.before_line(line);
                    (
                        window::checkpoint_read(size, byte),
                        Target::LineFrom {
                            checkpoint_byte: byte,
                            checkpoint_line: at_line,
                            line,
                        },
                        false,
                    )
                }
                None => (window::first_read(size, 0), Target::Byte(0), true),
            }
        }
    };
    let pieces = match read(&rest, &ranges) {
        Ok(pieces) => pieces,
        Err(e) => return First::Failed(io_error(e)),
    };
    let (Some(head), Some(around)) = (pieces.first(), pieces.last()) else {
        return First::Failed(io_error("nothing read"));
    };
    let rules = lang.map_or(Rules::Code, |l| l.rules());
    let head_bytes = &head.as_slice()[..(head.len as u64).min(HEAD) as usize];
    let plan = window::plan(
        rules,
        head_bytes,
        around.as_slice(),
        around.start,
        size,
        plan_target,
        rows,
    );
    let newlines = memchr::memchr_iter(b'\n', around.as_slice()).count();
    let density = if around.len == 0 {
        0.0
    } else {
        newlines as f64 / around.len as f64
    };
    let base = match plan_target {
        Target::LineFrom { line, .. } => (plan.target_line_start, u64::from(line), true),
        Target::Byte(_) if around.start == 0 => (0, 0, true),
        Target::Byte(_) => (around.start, (around.start as f64 * density) as u64, false),
    };
    let pieces = Arc::new(normalize(Pieces {
        size,
        pieces,
        base,
        density,
    }));
    let lines = (size as f64 * density) as u64 + 1;
    let anchor = RowPos {
        line_start: plan.target_line_start,
        sub_row: 0,
    };
    // Spans: the whole file's from the LRU when the content is known, else
    // the window a prefetch left there for this target, else the window
    // parsed alone.
    let (mut window, mut whole, mut highlight) = (None, None, Highlighting::Pending);
    let mut whole_from = None;
    let mut yielded = false;
    match lang {
        None => highlight = Highlighting::Plain,
        Some(_) if plan.encoding == Encoding::NotText => highlight = Highlighting::Plain,
        Some(lang) => {
            let cached = verified.and_then(|oid| engine.lru.spans(&whole_key(oid, lang), class));
            let screen = || {
                let key = LruKey::Screen {
                    of: SpansOf::new(verified?, lang),
                    target_line_start: plan.target_line_start,
                };
                let spans = engine.lru.spans(&key, class)?;
                let r = spans.range();
                (r.start <= plan.visible.start && plan.visible.end <= r.end).then_some(spans)
            };
            if let Some(spans) = cached {
                whole = Some(spans);
                whole_from = verified;
                highlight = Highlighting::On;
            } else if let Some(spans) = screen() {
                window = Some(spans);
                highlight = Highlighting::On;
            } else if let Some(w) = plan.window.clone() {
                // Compiling the query on first use is outside the budget.
                lang.prepare();
                match highlight::window(
                    lang,
                    &*pieces,
                    w,
                    &plan.json_pad,
                    plan.visible.clone(),
                    budget,
                ) {
                    Outcome::Done(spans) => {
                        window = Some(Arc::new(spans));
                        highlight = Highlighting::On;
                    }
                    Outcome::Unavailable => highlight = Highlighting::Plain,
                    Outcome::Yielded => yielded = true,
                    Outcome::TimedOut | Outcome::Cancelled => {}
                }
            }
        }
    }
    let oid = match target.origin {
        TextOrigin::Object => target.expect,
        TextOrigin::Worktree | TextOrigin::Dependency(_) => None,
    };
    First::Shown(Box::new(Shown {
        size,
        identity,
        rest,
        pieces,
        anchor,
        window,
        yielded,
        whole,
        whole_from,
        highlight,
        lang: lang.map(|l| l.static_name()),
        lines,
        oid,
        relocate,
    }))
}

/// What opening a file gives: the rest to read, its size and identity.
pub(crate) type Opening = Result<(Rest, u64, Option<FileIdentity>), DocState>;

/// A worktree file, opened beneath the root's `SafeRoot`.
pub(crate) fn worktree_file(
    engine: &EngineInner,
    target: &ReadTarget,
    root: &Arc<Path>,
) -> Opening {
    let rel =
        RelPath::new(target.path.as_bytes()).ok_or(DocState::Unreadable(ReadError::InvalidPath))?;
    let safe = engine.root(&target.project, root).map_err(io_error)?;
    open_in(&safe, rel)
}

/// A dependency file (ADR 0033): only beneath a dependency root of the
/// worktree, opened from that directory's own `SafeRoot` (no level of the
/// root or the path may be a symbolic link), and only with an extension or
/// a file name on the allowed list. A root in a cache outside the worktree
/// is not read.
pub(crate) fn dependency_file(
    engine: &EngineInner,
    target: &ReadTarget,
    root: &Arc<Path>,
    dep: &DepRoot,
) -> Opening {
    let DepRoot::Worktree { dir } = dep else {
        return Err(DocState::Unreadable(ReadError::NotAllowed));
    };
    let path: &str = &target.path;
    let inside = path
        .strip_prefix(&**dir)
        .and_then(|rest| rest.strip_prefix('/'))
        .filter(|rest| !rest.is_empty());
    let (Some(dir_rel), Some(inside)) = (RelPath::new(dir.as_bytes()), inside) else {
        return Err(DocState::Unreadable(ReadError::NotAllowed));
    };
    let Some(rel) = RelPath::new(inside.as_bytes()) else {
        return Err(DocState::Unreadable(ReadError::InvalidPath));
    };
    if !dependency_allowed(path) {
        return Err(DocState::Unreadable(ReadError::NotAllowed));
    }
    let safe = engine.root(&target.project, root).map_err(io_error)?;
    let dir_root = safe.open_dir(dir_rel).map_err(safe_error)?;
    open_in(&dir_root, rel)
}

/// ADR 0033's list: an entry starting with `.` is a file name's suffix,
/// any other a whole file name.
fn dependency_allowed(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    DEPENDENCY_EXTENSIONS.iter().any(|allowed| {
        if allowed.starts_with('.') {
            name.len() > allowed.len() && name.ends_with(allowed)
        } else {
            name == *allowed
        }
    })
}

fn open_in(safe: &SafeRoot, rel: RelPath<'_>) -> Opening {
    match safe.open_file(rel) {
        Ok(Opened::File(file)) => {
            let (size, identity) = (file.meta().size, file.identity());
            Ok((Rest::File(file), size, Some(identity)))
        }
        Ok(Opened::Symlink { target, .. }) => Err(DocState::Symlink {
            target: target.into(),
        }),
        Ok(Opened::Unsupported { .. }) => Err(DocState::Unsupported),
        Err(e) => Err(safe_error(e)),
    }
}

fn safe_error(error: SafeError) -> DocState {
    match error {
        SafeError::NotFound => DocState::Missing,
        SafeError::PathThroughSymlink => DocState::Unreadable(ReadError::PathThroughSymlink),
        SafeError::Io(e) => io_error(e),
    }
}

/// An object target's content: registered, in the LRU, or taken out of the
/// object database (ADR 0042). A text found in the LRU is used as `class`.
pub(crate) fn object(
    engine: &EngineInner,
    target: &ReadTarget,
    class: Class,
) -> Result<Rest, DocState> {
    let Some(oid) = target.expect else {
        return Err(DocState::Unreadable(ReadError::ObjectMissing));
    };
    if let Some(bytes) = engine.shares.get(&oid) {
        return Ok(Rest::Memory(bytes));
    }
    if let Some(text) = engine.lru.text(oid, class) {
        return Ok(Rest::Text(text));
    }
    let ProjectKey::Git { common_dir } = &target.project else {
        return Err(DocState::Unreadable(ReadError::ObjectMissing));
    };
    match engine.git.blob(common_dir, oid, OBJECT_MAX) {
        Ok(bytes) => Ok(Rest::Memory(bytes)),
        Err(GitError::TooLarge { .. }) => Err(DocState::Unreadable(ReadError::ObjectTooLarge)),
        Err(GitError::ObjectMissing(_)) => Err(DocState::Unreadable(ReadError::ObjectMissing)),
        Err(e) => Err(io_error(e)),
    }
}

/// The first screen's read: one `read_ranges`, or views into content in
/// memory.
fn read(rest: &Rest, ranges: &[Range<u64>]) -> std::io::Result<Vec<Piece>> {
    match rest {
        Rest::File(file) => {
            let read = file.read_ranges(ranges)?;
            Ok(ranges
                .iter()
                .zip(read.pieces)
                .map(|(r, bytes)| Piece::owned(r.start, bytes))
                .collect())
        }
        Rest::Memory(bytes) => Ok(view(bytes, ranges)),
        Rest::Text(text) => Ok(view(text.shared_bytes(), ranges)),
    }
}

fn view(bytes: &Arc<[u8]>, ranges: &[Range<u64>]) -> Vec<Piece> {
    ranges
        .iter()
        .map(|r| {
            let start = (r.start as usize).min(bytes.len());
            let end = (r.end as usize).min(bytes.len());
            Piece {
                start: start as u64,
                bytes: bytes.clone(),
                offset: start,
                len: end - start,
            }
        })
        .collect()
}

/// Drops pieces another piece holds.
fn normalize(mut pieces: Pieces) -> Pieces {
    pieces.pieces.sort_by_key(|p| p.start);
    let all = pieces.pieces.clone();
    pieces.pieces.retain(|p| {
        !all.iter().any(|q| {
            let (pr, qr) = (p.range(), q.range());
            qr.start <= pr.start && pr.end <= qr.end && (qr != pr) && q.len > p.len
        })
    });
    pieces
}

pub(crate) fn whole_key(oid: BlobId, lang: &crate::Lang) -> LruKey {
    SpansOf::new(oid, lang).spans(WHOLE)
}

/// What follows a first screen.
pub(crate) enum Next {
    Nothing,
    Complete(Rest),
}

/// Writes the first screen into the state.
pub(crate) fn fill_first(st: &mut DocMut, target: &ReadTarget, first: First) -> Next {
    match first {
        First::Failed(state) => {
            st.state = state;
            st.content = Content::Nothing;
            st.whole = None;
            st.windows.clear();
            st.file = None;
            st.pages = None;
            st.size = 0;
            st.tier = None;
            st.oid = None;
            st.highlight = Highlighting::Plain;
            if target.origin == TextOrigin::Worktree {
                st.editable = Editable::No(NotEditable::NotLoaded);
            }
            st.set_anchor(RowPos::default());
            Next::Nothing
        }
        First::Shown(shown) => {
            let shown = *shown;
            st.state = DocState::Ready;
            st.size = shown.size;
            st.identity = shown.identity;
            st.lang = shown.lang;
            st.tier = Some(if shown.size <= TIER_A_MAX {
                Tier::A
            } else if shown.size <= TIER_B_MAX {
                Tier::B
            } else {
                Tier::C
            });
            st.oid = shown.oid;
            st.highlight = shown.highlight;
            st.lines = LineCount::Estimated(shown.lines);
            st.widest = 0;
            if target.origin == TextOrigin::Worktree {
                st.editable = Editable::No(NotEditable::NotLoaded);
            }
            st.whole = shown.whole;
            st.whole_from = shown.whole_from;
            st.windows = shown.window.into_iter().collect();
            st.relocate = shown.relocate;
            st.set_anchor(shown.anchor);
            if st.tier == Some(Tier::C) {
                let cache = PageCache::new(shown.size);
                st.content = Content::Paged(Arc::new(Paged {
                    pages: cache.pages().clone(),
                    pieces: shown.pieces,
                    index: None,
                }));
                st.pages = Some(cache);
                // Kept for the pages, the scan and finds.
                if let Rest::File(file) = shown.rest {
                    st.file = Some(Arc::new(file));
                }
                Next::Nothing
            } else {
                st.content = Content::Pieces(shown.pieces);
                st.file = None;
                st.pages = None;
                Next::Complete(shown.rest)
            }
        }
    }
}

fn apply_first(engine: &Arc<EngineInner>, doc: &Arc<Doc>, load: u64, first: First) {
    let mut st = doc.lock();
    if doc.stale(&st, load) {
        return;
    }
    let next = fill_first(&mut st, &doc.target, first);
    let tier_c = st.tier == Some(Tier::C);
    let file = st.file.clone();
    let find = st.find.waiting;
    doc.commit(st);
    if find {
        run_find(engine, doc);
    }
    #[cfg(test)]
    if let Some(hook) = crate::util::lock(&engine.after_first).take() {
        hook(&doc.current());
    }
    watch::ensure(engine, doc);
    match next {
        Next::Nothing => {}
        Next::Complete(rest) => {
            let (engine2, doc2) = (engine.clone(), doc.clone());
            let _ = engine.spawn(Priority::Background, async move {
                complete(&engine2, &doc2, load, rest);
            });
        }
    }
    if tier_c {
        if let Some(file) = file {
            let (engine2, doc2) = (engine.clone(), doc.clone());
            let _ = engine.spawn(Priority::Background, async move {
                scan(&engine2, &doc2, load, &file);
            });
        }
        visible_work(engine, doc);
    }
}

/// The full read of content of at most 16 MiB.
fn complete(engine: &Arc<EngineInner>, doc: &Arc<Doc>, load: u64, rest: Rest) {
    if doc.stale(&doc.lock(), load) {
        return;
    }
    let target = &doc.target;
    let (text, checkpoints, oid, identity) = match rest {
        Rest::File(file) => {
            let (bytes, identity) = match file.read_all(TIER_B_MAX) {
                Ok(FullRead::File { bytes, meta }) => (bytes, meta.identity()),
                Ok(FullRead::TooLarge { .. }) => {
                    // It grew past 16 MiB: open it again as tier C.
                    drop(file);
                    let at = doc.lock().at;
                    reload(engine, doc, at);
                    return;
                }
                Err(e) => {
                    let mut st = doc.lock();
                    if !doc.stale(&st, load) {
                        st.state = io_error(e);
                        st.content = Content::Nothing;
                        doc.commit(st);
                    }
                    return;
                }
            };
            drop(file);
            let kind = engine.hash_kind(target);
            let oid = blob_id(&Oid::for_blob(kind, &bytes));
            let (text, checkpoints) = Text::scanned(Arc::from(bytes));
            engine.remember(&doc.root, &target.path, identity, oid);
            (text, checkpoints, oid, Some(identity))
        }
        Rest::Memory(bytes) => {
            let (text, checkpoints) = Text::scanned(bytes);
            let oid = target.expect.unwrap_or(BlobId::Sha1([0; 20]));
            (text, checkpoints, oid, None)
        }
        Rest::Text(text) => {
            let oid = target.expect.unwrap_or(BlobId::Sha1([0; 20]));
            return apply_full(engine, doc, load, text, oid, None);
        }
    };
    if text.len() >= CHECKPOINT_MIN {
        engine.store_checkpoints(&target.project, &oid, &checkpoints);
    }
    apply_full(engine, doc, load, Arc::new(text), oid, identity);
}

fn editable(origin: &TextOrigin, len: u64, index: &LineIndex) -> Editable {
    let facts = index.facts();
    Editable::No(if *origin != TextOrigin::Worktree {
        NotEditable::ReadOnlySource
    } else if len > TIER_A_MAX {
        NotEditable::TooLarge
    } else if index.has_long_line() {
        NotEditable::LongLine
    } else if !facts.is_text() {
        NotEditable::NotText
    } else if facts.mixed_line_endings() {
        NotEditable::MixedLineEndings
    } else {
        return Editable::Yes;
    })
}

/// Where the target is in content whose line index exists: by its byte,
/// or by its line when `by_line` (the content is not the one the target's
/// byte was taken from).
fn target_pos(st: &DocMut, target: &ReadTarget, by_line: bool) -> RowPos {
    let snapshot = st.snapshot(target);
    let cols = st.viewport.cols.max(1);
    let line_pos = |line: u32, column: u32| {
        let Some(src) = snapshot.content.source() else {
            return RowPos::default();
        };
        let Some(index) = snapshot.content.index() else {
            return RowPos::default();
        };
        let start = index.line_start_in(src, u64::from(line));
        let next = index.line_start_in(src, u64::from(line) + 1);
        match start {
            Some(start) => {
                let end = next.map_or(st.size, |n| n.saturating_sub(1).max(start));
                snapshot.pos_of_byte((start + u64::from(column)).min(end), cols)
            }
            // The bytes are not in memory: the line start the index holds
            // before it.
            None => RowPos {
                line_start: index.block_start(u64::from(line)),
                sub_row: 0,
            },
        }
    };
    match st.at {
        Anchor::Start => RowPos::default(),
        Anchor::Byte { byte, .. } if !by_line => snapshot.pos_of_byte(byte, cols),
        Anchor::Byte { line, column, .. } | Anchor::Line { line, column } => line_pos(line, column),
    }
}

fn apply_full(
    engine: &Arc<EngineInner>,
    doc: &Arc<Doc>,
    load: u64,
    text: Arc<Text>,
    oid: BlobId,
    identity: Option<FileIdentity>,
) {
    let target = &doc.target;
    let mut st = doc.lock();
    if doc.stale(&st, load) {
        return;
    }
    let changed = identity.is_some() && st.identity.is_some() && identity != st.identity;
    if identity.is_some() {
        st.identity = identity;
    }
    let index = text.index();
    let len = text.len();
    st.size = len;
    st.oid = Some(oid);
    st.lines = LineCount::Exact(index.line_count());
    st.widest = index.widest_cells();
    st.tier = Some(if len <= TIER_A_MAX && !index.has_long_line() {
        Tier::A
    } else {
        Tier::B
    });
    st.editable = editable(&target.origin, len, index);
    st.content = Content::Text(text.clone());
    if changed {
        st.whole = None;
        st.windows.clear();
    }
    // Whole-file spans belong to the OID they were found by: not this
    // content's, they go (and the parse below makes this content's).
    if st.whole.is_some() && st.whole_from != Some(oid) {
        st.whole = None;
        if st.windows.is_empty() {
            st.highlight = Highlighting::Pending;
        }
    }
    st.whole_from = None;
    let by_line = changed || st.relocate || moved_by_line(&st, Some(oid));
    st.relocate = false;
    let anchor = target_pos(&st, target, by_line);
    st.set_anchor(anchor);
    let langs = engine.languages.clone();
    let lang = st.lang.and_then(|name| langs.get(name));
    if lang.is_none() || !index.facts().is_text() {
        st.highlight = Highlighting::Plain;
        st.whole = None;
        st.windows.clear();
    }
    if len <= TIER_A_MAX {
        engine.lru.insert(
            LruKey::Text(oid),
            LruValue::Text(text.clone()),
            Class::Opened,
        );
    }
    let tier_a = st.tier == Some(Tier::A);
    let parse = tier_a && st.highlight != Highlighting::Plain && st.whole.is_none();
    let find = st.find.waiting;
    doc.commit(st);
    if parse && let Some(lang) = lang {
        if let Some(spans) = engine.lru.spans(&whole_key(oid, lang), Class::Opened) {
            set_whole(doc, load, spans);
        } else {
            let (engine2, doc2, name) = (engine.clone(), doc.clone(), lang.static_name());
            let _ = engine.spawn(Priority::Interactive, async move {
                whole_parse(&engine2, &doc2, load, text, oid, name).await;
            });
        }
    } else if !tier_a {
        chain::begin(engine, doc, load);
        visible_work(engine, doc);
    }
    if find {
        run_find(engine, doc);
    }
}

/// A byte anchor whose OID is not the content's: its byte means nothing
/// here, its line does (after the full read and after the scan alike).
fn moved_by_line(st: &DocMut, oid: Option<BlobId>) -> bool {
    matches!(st.at, Anchor::Byte { .. }) && st.expect.is_some() && oid.is_some() && st.expect != oid
}

fn set_whole(doc: &Doc, load: u64, spans: Arc<WindowSpans>) {
    let mut st = doc.lock();
    if doc.stale(&st, load) {
        return;
    }
    st.whole = Some(spans);
    st.windows.clear();
    st.highlight = Highlighting::On;
    doc.commit(st);
}

/// Tier A's whole-file parse, in 8 ms slices with a turn for other tasks
/// between them.
async fn whole_parse(
    engine: &Arc<EngineInner>,
    doc: &Arc<Doc>,
    load: u64,
    text: Arc<Text>,
    oid: BlobId,
    lang: &'static str,
) {
    let langs = engine.languages.clone();
    let Some(lang) = langs.get(lang) else {
        return;
    };
    let cancel = doc.lock().cancel.clone();
    let src: Arc<dyn Source> = text;
    let mut parse = FullParse::new(lang, src);
    loop {
        if doc.stale(&doc.lock(), load) {
            return;
        }
        let budget = Budget {
            budget: SLICE,
            cancel: &cancel,
            yield_probe: None,
        };
        match parse.step(budget) {
            Outcome::Done(spans) => {
                let spans = Arc::new(spans);
                engine.lru.insert(
                    whole_key(oid, lang),
                    LruValue::Spans(spans.clone()),
                    Class::Opened,
                );
                set_whole(doc, load, spans);
                return;
            }
            Outcome::TimedOut | Outcome::Yielded => yield_now().await,
            Outcome::Cancelled => return,
            Outcome::Unavailable => {
                let mut st = doc.lock();
                if !doc.stale(&st, load) {
                    st.highlight = Highlighting::Plain;
                    st.windows.clear();
                    doc.commit(st);
                }
                return;
            }
        }
    }
}

/// Tier C's streaming scan: 1 MiB reads on the same `OpenFile`, the blob
/// hash and the line index in one pass; cancelled between reads.
fn scan(engine: &Arc<EngineInner>, doc: &Arc<Doc>, load: u64, file: &OpenFile) {
    let target = &doc.target;
    let (cancel, identity) = {
        let st = doc.lock();
        (st.cancel.clone(), st.identity)
    };
    let size = file.meta().size;
    let kind = engine.hash_kind(target);
    let mut hasher = BlobHasher::new(kind, size);
    let mut builder = IndexBuilder::new();
    let mut at = 0;
    while at < size {
        if cancel.load(Ordering::SeqCst) {
            return;
        }
        let end = (at + SCAN_BLOCK).min(size);
        let read = match file.read_ranges(std::slice::from_ref(&(at..end))) {
            Ok(read) => read,
            Err(e) => {
                log(format_args!("scan of {}: {e}", target.path));
                return;
            }
        };
        let piece = read.pieces.into_iter().next().unwrap_or_default();
        if Some(read.meta.identity()) != identity || piece.len() as u64 != end - at {
            // Changed while it was read: open it again.
            let at = doc.current_line();
            reload(engine, doc, at);
            return;
        }
        hasher.update(&piece);
        builder.feed(&piece);
        at = end;
    }
    let scanned = builder.finish();
    let oid = hasher.finish().map(|o| blob_id(&o));
    if let (Some(oid), Some(identity)) = (oid, identity) {
        engine.store_checkpoints(&target.project, &oid, &scanned.checkpoints());
        engine.remember(&doc.root, &target.path, identity, oid);
    }
    let index = Arc::new(scanned.index);
    // A line to move to: its index entry's pages are read now, so the
    // line's start is exact (the first screen read other bytes).
    let at_line = {
        let st = doc.lock();
        if doc.stale(&st, load) {
            return;
        }
        let by_line = st.relocate || moved_by_line(&st, oid);
        match st.at {
            Anchor::Line { line, .. } => Some(line),
            Anchor::Byte { line, .. } if by_line => Some(line),
            _ => None,
        }
    };
    let pages = at_line.and_then(|line| {
        let start = index.block_start(u64::from(line)) / PAGE * PAGE;
        let end = (start + SCAN_BLOCK).min(size);
        let read = file.read_ranges(std::slice::from_ref(&(start..end))).ok()?;
        (Some(read.meta.identity()) == identity)
            .then(|| split_pages(start, read.pieces.into_iter().next().unwrap_or_default()))
    });
    let mut st = doc.lock();
    if doc.stale(&st, load) {
        return;
    }
    if let (Some(got), Some(cache)) = (pages, st.pages.as_mut()) {
        cache.add(got);
        let pages = cache.pages().clone();
        if let Content::Paged(paged) = &st.content {
            st.content = Content::Paged(Arc::new(Paged {
                pages,
                pieces: paged.pieces.clone(),
                index: paged.index.clone(),
            }));
        }
    }
    st.oid = oid;
    st.lines = LineCount::Exact(index.line_count());
    st.widest = index.widest_cells();
    st.editable = Editable::No(NotEditable::TooLarge);
    if !index.facts().is_text() {
        st.highlight = Highlighting::Plain;
        st.windows.clear();
    }
    if let Content::Paged(paged) = &st.content {
        st.content = Content::Paged(Arc::new(Paged {
            pages: paged.pages.clone(),
            pieces: paged.pieces.clone(),
            index: Some(index),
        }));
    }
    let by_line = st.relocate || moved_by_line(&st, oid);
    st.relocate = false;
    let anchor = target_pos(&st, target, by_line);
    st.set_anchor(anchor);
    doc.commit(st);
    chain::begin(engine, doc, load);
    visible_work(engine, doc);
}

/// The bytes the viewport shows from the top: the display row's own bytes,
/// not its line's start (a row deep in a long line is megabytes on).
pub(crate) fn visible(st: &DocMut, target: &ReadTarget) -> Range<u64> {
    let top = st.top.unwrap_or(st.anchor);
    st.snapshot(target)
        .visible_bytes(top, st.viewport.rows, st.viewport.cols)
}

/// The pages tier C reads for the visible bytes.
fn page_range(visible: &Range<u64>) -> Range<u64> {
    visible.start.saturating_sub(PAGES_BEFORE)..visible.end.max(visible.start + PAGES_AFTER)
}

/// After scrolling (or new content): tier C reads the pages around the
/// top, tiers B and C highlight the window there.
pub(crate) fn visible_work(engine: &Arc<EngineInner>, doc: &Arc<Doc>) {
    let mut st = doc.lock();
    if doc.is_closed() || st.state != DocState::Ready {
        return;
    }
    let load = st.load;
    let shown = visible(&st, &doc.target);
    let mut pages = false;
    if let Some(cache) = &st.pages
        && !cache.missing(page_range(&shown)).is_empty()
        && !st.pages_busy
    {
        st.pages_busy = true;
        pages = true;
    }
    let window = needs_window(&st, &shown);
    let start_window = window && !st.window_busy && !st.pages_busy;
    if window && st.window_busy {
        st.window_again = true;
    }
    if start_window {
        st.window_busy = true;
    }
    drop(st);
    if pages {
        let (engine2, doc2) = (engine.clone(), doc.clone());
        let _ = engine.spawn(Priority::Interactive, async move {
            read_pages(&engine2, &doc2, load);
        });
    }
    if start_window {
        let (engine2, doc2) = (engine.clone(), doc.clone());
        let spawned = engine.spawn(Priority::Interactive, async move {
            visible_window(&engine2, &doc2, load).await;
        });
        if !spawned {
            doc.lock().window_busy = false;
        }
    }
}

/// Whether the visible bytes lack a window: one of the chain's (the
/// visible ones and one on each side) where the chain reaches them, else
/// any window covering them.
fn needs_window(st: &DocMut, shown: &Range<u64>) -> bool {
    if !matches!(st.tier, Some(Tier::B | Tier::C))
        || st.lang.is_none()
        || st.highlight == Highlighting::Plain
        || st.whole.is_some()
    {
        return false;
    }
    chain::missing(st, &chain::want(st.chain.as_ref(), shown), shown)
}

/// Tier C: the pages around the top, read in one `read_ranges`.
fn read_pages(engine: &Arc<EngineInner>, doc: &Arc<Doc>, load: u64) {
    loop {
        let (file, wanted, identity, range) = {
            let mut st = doc.lock();
            if doc.stale(&st, load) {
                return;
            }
            let range = page_range(&visible(&st, &doc.target));
            let wanted = st
                .pages
                .as_ref()
                .map(|c| c.missing(range.clone()))
                .unwrap_or_default();
            if wanted.is_empty() || st.file.is_none() {
                st.pages_busy = false;
                drop(st);
                visible_work(engine, doc);
                return;
            }
            (st.file.clone(), wanted, st.identity, range)
        };
        let Some(file) = file else { return };
        // Consecutive pages make one range; at most 1 MiB in all.
        let mut ranges: Vec<Range<u64>> = Vec::new();
        for &page in wanted.iter().take(16) {
            let (start, end) = (page * PAGE, (page + 1) * PAGE);
            if let Some(last) = ranges.last_mut().filter(|l| l.end == start) {
                last.end = end;
            } else if ranges.len() == lectern_platform::MAX_RANGES {
                break;
            } else {
                ranges.push(start..end);
            }
        }
        let read = match file.read_ranges(&ranges) {
            Ok(read) => read,
            Err(e) => {
                log(format_args!("pages of {}: {e}", doc.target.path));
                doc.lock().pages_busy = false;
                return;
            }
        };
        if Some(read.meta.identity()) != identity {
            doc.lock().pages_busy = false;
            let at = doc.current_line();
            reload(engine, doc, at);
            return;
        }
        let mut got = Vec::new();
        for (range, bytes) in ranges.iter().zip(read.pieces) {
            got.extend(split_pages(range.start, bytes));
        }
        let mut st = doc.lock();
        if doc.stale(&st, load) {
            return;
        }
        if let Some(cache) = st.pages.as_mut() {
            cache.add(got);
            cache.touch(range);
            let pages = cache.pages().clone();
            if let Content::Paged(paged) = &st.content {
                st.content = Content::Paged(Arc::new(Paged {
                    pages,
                    pieces: paged.pieces.clone(),
                    index: paged.index.clone(),
                }));
            }
        }
        doc.commit(st);
    }
}

/// Bytes read from `start` (a page boundary) as pages.
fn split_pages(start: u64, bytes: Vec<u8>) -> Vec<(u64, Arc<[u8]>)> {
    bytes
        .chunks(PAGE as usize)
        .enumerate()
        .map(|(i, page)| (start / PAGE + i as u64, Arc::from(page)))
        .collect()
}

/// The most windows one round of [`visible_window`] computes: the visible
/// ones and one on each side, and the frontier's few on the way to them.
const ROUND_MAX: usize = 12;

/// Tiers B and C: the windows the visible bytes want (`chain::want`): the
/// chain's from their starts, taken from the LRU or computed here, where
/// the chain reaches the top; else the window at the top parsed alone like
/// the first screen's.
async fn visible_window(engine: &Arc<EngineInner>, doc: &Arc<Doc>, load: u64) {
    let langs = engine.languages.clone();
    let mut computed = 0;
    loop {
        let (want, shown, content, file, identity, size, rows, lang, cancel, of) = {
            let st = doc.lock();
            if doc.stale(&st, load) {
                return;
            }
            let shown = visible(&st, &doc.target);
            let want = chain::want(st.chain.as_ref(), &shown);
            // The first window of the chain the document lacks.
            let want = match want {
                Want::Windows(starts) => {
                    starts.into_iter().find(|s| !chain::has(&st, *s)).map(Some)
                }
                Want::Heuristic => chain::missing(&st, &Want::Heuristic, &shown).then_some(None),
            };
            (
                want,
                shown,
                st.content.clone(),
                st.file.clone(),
                st.identity,
                st.size,
                st.viewport.rows,
                st.lang.and_then(|name| langs.get(name)),
                st.cancel.clone(),
                st.chain.as_ref().map(|c| c.of),
            )
        };
        let more = match (want, lang) {
            (Some(Some(start)), Some(lang)) if computed < ROUND_MAX => {
                computed += 1;
                let content = (content, file, identity, size);
                chain_window(engine, doc, load, start, content, lang, &cancel, of).await
            }
            (Some(None), Some(lang)) if computed == 0 => {
                computed += 1;
                let spans = window_at(lang, &content, shown.start, size, rows, &cancel);
                let mut st = doc.lock();
                if doc.stale(&st, load) {
                    return;
                }
                if let Some(spans) = spans {
                    chain::add(&mut st, Arc::new(spans));
                    st.highlight = Highlighting::On;
                }
                doc.commit(st);
                false
            }
            _ => false,
        };
        if more {
            continue;
        }
        let mut st = doc.lock();
        if doc.stale(&st, load) {
            return;
        }
        if st.window_again {
            st.window_again = false;
            computed = 0;
            drop(st);
            continue;
        }
        st.window_busy = false;
        return;
    }
}

/// The chain's window from `start` for the visible bytes: from the LRU, or
/// computed. Whether to go on with the next one.
#[expect(
    clippy::too_many_arguments,
    reason = "the window, the document's content and its load"
)]
async fn chain_window(
    engine: &Arc<EngineInner>,
    doc: &Arc<Doc>,
    load: u64,
    start: u64,
    (content, file, identity, size): (Content, Option<Arc<OpenFile>>, Option<FileIdentity>, u64),
    lang: &crate::Lang,
    cancel: &AtomicBool,
    of: Option<SpansOf>,
) -> bool {
    let Some(of) = of else {
        return false;
    };
    if let Some(spans) = engine.lru.spans(&of.spans(start), Class::Opened) {
        let mut st = doc.lock();
        if doc.stale(&st, load) {
            return false;
        }
        // A window's spans end where the next window starts.
        let next = spans.range().end;
        let moved = st
            .chain
            .as_mut()
            .is_some_and(|c| c.advance(start, next, size));
        chain::add(&mut st, spans);
        st.highlight = Highlighting::On;
        doc.commit(st);
        if moved {
            chain::kick(engine);
        }
        return true;
    }
    let Some(src) = chain::source(&content, file.as_ref(), identity, start, size) else {
        return false;
    };
    drop(content);
    match chain::compute(lang, src, start, cancel).await {
        Computed::Done(spans, next, spent) => {
            chain::land(engine, doc, load, start, spans, next, spent, true);
            true
        }
        Computed::Unavailable => {
            chain::plain(doc, load);
            false
        }
        Computed::Stopped => false,
    }
}

fn window_at(
    lang: &crate::Lang,
    content: &Content,
    top: u64,
    size: u64,
    rows: u32,
    cancel: &AtomicBool,
) -> Option<WindowSpans> {
    let src = content.source()?;
    let head = slice(src, 0..HEAD.min(size))?;
    let around_start = top.saturating_sub(AROUND_BEFORE);
    let around = slice(src, around_start..(top + AROUND_AFTER).min(size))?;
    let plan = window::plan(
        lang.rules(),
        &head,
        &around,
        around_start,
        size,
        Target::Byte(top),
        // Two more screens below, so scrolling a little needs no new window.
        rows.saturating_mul(3),
    );
    let w = plan.window.clone()?;
    if plan.encoding == Encoding::NotText {
        return None;
    }
    lang.prepare();
    let budget = Budget {
        budget: SCROLL_WINDOW_BUDGET,
        cancel,
        yield_probe: None,
    };
    highlight::window(lang, src, w, &plan.json_pad, plan.visible, budget).done()
}

/// A new query (or none: clear): cancels the last one.
pub(crate) fn find(engine: &Arc<EngineInner>, doc: &Arc<Doc>, query: Option<FindQuery>) {
    let mut st = doc.lock();
    st.find.cancel.store(true, Ordering::SeqCst);
    st.find.cancel = Arc::new(AtomicBool::new(false));
    st.find.generation += 1;
    st.find.query = query;
    st.find.results = None;
    st.find.waiting = st.find.query.is_some();
    doc.commit(st);
    run_find(engine, doc);
}

fn run_find(engine: &Arc<EngineInner>, doc: &Arc<Doc>) {
    let generation = {
        let st = doc.lock();
        let ready = st.find.query.is_some()
            && match &st.content {
                Content::Text(_) => true,
                Content::Paged(_) => st.file.is_some(),
                _ => false,
            };
        if !ready {
            return;
        }
        st.find.generation
    };
    let doc2 = doc.clone();
    let _ = engine.spawn(Priority::Interactive, async move {
        find_task(&doc2, generation);
    });
}

fn find_task(doc: &Doc, generation: u64) {
    let (cancel, query, content, file, size) = {
        let mut st = doc.lock();
        if st.find.generation != generation {
            return;
        }
        let Some(query) = st.find.query.clone() else {
            return;
        };
        st.find.waiting = false;
        (
            st.find.cancel.clone(),
            query,
            st.content.clone(),
            st.file.clone(),
            st.size,
        )
    };
    let publish = |results: FindResults| {
        let mut st = doc.lock();
        if st.find.generation != generation || doc.is_closed() {
            return false;
        }
        st.find.results = Some(Arc::new(results));
        doc.commit(st);
        true
    };
    let stopped = || cancel.load(Ordering::SeqCst);
    let results = match (&content, file) {
        (Content::Text(text), _) => find_in(text.as_bytes(), &query.text, |partial| {
            stopped() || !publish(partial.clone())
        }),
        (Content::Paged(_), Some(file)) => find_file(&file, size, &query.text, |partial| {
            stopped() || !publish(partial.clone())
        }),
        _ => return,
    };
    if let Some(results) = results {
        publish(results);
    }
}

/// Find over a file read in 1 MiB blocks (tier C).
fn find_file(
    file: &OpenFile,
    size: u64,
    query: &str,
    mut stop: impl FnMut(&FindResults) -> bool,
) -> Option<FindResults> {
    let Some(mut finder) = Finder::new(query) else {
        return Some(FindResults {
            done: true,
            ..FindResults::default()
        });
    };
    let mut at = 0u64;
    while at < size {
        let end = (at + BLOCK as u64).min(size);
        let read = file.read_ranges(std::slice::from_ref(&(at..end))).ok()?;
        let block = read.pieces.into_iter().next().unwrap_or_default();
        let limit = if end >= size {
            size
        } else {
            end - finder.overlap() as u64
        };
        if finder.feed(&block, at, limit) {
            break;
        }
        if limit <= at {
            break;
        }
        at = limit;
        if at < size && stop(&finder.results(false)) {
            return None;
        }
    }
    Some(finder.results(true))
}
