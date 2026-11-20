//! The window's reading state, without the view (docs/design/file_reader.md
//! 「本 crate 的公开项」「数据流与线程」).
//!
//! `Reader::open` may come from any thread: it queues the request, calls
//! the wake-up the view installed, and, while nothing is shown, starts the
//! request's first screen (an early start, ADR 0063: the document waits in
//! a slot, shown by nothing). The view (step 7) takes the requests on the
//! main thread, asks the leave guard, and then either
//! [`apply`](Session::apply)s the request (it adopts the document started
//! for it, or opens one) or [`cancel`](Session::cancel)s it (the document
//! is dropped); the document it shows is the [`Session`]'s, read through
//! [`snapshot`](Session::snapshot). Nothing here blocks or does IO: opening
//! is `Engine::open`, which queues its tasks.
//!
//! Locks: the queue, the wake-up slot and the reading state are separate
//! `Mutex`es, never held across a call into another crate. Effects (events,
//! focus, dropping a document) run after the lock is released: an event is
//! dispatched synchronously, and its listener may call `open` again.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use lectern_logging::{debug, warn};
use lectern_services::code_intel::{Focus, TextRef};
use lectern_services::project::{BlobId, ProjectKey, ProjectSnapshot, WorktreeKey};
use lectern_services::reader::{
    Anchor, ObjectShare, OpenOutcome, OpenTicket, ReadTarget, Reader, ReaderEvent,
};
use lectern_syntax::limits::PREFETCH_MAX;
use lectern_syntax::{
    DocHandle, DocSnapshot, EditSession, Engine, FindQuery, LineNo, NotEditable, Row, RowPos,
    Viewport,
};

use crate::diag;
use crate::focus::{Decision, decide, root_of};
use crate::history::History;

/// What the session needs from the rest of the process: the project
/// snapshots, `code_intel`'s focus and the event bus. The plugin implements
/// it over the kernel's services; tests implement it with fakes.
pub trait Host: Send + Sync {
    /// `Projects::snapshot`.
    fn snapshot(&self, project: &ProjectKey) -> Option<Arc<ProjectSnapshot>>;
    /// `Projects::open_projects`: the projects in the order they were opened
    /// (the Files tree shows the latest).
    fn open_projects(&self) -> Vec<ProjectKey>;
    /// `CodeIntel::set_focus`.
    fn set_focus(&self, project: &ProjectKey, worktree: Option<&WorktreeKey>, focus: Focus);
    /// Emits on the kernel's event bus, from the thread that calls (the
    /// main thread: the session emits only from the methods the view calls).
    fn emit(&self, event: ReaderEvent);
}

/// Called from any thread: "something is waiting for the main thread" (a
/// request was queued, or the current document published a snapshot).
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// The viewport of a document opened before the view has said how big it is.
const FIRST_VIEWPORT: Viewport = Viewport {
    rows: 40,
    cols: 100,
};

/// One `Reader::open`, waiting for the view.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Request {
    pub ticket: OpenTicket,
    pub target: ReadTarget,
}

/// The wake-up the view installs, shared with the documents' `notify`
/// (which holds the slot, not the session: no cycle).
#[derive(Default)]
struct WakeSlot(Mutex<Option<Wake>>);

impl WakeSlot {
    fn call(&self) {
        let wake = lock(&self.0).clone();
        if let Some(wake) = wake {
            wake();
        }
    }
}

/// The text `code_intel` was told is current, with the pair it was told it
/// for (to clear it there).
#[derive(Clone, PartialEq, Eq, Debug)]
struct FocusSet {
    project: ProjectKey,
    worktree: Option<WorktreeKey>,
    text: TextRef,
}

/// The document shown: its handle, what it was opened with and where the
/// view scrolled to.
struct Current {
    handle: DocHandle,
    target: ReadTarget,
    top: Option<RowPos>,
}

struct Reading {
    current: Option<Current>,
    /// What was shown last. It outlives the document (`close`): the Files
    /// tree opens at it after the reader is left.
    last: Option<ReadTarget>,
    history: History,
    viewport: Viewport,
    focus: Option<FocusSet>,
}

/// The first screen of a request, started when `Reader::open` was called
/// (docs/design/file_reader.md「数据流与线程」): the document, not yet shown.
/// `apply` adopts it, `cancel` drops it.
struct Prestart {
    ticket: OpenTicket,
    root: Arc<Path>,
    handle: DocHandle,
}

/// The one speculative document of the session.
enum Speculative {
    None,
    /// Someone is opening it (the lock is not held across `Engine::open`).
    Claimed,
    Ready(Prestart),
}

enum Via {
    Open,
    Back,
    Forward,
}

/// One window's reading state. Built by the plugin's `activate`
/// (`Engine::new`, no IO) and provided as `dyn Reader`.
pub struct Session {
    engine: Engine,
    host: Arc<dyn Host>,
    next_ticket: AtomicU64,
    pending: Mutex<VecDeque<Request>>,
    wake: Arc<WakeSlot>,
    reading: Mutex<Reading>,
    /// At most one: for the first request of a session that shows nothing
    /// yet.
    prestart: Mutex<Speculative>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl Session {
    pub fn new(engine: Engine, host: Arc<dyn Host>) -> Session {
        Session {
            engine,
            host,
            next_ticket: AtomicU64::new(1),
            pending: Mutex::new(VecDeque::new()),
            wake: Arc::default(),
            reading: Mutex::new(Reading {
                current: None,
                last: None,
                history: History::default(),
                viewport: FIRST_VIEWPORT,
                focus: None,
            }),
            prestart: Mutex::new(Speculative::None),
        }
    }

    /// The plugin's `started`: documents' tasks go through `handle` from
    /// now on.
    pub fn start(&self, handle: lectern_kernel::PluginHandle) {
        self.engine.start(handle);
    }

    /// A project closed: the engine drops its roots and subscriptions.
    /// Never blocks; any thread.
    pub fn close_project(&self, project: &ProjectKey) {
        self.engine.close_project(project);
    }

    /// The engine (tests).
    #[cfg(test)]
    pub(crate) fn engine(&self) -> &Engine {
        &self.engine
    }

    // ---- the view's side (main thread) -----------------------------------

    /// Installs (or removes) the wake-up. Requests queued before it was
    /// installed call it now.
    pub fn set_waker(&self, wake: Option<Wake>) {
        let install = wake.is_some();
        *lock(&self.wake.0) = wake;
        if install && !lock(&self.pending).is_empty() {
            self.wake.call();
        }
    }

    /// The oldest request not taken yet.
    pub fn next_request(&self) -> Option<Request> {
        lock(&self.pending).pop_front()
    }

    /// Requests waiting.
    pub fn pending(&self) -> usize {
        lock(&self.pending).len()
    }

    /// The leave guard passed: the request replaces the document shown (the
    /// position left goes to the back history, what was ahead is gone), and
    /// `ReaderEvent::Open { Shown }` follows. When the target cannot be
    /// placed (its project is closed, or has no such worktree) nothing
    /// changes and the event says `Cancelled`.
    pub fn apply(&self, request: Request) -> OpenOutcome {
        let shown = self.switch(request.target, Via::Open, Some(request.ticket));
        let outcome = if shown {
            OpenOutcome::Shown
        } else {
            OpenOutcome::Cancelled
        };
        self.host.emit(ReaderEvent::Open {
            ticket: request.ticket,
            outcome,
        });
        outcome
    }

    /// The leave guard said no: the document shown and the history stay,
    /// and `ReaderEvent::Open { Cancelled }` follows.
    pub fn cancel(&self, request: Request) {
        // The document its first screen was started for is not shown.
        drop(self.take_prestart(request.ticket));
        self.host.emit(ReaderEvent::Open {
            ticket: request.ticket,
            outcome: OpenOutcome::Cancelled,
        });
    }

    /// Back to the nearest position behind (⌘[), after the leave guard.
    /// False when there is none or it cannot be placed.
    pub fn back(&self) -> bool {
        let target = lock(&self.reading).history.peek_back().cloned();
        target.is_some_and(|target| self.switch(target, Via::Back, None))
    }

    /// Forward to the nearest position ahead (⌘]), after the leave guard.
    pub fn forward(&self) -> bool {
        let target = lock(&self.reading).history.peek_forward().cloned();
        target.is_some_and(|target| self.switch(target, Via::Forward, None))
    }

    pub fn can_back(&self) -> bool {
        lock(&self.reading).history.can_back()
    }

    pub fn can_forward(&self) -> bool {
        lock(&self.reading).history.can_forward()
    }

    /// The positions behind and ahead (tests, probe).
    pub fn history_len(&self) -> (usize, usize) {
        lock(&self.reading).history.len()
    }

    /// The viewport changed.
    pub fn set_viewport(&self, viewport: Viewport) {
        let mut reading = lock(&self.reading);
        reading.viewport = viewport;
        if let Some(current) = &reading.current {
            current.handle.set_viewport(viewport);
        }
    }

    /// The view scrolled to `top`: kept for the history, and the document
    /// highlights there (tiers B and C).
    pub fn scrolled(&self, top: RowPos) {
        let mut reading = lock(&self.reading);
        if let Some(current) = &mut reading.current {
            current.top = Some(top);
            current.handle.scrolled(top);
        }
    }

    /// Finds `query` in the document shown (`None` clears the find). The
    /// matches come in later snapshots.
    pub fn find(&self, query: Option<FindQuery>) {
        if let Some(current) = &lock(&self.reading).current {
            match query {
                Some(query) => current.handle.find(query),
                None => current.handle.clear_find(),
            }
        }
    }

    /// The text `code_intel` was told is current, with its project: what a
    /// query about a place in the document names (the pointer's prefetch).
    /// None until the content's OID is known, and where no text is current.
    pub fn current_text(&self) -> Option<(ProjectKey, TextRef)> {
        lock(&self.reading)
            .focus
            .as_ref()
            .map(|focus| (focus.project.clone(), focus.text.clone()))
    }

    /// The target of the document shown last, kept after the reading view
    /// ends (`close`): what the Files tree reveals when it opens.
    pub fn last_target(&self) -> Option<ReadTarget> {
        lock(&self.reading).last.clone()
    }

    /// The project the window shows: the one opened last. The Files tree
    /// reads its snapshot (`Projects::snapshot`, a copy of an `Arc`).
    pub fn open_project(&self) -> Option<ProjectKey> {
        self.host.open_projects().pop()
    }

    /// `Projects::snapshot` of `project`.
    pub fn project_snapshot(&self, project: &ProjectKey) -> Option<Arc<ProjectSnapshot>> {
        self.host.snapshot(project)
    }

    /// Enters edit mode in the document shown (docs/design/file_reader.md
    /// 「编辑」): the session over its content, or why there is none. The
    /// view holds the session while it edits; the last handle's drop ends
    /// it. Copies the content into the buffer's text on this thread.
    pub fn edit(&self) -> Result<EditSession, NotEditable> {
        match &lock(&self.reading).current {
            Some(current) => current.handle.edit(),
            None => Err(NotEditable::NotLoaded),
        }
    }

    /// The current snapshot, without touching the focus.
    pub fn snapshot(&self) -> Option<Arc<DocSnapshot>> {
        lock(&self.reading)
            .current
            .as_ref()
            .map(|c| c.handle.snapshot())
    }

    /// The current snapshot, after bringing `code_intel`'s focus up to
    /// date: `Current(Some(TextRef))` once the snapshot's OID is known (and
    /// again when it changes), `Current(None)` when it is not or the
    /// document is gone. The view calls it after every wake-up; it calls
    /// `set_focus` only when the text changed.
    pub fn sync(&self) -> Option<Arc<DocSnapshot>> {
        let (target, snapshot) = {
            let reading = lock(&self.reading);
            match &reading.current {
                Some(c) => (Some(c.target.clone()), Some(c.handle.snapshot())),
                None => (None, None),
            }
        };
        let desired = match (target, &snapshot) {
            (Some(target), Some(snapshot)) => {
                let project = self.host.snapshot(&target.project);
                match decide(
                    &target,
                    snapshot.oid,
                    snapshot.canonical,
                    project.as_deref(),
                ) {
                    Decision::Focus(text) => Some(FocusSet {
                        project: target.project,
                        worktree: target.worktree,
                        text,
                    }),
                    Decision::AskCanonical => {
                        // Queues a `Background` task in `syntax` and returns:
                        // no lock of this crate is taken by it.
                        if let Some(current) = &lock(&self.reading).current {
                            current.handle.canonical_oid();
                        }
                        None
                    }
                    Decision::None => None,
                }
            }
            _ => None,
        };
        self.set_focus(desired);
        snapshot
    }

    /// The reading view closed: the document goes (its tasks are cancelled,
    /// its file closed off this thread) and the focus is cleared. The
    /// history stays.
    pub fn close(&self) {
        let current = lock(&self.reading).current.take();
        drop(current);
        self.set_focus(None);
    }

    // ---- the process's side (any thread) ---------------------------------

    fn root(&self, target: &ReadTarget) -> Option<Arc<Path>> {
        let snapshot = self.host.snapshot(&target.project)?;
        root_of(&snapshot, target)
    }

    /// Shows `target` instead of the current document. False when it
    /// cannot be placed: nothing changed.
    fn switch(&self, target: ReadTarget, via: Via, ticket: Option<OpenTicket>) -> bool {
        // Taken first: a request that is not shown (no root) must not leave
        // its early-started document in the slot.
        let started = ticket.and_then(|ticket| self.take_prestart(ticket));
        let Some(root) = self.root(&target) else {
            warn!(
                "a request to show a file is dropped; its project or worktree has no root";
                via = match via {
                    Via::Open => "open",
                    Via::Back => "back",
                    Via::Forward => "forward",
                },
                origin = diag::origin(&target.origin),
                ticket = ticket.map(|t| t.0),
            );
            return false;
        };
        let viewport = lock(&self.reading).viewport;
        let handle = match started.filter(|started| started.root == root) {
            // Its first screen was started for this request (the viewport
            // may have changed since).
            Some(started) => {
                started.handle.set_viewport(viewport);
                started.handle
            }
            None => self.open_document(target.clone(), root, viewport),
        };
        let left = {
            let mut reading = lock(&self.reading);
            let cols = reading.viewport.cols;
            let left = reading.current.take();
            let position = left.as_ref().map(|c| position(c, cols));
            match via {
                Via::Open => reading.history.visit(position),
                Via::Back => reading.history.went_back(position),
                Via::Forward => reading.history.went_forward(position),
            }
            reading.last = Some(target.clone());
            reading.current = Some(Current {
                handle,
                target,
                top: None,
            });
            left
        };
        // Cancels the old document's tasks and closes its file.
        drop(left);
        // The new document's OID is not known yet: the old focus goes.
        self.sync();
        true
    }

    /// `Engine::open` with the wake-up slot as the document's `notify`.
    fn open_document(&self, target: ReadTarget, root: Arc<Path>, viewport: Viewport) -> DocHandle {
        let slot = self.wake.clone();
        self.engine
            .open(target, root, viewport, Arc::new(move || slot.call()))
    }

    /// Starts the first screen of `target` before the view exists, when
    /// nothing is shown, nothing waits and nothing is started (so no edit
    /// is open, and no leave guard has a question): the result is ready when
    /// the view has been built. True when it was started. Nothing the
    /// session shows changes until `apply`.
    fn start_early(&self, ticket: OpenTicket, target: &ReadTarget) -> bool {
        if !self.engine.started() {
            return false;
        }
        {
            let mut slot = lock(&self.prestart);
            if !matches!(*slot, Speculative::None)
                || !lock(&self.pending).is_empty()
                || lock(&self.reading).current.is_some()
            {
                return false;
            }
            *slot = Speculative::Claimed;
        }
        let Some(root) = self.root(target) else {
            *lock(&self.prestart) = Speculative::None;
            return false;
        };
        let viewport = lock(&self.reading).viewport;
        let handle = self.open_document(target.clone(), root.clone(), viewport);
        *lock(&self.prestart) = Speculative::Ready(Prestart {
            ticket,
            root,
            handle,
        });
        true
    }

    /// The document started for `ticket`, if there is one.
    fn take_prestart(&self, ticket: OpenTicket) -> Option<Prestart> {
        let mut slot = lock(&self.prestart);
        match &*slot {
            Speculative::Ready(started) if started.ticket == ticket => {
                match std::mem::replace(&mut *slot, Speculative::None) {
                    Speculative::Ready(started) => Some(started),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Whether a first screen was started for `ticket` and not yet adopted
    /// or dropped (tests).
    #[cfg(test)]
    pub(crate) fn prestarted(&self, ticket: OpenTicket) -> bool {
        matches!(&*lock(&self.prestart), Speculative::Ready(s) if s.ticket == ticket)
    }

    fn set_focus(&self, desired: Option<FocusSet>) {
        let (clear, set) = {
            let mut reading = lock(&self.reading);
            if reading.focus == desired {
                return;
            }
            let old = std::mem::replace(&mut reading.focus, desired.clone());
            let clear = old.filter(|old| {
                desired
                    .as_ref()
                    .is_none_or(|d| (&d.project, &d.worktree) != (&old.project, &old.worktree))
            });
            (clear, desired)
        };
        if let Some(old) = clear {
            self.host
                .set_focus(&old.project, old.worktree.as_ref(), Focus::Current(None));
        }
        if let Some(new) = set {
            self.host.set_focus(
                &new.project,
                new.worktree.as_ref(),
                Focus::Current(Some(new.text)),
            );
        }
    }
}

/// Where the reading of `current` is, as a target to come back to: the
/// OID of the content seen and the byte offset and line of the first row
/// shown. No content.
fn position(current: &Current, cols: u32) -> ReadTarget {
    let snapshot = current.handle.snapshot();
    let top = current.top.unwrap_or(snapshot.anchor);
    let at = match snapshot.rows(top, 1, cols).first() {
        Some(Row::Text { pos, line, .. }) => {
            let (LineNo::Exact(line) | LineNo::Estimated(line)) = *line;
            Anchor::Byte {
                byte: pos.line_start,
                line: u32::try_from(line).unwrap_or(u32::MAX),
                column: 0,
            }
        }
        _ => current.target.at,
    };
    let expect: Option<BlobId> = snapshot.oid.or(current.target.expect);
    ReadTarget {
        expect,
        at,
        ..current.target.clone()
    }
}

impl Reader for Session {
    fn open(&self, target: ReadTarget) -> OpenTicket {
        let ticket = OpenTicket(self.next_ticket.fetch_add(1, Ordering::Relaxed));
        // The language's query compiles beside the first screen's reading,
        // not after it (about 5–18 ms the first time in a process).
        self.engine.warm(&target.path);
        let prestarted = self.start_early(ticket, &target);
        let pending = {
            let mut queue = lock(&self.pending);
            queue.push_back(Request { ticket, target });
            queue.len()
        };
        debug!(
            "a request to open a file was queued";
            ticket = ticket.0,
            prestarted = prestarted,
            pending = pending,
        );
        self.wake.call();
        ticket
    }

    fn prefetch(&self, targets: Vec<ReadTarget>) {
        let mut resolved = Vec::with_capacity(targets.len().min(PREFETCH_MAX));
        for target in targets {
            if resolved.len() == PREFETCH_MAX {
                break;
            }
            if let Some(root) = self.root(&target) {
                resolved.push((target, root));
            }
        }
        self.engine.prefetch(resolved);
    }

    fn share_object(&self, oid: BlobId, bytes: Arc<[u8]>) -> ObjectShare {
        self.engine.share_object(oid, bytes)
    }
}
