//! The window chain of tiers B and C (docs/design/file_reader.md「档位与降级」
//! 「批量线程上的先后」): windows cut by their own tree
//! ([`ChainWindow`]), computed in order from the file's start once the
//! content's OID is known, their starts kept per document and in the LRU
//! (`LruKey::Starts`, resumed where it broke off).
//!
//! - The visible windows and one on each side are windows of the chain
//!   wherever its starts reach the visible bytes, computed in the
//!   document's `Interactive` tasks (`load::visible_window`); before the
//!   chain gets there they are parsed alone from a heuristic safe start, as
//!   on the first screen ([`Want::Heuristic`]).
//! - The rest of the chain is batch work (ADR 0033's memory-only kind): one
//!   `Batch` task at a time across the engine, holding a yielding
//!   `BackgroundGuard`; a turn does at most one window's step of
//!   [`BATCH_SEGMENT`], then the task goes to the end of the batch queue,
//!   behind the batch work already queued. A window whose step ran out, or
//!   stopped for the yield switch or the pause, waits in [`Sched::parked`]
//!   for the next turn, which goes on from where it stopped. A batch window's spans are kept
//!   (in the LRU, as background work) only when it is next to the visible
//!   bytes; its start goes into the table either way.
//! - Every window computed is recorded in the document's [`SlowWindows`]:
//!   two slow ones in a row turn its highlighting off.

use std::ops::Range;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Weak};

use lectern_kernel::BackgroundGuard;
use lectern_platform::{FileIdentity, OpenFile};

use crate::Lang;
use crate::highlight::{Budget, ChainWindow, Outcome, SlowWindows, Source, Spent};
use crate::limits::{BATCH_SEGMENT, LONG_LINE, SLICE, WINDOW, WINDOW_MAX};
use crate::lru::{Class, LruKey, LruValue, SpansOf, Starts};
use crate::spans::WindowSpans;
use crate::util::{lock, yield_now};

use super::content::Content;
use super::engine::{Doc, DocMut, EngineInner, trace};
#[cfg(any(test, feature = "test-support"))]
use super::engine::{StepEnd, Trace};
use super::{DocState, Highlighting, Tier, load};

/// The windows a document keeps (the whole file's spans aside).
pub(crate) const WINDOWS_MAX: usize = 8;

/// Tier C reads a chain window's bytes from its file: the widest window and
/// this much after it, where the cut may look for an identifier's end.
const SLAB_MARGIN: u64 = LONG_LINE;

/// A document's window chain.
pub(crate) struct ChainState {
    pub of: SpansOf,
    /// The starts found, ascending: the first is 0, the last the frontier
    /// (whose window's end is not known until `done`).
    starts: Vec<u64>,
    /// The frontier's window ends at the file's end.
    pub done: bool,
    pub slow: SlowWindows,
}

impl ChainState {
    /// A chain of `of`, from the start table found in the LRU or from the
    /// file's start.
    pub(crate) fn new(of: SpansOf, table: Option<&Starts>) -> ChainState {
        let (starts, done) = match table {
            Some(t) if t.starts.first() == Some(&0) => (t.starts.to_vec(), t.done),
            _ => (vec![0], false),
        };
        ChainState {
            of,
            starts,
            done,
            slow: SlowWindows::default(),
        }
    }

    pub(crate) fn frontier(&self) -> u64 {
        self.starts.last().copied().unwrap_or(0)
    }

    #[cfg(test)]
    pub(crate) fn starts(&self) -> &[u64] {
        &self.starts
    }

    /// The window from `start` ends at `next`: when `start` is the frontier
    /// the chain goes on (or is done at the file's end). Whether it moved.
    pub(crate) fn advance(&mut self, start: u64, next: u64, size: u64) -> bool {
        if self.done || self.frontier() != start {
            return false;
        }
        if next >= size || next <= start {
            self.done = true;
        } else {
            self.starts.push(next);
        }
        true
    }

    fn table(&self) -> Starts {
        Starts {
            starts: self.starts.clone().into(),
            done: self.done,
        }
    }
}

/// What the visible bytes need.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Want {
    /// The chain's windows from these starts: those meeting the visible
    /// bytes and one on each side (the frontier's when it is next).
    Windows(Vec<u64>),
    /// The chain has not got there: a window parsed alone from a heuristic
    /// safe start, as on the first screen.
    Heuristic,
}

pub(crate) fn want(chain: Option<&ChainState>, shown: &Range<u64>) -> Want {
    let Some(chain) = chain else {
        return Want::Heuristic;
    };
    let starts = &chain.starts;
    let last = starts.len() - 1;
    let first = starts
        .partition_point(|&s| s <= shown.start)
        .saturating_sub(1);
    // Past the frontier by more than its window can reach: not there yet.
    if first == last && !chain.done && shown.start - starts[last] >= WINDOW_MAX {
        return Want::Heuristic;
    }
    let end = shown.end.max(shown.start + 1);
    let to = starts
        .partition_point(|&s| s < end)
        .saturating_sub(1)
        .max(first);
    let from = first.saturating_sub(1);
    let to = (to + 1).min(last).min(from + WINDOWS_MAX - 2);
    Want::Windows(starts[from..=to].to_vec())
}

/// Whether the document lacks a window `want` asks for.
pub(crate) fn missing(st: &DocMut, want: &Want, shown: &Range<u64>) -> bool {
    match want {
        Want::Heuristic => !st
            .windows
            .iter()
            .any(|w| w.range().start <= shown.start && w.range().end >= shown.end),
        Want::Windows(starts) => starts.iter().any(|s| !has(st, *s)),
    }
}

/// Whether the document holds the chain's window from `start`.
pub(crate) fn has(st: &DocMut, start: u64) -> bool {
    st.windows
        .iter()
        .any(|w| w.exact() && w.range().start == start)
}

/// Adds a window to the document's: the newest first, at most
/// [`WINDOWS_MAX`]; a window of the chain replaces the windows parsed alone
/// that it meets.
pub(crate) fn add(st: &mut DocMut, spans: Arc<WindowSpans>) {
    let range = spans.range();
    let exact = spans.exact();
    st.windows.retain(|w| {
        let r = w.range();
        let meets = r.start < range.end && range.start < r.end;
        !(r == range || (exact && !w.exact() && meets))
    });
    st.windows.insert(0, spans);
    st.windows.truncate(WINDOWS_MAX);
}

/// The content's OID is known (tier B's full read, tier C's scan): the
/// chain starts, from the start table in the LRU when there is one, and the
/// batch work goes on with it.
pub(crate) fn begin(engine: &Arc<EngineInner>, doc: &Arc<Doc>, load: u64) {
    let more = {
        let mut st = doc.lock();
        if doc.stale(&st, load) || !wants_chain(&st) {
            return;
        }
        let (Some(oid), Some(name)) = (st.oid, st.lang) else {
            return;
        };
        let langs = engine.languages.clone();
        let Some(lang) = langs.get(name) else {
            return;
        };
        let of = SpansOf::new(oid, lang);
        let table = engine.lru.starts(of, Class::Opened);
        let chain = ChainState::new(of, table.as_deref());
        let more = !chain.done;
        st.chain = Some(chain);
        more
    };
    if more {
        kick(engine);
    }
}

/// A document whose highlighting comes from the chain.
fn wants_chain(st: &DocMut) -> bool {
    st.state == DocState::Ready
        && matches!(st.tier, Some(Tier::B | Tier::C))
        && st.lang.is_some()
        && st.highlight != Highlighting::Plain
        && st.whole.is_none()
}

/// The bytes a window from `start` reads: tier B's text, or tier C's file
/// read now (blocking IO; executor threads only), None when the file
/// changed.
pub(crate) fn source(
    content: &Content,
    file: Option<&Arc<OpenFile>>,
    identity: Option<FileIdentity>,
    start: u64,
    size: u64,
) -> Option<Arc<dyn Source>> {
    match content {
        Content::Text(text) => {
            let src: Arc<dyn Source> = text.clone();
            Some(src)
        }
        Content::Paged(_) => {
            let file = file?;
            let end = (start + WINDOW_MAX + SLAB_MARGIN).min(size);
            let read = file.read_ranges(std::slice::from_ref(&(start..end))).ok()?;
            if Some(read.meta.identity()) != identity {
                return None;
            }
            let bytes = read.pieces.into_iter().next().unwrap_or_default();
            Some(Arc::new(Slab {
                base: start,
                bytes,
                len: size,
            }))
        }
        Content::Nothing | Content::Pieces(_) => None,
    }
}

/// Bytes of a file read from `base`, for one window of tier C.
struct Slab {
    base: u64,
    bytes: Vec<u8>,
    len: u64,
}

impl Source for Slab {
    fn len(&self) -> u64 {
        self.len
    }

    fn chunk(&self, at: u64) -> Option<&[u8]> {
        let at = usize::try_from(at.checked_sub(self.base)?).ok()?;
        self.bytes.get(at..).filter(|c| !c.is_empty())
    }
}

/// How a window computed for the visible bytes ended.
pub(crate) enum Computed {
    Done(WindowSpans, u64, Spent),
    Unavailable,
    Stopped,
}

/// A window of the chain from `start`, for the visible bytes
/// (`Interactive`): slices of [`SLICE`] with a turn for other tasks
/// between them.
pub(crate) async fn compute(
    lang: &Lang,
    src: Arc<dyn Source>,
    start: u64,
    cancel: &AtomicBool,
) -> Computed {
    let mut window = ChainWindow::new(lang, src, start);
    loop {
        let budget = Budget {
            budget: SLICE,
            cancel,
            yield_probe: None,
        };
        match window.step(budget) {
            Outcome::Done((spans, next)) => return Computed::Done(spans, next, window.spent()),
            Outcome::TimedOut | Outcome::Yielded => yield_now().await,
            Outcome::Cancelled => return Computed::Stopped,
            Outcome::Unavailable => return Computed::Unavailable,
        }
    }
}

/// A window of the chain from `start` landed: the chain goes on, the
/// window's parse is recorded for [`SlowWindows`], and its spans are kept
/// when the visible bytes want them (the document's own windows and the LRU
/// for `interactive` work, only the LRU for batch work).
#[expect(
    clippy::too_many_arguments,
    reason = "one window's result, its document and its load"
)]
pub(crate) fn land(
    engine: &Arc<EngineInner>,
    doc: &Arc<Doc>,
    load: u64,
    start: u64,
    spans: WindowSpans,
    next: u64,
    spent: Spent,
    interactive: bool,
) {
    let spans = Arc::new(spans);
    let mut st = doc.lock();
    if doc.stale(&st, load) {
        return;
    }
    let size = st.size;
    let Some(chain) = st.chain.as_mut() else {
        return;
    };
    let of = chain.of;
    let moved = chain.advance(start, next, size);
    if chain.slow.record(spent) {
        // Two slow windows in a row: plain text for this opening.
        st.chain = None;
        st.windows.clear();
        st.highlight = Highlighting::Plain;
        doc.commit(st);
        return;
    }
    let table = moved.then(|| chain.table());
    let more = moved && !chain.done;
    let shown = load::visible(&st, &doc.target);
    let near = matches!(want(st.chain.as_ref(), &shown), Want::Windows(s) if s.contains(&start));
    if interactive {
        add(&mut st, spans.clone());
        st.highlight = Highlighting::On;
        doc.commit(st);
    } else {
        drop(st);
    }
    if let Some(table) = table {
        engine.lru.insert(
            LruKey::Starts(of),
            LruValue::Starts(Arc::new(table)),
            Class::Background,
        );
    }
    if interactive || near {
        let class = if interactive {
            Class::Opened
        } else {
            Class::Background
        };
        engine
            .lru
            .insert(of.spans(start), LruValue::Spans(spans), class);
    }
    if more {
        kick(engine);
    }
    if near && !interactive {
        load::visible_work(engine, doc);
    }
}

/// The highlighting is unavailable (the grammar was disabled): plain text.
pub(crate) fn plain(doc: &Doc, load: u64) {
    let mut st = doc.lock();
    if doc.stale(&st, load) {
        return;
    }
    st.chain = None;
    st.windows.clear();
    st.highlight = Highlighting::Plain;
    doc.commit(st);
}

/// The batch side: whether the task is queued or running, and the window
/// it stopped in.
#[derive(Default)]
pub(crate) struct Sched {
    busy: bool,
    /// Work arrived while the task was busy: it takes another turn.
    again: bool,
    parked: Option<Parked>,
    /// Where the next search for a document with chain work begins.
    cursor: usize,
}

/// A batch window that stopped before its end, kept for the next task.
pub(crate) struct Parked {
    doc: Weak<Doc>,
    load: u64,
    start: u64,
    window: ChainWindow,
}

/// One task's window.
struct Job {
    doc: Arc<Doc>,
    load: u64,
    start: u64,
    window: ChainWindow,
    cancel: Arc<AtomicBool>,
}

/// Queues the batch task unless it is queued or running (then it takes
/// another turn). Never called with a document's lock held.
pub(crate) fn kick(engine: &Arc<EngineInner>) {
    {
        let mut sched = lock(&engine.chain);
        if sched.busy {
            sched.again = true;
            return;
        }
        sched.busy = true;
        sched.again = false;
    }
    spawn(engine);
}

fn spawn(engine: &Arc<EngineInner>) {
    let engine2 = engine.clone();
    if !engine.spawn_yielding(move |guard| async move { task(&engine2, guard).await }) {
        lock(&engine.chain).busy = false;
    }
}

/// The batch task: at most one window's step a turn, then back to the end
/// of the batch queue (`yield_now`, as code_intel's batch loop does) while
/// there may be more; it ends when a turn found no window.
async fn task(engine: &Arc<EngineInner>, guard: BackgroundGuard) {
    loop {
        trace!(engine, Trace::ChainTask);
        let worked = segment(engine, &guard).await;
        {
            let mut sched = lock(&engine.chain);
            let again = worked || sched.again;
            sched.again = false;
            if !again {
                sched.busy = false;
                return;
            }
        }
        yield_now().await;
    }
}

/// The batch boundary (the pause and the yield switch), then one step of at
/// most [`BATCH_SEGMENT`] on the next window. False when there was none.
async fn segment(engine: &Arc<EngineInner>, guard: &BackgroundGuard) -> bool {
    // A window is about 64 KiB: it fits a keepalive slot.
    let _grant = guard.proceed_item(WINDOW).await;
    let Some(mut job) = take(engine) else {
        return false;
    };
    let probe = || guard.yield_requested() || guard.pause_requested();
    let budget = Budget {
        budget: BATCH_SEGMENT,
        cancel: &job.cancel,
        yield_probe: Some(&probe),
    };
    trace!(
        engine,
        Trace::ChainStep {
            start: job.start,
            budget: BATCH_SEGMENT,
        }
    );
    let outcome = job.window.step(budget);
    trace!(
        engine,
        Trace::ChainStepEnd {
            start: job.start,
            end: match &outcome {
                Outcome::Done(_) => StepEnd::Done,
                Outcome::TimedOut => StepEnd::TimedOut,
                Outcome::Yielded => StepEnd::Yielded,
                Outcome::Cancelled => StepEnd::Cancelled,
                Outcome::Unavailable => StepEnd::Unavailable,
            },
        }
    );
    match outcome {
        Outcome::Done((spans, next)) => {
            let spent = job.window.spent();
            land(
                engine, &job.doc, job.load, job.start, spans, next, spent, false,
            );
        }
        Outcome::TimedOut | Outcome::Yielded => {
            lock(&engine.chain).parked = Some(Parked {
                doc: Arc::downgrade(&job.doc),
                load: job.load,
                start: job.start,
                window: job.window,
            });
        }
        Outcome::Cancelled => {}
        Outcome::Unavailable => plain(&job.doc, job.load),
    }
    true
}

/// The window to work on: the parked one while its document still wants
/// it, else the frontier of the next document with chain work, in turn.
fn take(engine: &Arc<EngineInner>) -> Option<Job> {
    let parked = lock(&engine.chain).parked.take();
    if let Some(p) = parked
        && let Some(doc) = p.doc.upgrade()
    {
        let cancel = {
            let st = doc.lock();
            let current = !doc.stale(&st, p.load)
                && wants_chain(&st)
                && st
                    .chain
                    .as_ref()
                    .is_some_and(|c| !c.done && c.frontier() == p.start);
            current.then(|| st.cancel.clone())
        };
        if let Some(cancel) = cancel {
            return Some(Job {
                doc,
                load: p.load,
                start: p.start,
                window: p.window,
                cancel,
            });
        }
    }
    let docs = engine.live_docs();
    let cursor = lock(&engine.chain).cursor;
    for i in 0..docs.len() {
        let at = (cursor + i) % docs.len();
        let doc = &docs[at];
        let fresh = {
            let st = doc.lock();
            let chain = st.chain.as_ref().filter(|c| !c.done);
            match chain {
                Some(chain) if wants_chain(&st) => Some((
                    st.load,
                    chain.frontier(),
                    st.lang,
                    st.content.clone(),
                    st.file.clone(),
                    st.identity,
                    st.cancel.clone(),
                    st.size,
                )),
                _ => None,
            }
        };
        let Some((load, start, Some(name), content, file, identity, cancel, size)) = fresh else {
            continue;
        };
        lock(&engine.chain).cursor = at + 1;
        let langs = engine.languages.clone();
        let Some(lang) = langs.get(name) else {
            continue;
        };
        let Some(src) = source(&content, file.as_ref(), identity, start, size) else {
            continue;
        };
        return Some(Job {
            doc: doc.clone(),
            load,
            start,
            window: ChainWindow::new(lang, src, start),
            cancel,
        });
    }
    None
}

/// The document closed: takes the window a task stopped in for it, so the
/// content it holds is dropped with the document's.
pub(crate) fn forget(engine: &EngineInner, doc: &Doc) -> Option<Parked> {
    let mut sched = lock(&engine.chain);
    let theirs = sched
        .parked
        .as_ref()
        .is_some_and(|p| std::ptr::eq(p.doc.as_ptr(), doc));
    if theirs { sched.parked.take() } else { None }
}
