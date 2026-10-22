//! The engine (one per process) and the open documents' shared state.
//!
//! A document's tasks are numbered by load: opening, and every reload after
//! an outside change, starts a new load and cancels the old one's tasks
//! (their results are dropped when they land). Every task goes through the
//! plugin's handle (`own_with`) holding a `BusyGuard`, except the
//! memory-only batch work (the prefetch, the window chain's batch windows),
//! which holds a yielding `BackgroundGuard` (ADR 0033).

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
#[cfg(any(test, feature = "test-support"))]
use std::time::Duration;

use lectern_cache::{Cache, Partition};
use lectern_executor::{Executor, Priority};
use lectern_fs_watch::Watcher;
use lectern_git::HashKind;
use lectern_kernel::{BackgroundGuard, BusyGuard, PluginHandle};
use lectern_platform::{FileIdentity, OpenFile, SafeRoot};
use lectern_services::code_intel::TextOrigin;
use lectern_services::project::{BlobId, ProjectKey};
use lectern_services::reader::{Anchor, ObjectShare, ReadTarget};

use crate::Languages;
use crate::find::{FindQuery, FindResults};
use crate::git_access::{GitAccess, LecternGit, hash_kind};
use crate::limits::LRU;
use crate::lru::Lru;
use crate::objects::Shares;
use crate::spans::WindowSpans;
use crate::text::LineCheckpoints;
use crate::util::{lock, log};

use super::chain::{self, ChainState};
use super::content::{Content, PageCache};
use super::edit::{self, EditSession};
use super::{
    DocSnapshot, DocState, Editable, Highlighting, LineCount, NotEditable, RowPos, Tier, Viewport,
    load, prefetch, watch,
};

/// How many file identities and the OIDs their scans found are kept, so a
/// later open can use the line checkpoints (folder projects have no
/// `FileList` OID).
const KNOWN_MAX: usize = 64;

/// What the engine is built from: `app`'s shared resources, handed over by
/// the file-reader plugin.
pub struct EngineDeps {
    pub executor: Executor,
    pub cache: Cache,
    pub watcher: Watcher,
    pub languages: Arc<Languages>,
}

/// One per process, made by the file-reader plugin: the language table,
/// the LRU, the roots' `SafeRoot`s, the object share registry.
pub struct Engine {
    inner: Arc<EngineInner>,
}

pub(crate) struct EngineInner {
    pub executor: Executor,
    pub cache: Cache,
    pub watcher: Watcher,
    pub languages: Arc<Languages>,
    pub git: Arc<dyn GitAccess>,
    handle: Mutex<Option<PluginHandle>>,
    roots: Mutex<Roots>,
    pub lru: Lru,
    pub shares: Arc<Shares>,
    known: Mutex<VecDeque<Known>>,
    docs: Mutex<Vec<Weak<Doc>>>,
    in_flight: Arc<AtomicUsize>,
    /// The window chain's batch task (at most one) and the window it
    /// stopped in.
    pub chain: Mutex<chain::Sched>,
    /// The prefetch's targets and its task (at most one).
    pub prefetch: Mutex<prefetch::Queue>,
    /// Runs once right after the next first screen is published (tests).
    #[cfg(test)]
    pub after_first: Mutex<Option<Hook>>,
    #[cfg(any(test, feature = "test-support"))]
    trace: Mutex<Option<Tracer>>,
}

/// Gets the snapshot just published.
#[cfg(test)]
pub(crate) type Hook = Box<dyn FnOnce(&DocSnapshot) + Send>;

#[cfg(any(test, feature = "test-support"))]
type Tracer = Arc<dyn Fn(Trace) + Send + Sync>;

/// What the engine's batch work does, as it happens (`test-support`: the
/// tests and `syntax/prefetch_wait`). Called on the thread doing it.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Trace {
    /// A turn of the prefetch task begins: it got a thread, first or back
    /// from the end of the batch queue.
    PrefetchTask,
    /// The prefetch begins to read and hash `path` (over 1 MiB, its OID
    /// not known for its identity).
    PrefetchHash { path: Arc<str> },
    /// The prefetch begins the parse of `path`: the whole file up to 1 MiB,
    /// else the first screen's window.
    PrefetchParse { path: Arc<str> },
    /// That parse stopped at a slice boundary for the yield switch (or the
    /// pause), keeping its state.
    PrefetchYielded { path: Arc<str> },
    /// The prefetch put `path`'s spans in the LRU.
    PrefetchDone { path: Arc<str> },
    /// A turn of the window chain's batch task begins.
    ChainTask,
    /// A batch window's step from `start` begins with `budget`.
    ChainStep { start: u64, budget: Duration },
    /// It returned.
    ChainStepEnd { start: u64, end: StepEnd },
}

/// How a traced step ended.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StepEnd {
    Done,
    TimedOut,
    Yielded,
    Cancelled,
    Unavailable,
}

/// Reports a [`Trace`] event when the tracer is set; nothing outside
/// `test-support`.
macro_rules! trace {
    ($engine:expr, $event:expr) => {
        #[cfg(any(test, feature = "test-support"))]
        $engine.trace($event);
    };
}
pub(crate) use trace;

/// The roots' `SafeRoot`s, by project and root path.
type Roots = HashMap<(ProjectKey, Arc<Path>), Arc<SafeRoot>>;

struct Known {
    root: Arc<Path>,
    path: Arc<str>,
    identity: FileIdentity,
    oid: BlobId,
}

impl Engine {
    /// No IO, no thread (ADR 0017).
    pub fn new(deps: EngineDeps) -> Engine {
        Engine::with_git(deps, Arc::new(LecternGit::new()))
    }

    pub(crate) fn with_git(deps: EngineDeps, git: Arc<dyn GitAccess>) -> Engine {
        Engine {
            inner: Arc::new(EngineInner {
                executor: deps.executor,
                cache: deps.cache,
                watcher: deps.watcher,
                languages: deps.languages,
                git,
                handle: Mutex::new(None),
                roots: Mutex::new(HashMap::new()),
                lru: Lru::new(LRU),
                shares: Arc::new(Shares::default()),
                known: Mutex::new(VecDeque::new()),
                docs: Mutex::new(Vec::new()),
                in_flight: Arc::new(AtomicUsize::new(0)),
                chain: Mutex::new(chain::Sched::default()),
                prefetch: Mutex::new(prefetch::Queue::default()),
                #[cfg(test)]
                after_first: Mutex::new(None),
                #[cfg(any(test, feature = "test-support"))]
                trace: Mutex::new(None),
            }),
        }
    }

    /// Called once from the plugin's `started`: tasks go through this
    /// handle from then on.
    pub fn start(&self, handle: PluginHandle) {
        *lock(&self.inner.handle) = Some(handle);
    }

    /// Opens a document. Returns at once and may be called on the UI
    /// thread: the first screen is read in an `Interactive` task, which
    /// calls `notify` (any thread) after each new snapshot. `root` is the
    /// worktree's or folder's root, from the project snapshot.
    pub fn open(
        &self,
        target: ReadTarget,
        root: Arc<Path>,
        viewport: Viewport,
        notify: Arc<dyn Fn() + Send + Sync>,
    ) -> DocHandle {
        let doc = Arc::new(Doc::new(&self.inner, target, root, viewport, notify));
        {
            let mut docs = lock(&self.inner.docs);
            docs.retain(|d| d.strong_count() > 0);
            docs.push(Arc::downgrade(&doc));
        }
        let at = doc.target.at;
        load::start(&self.inner, &doc, at);
        DocHandle { doc }
    }

    /// Prefetch while interaction is quiet (`Reader::prefetch`, ADR 0033):
    /// the first [`PREFETCH_MAX`](crate::limits::PREFETCH_MAX) targets, each
    /// with its root, replace those of the last request not started yet.
    /// One `Batch` task at a time, one file a turn, holding a yielding
    /// `BackgroundGuard`: a file up to 1 MiB gets its whole spans, a larger
    /// one its first screen's window; the results go only into the LRU (as
    /// background work, evicted first), nothing is opened or notified.
    /// Returns at once.
    pub fn prefetch(&self, targets: Vec<(ReadTarget, Arc<Path>)>) {
        prefetch::request(&self.inner, targets);
    }

    /// Registers object database content someone holds (the review's diff
    /// sides) until the returned handle drops: opening that OID's `Object`
    /// target then reads no object. Never blocks.
    pub fn share_object(&self, oid: BlobId, bytes: Arc<[u8]>) -> ObjectShare {
        self.inner.shares.register(oid, bytes)
    }

    /// The project closed: drops its roots, its repository handle and its
    /// documents' subscriptions. A document with a live edit session keeps
    /// its subscription, and the session its root, until the session ends
    /// (the view may hold unsaved changes). Never blocks.
    pub fn close_project(&self, project: &ProjectKey) {
        let inner = &self.inner;
        let roots: Vec<Arc<SafeRoot>> = {
            let mut roots = lock(&inner.roots);
            let keys: Vec<_> = roots
                .keys()
                .filter(|(p, _)| p == project)
                .cloned()
                .collect();
            keys.iter().filter_map(|k| roots.remove(k)).collect()
        };
        let docs: Vec<Arc<Doc>> = lock(&inner.docs)
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|d| &d.target.project == project && d.editing().is_none())
            .collect();
        let watches: Vec<_> = docs.iter().filter_map(|d| d.lock().watch.take()).collect();
        let git = inner.git.clone();
        let common = match project {
            ProjectKey::Git { common_dir } => Some(common_dir.clone()),
            ProjectKey::Folder { .. } => None,
        };
        // Closing descriptors is IO: off this thread.
        let garbage = (roots, watches);
        if !inner.spawn(Priority::Background, async move {
            drop(garbage);
            if let Some(common) = common {
                git.forget(&common);
            }
        }) {
            log(format_args!(
                "close_project before the engine started; nothing to release"
            ));
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Engine {
    /// The tasks submitted that have not ended.
    pub fn tasks_in_flight(&self) -> usize {
        self.inner.in_flight.load(Ordering::SeqCst)
    }

    /// The bytes the LRU counts.
    pub fn lru_bytes(&self) -> u64 {
        self.inner.lru.total()
    }

    /// The LRU's entries.
    pub fn lru_len(&self) -> usize {
        self.inner.lru.len()
    }

    /// Reports what the batch work does from now on ([`Trace`]), on the
    /// threads doing it.
    pub fn set_trace(&self, tracer: impl Fn(Trace) + Send + Sync + 'static) {
        *lock(&self.inner.trace) = Some(Arc::new(tracer));
    }

    /// The first screen of `target` on the calling thread, as `open`'s
    /// first task computes it, with no task after it: for the benchmarks.
    pub fn first_screen(
        &self,
        target: ReadTarget,
        root: Arc<Path>,
        viewport: Viewport,
    ) -> Arc<DocSnapshot> {
        self.first_screen_within(target, root, viewport, crate::limits::FIRST_WINDOW_DEADLINE)
    }

    /// [`Engine::first_screen`] with the window's parse given `deadline`
    /// of wall time instead of the 8 ms: for a test whose subject is not
    /// the deadline (`deadline_*` test it), which a busy machine misses
    /// when the thread waits for a CPU.
    pub(crate) fn first_screen_within(
        &self,
        target: ReadTarget,
        root: Arc<Path>,
        viewport: Viewport,
        deadline: Duration,
    ) -> Arc<DocSnapshot> {
        let doc = Doc::new(&self.inner, target, root, viewport, Arc::new(|| {}));
        let cancel = AtomicBool::new(false);
        let first = load::first_screen(
            &self.inner,
            &doc.target,
            &doc.root,
            doc.target.at,
            doc.target.expect,
            viewport.rows,
            crate::highlight::Budget {
                budget: deadline,
                ..load::first_budget(&cancel)
            },
            crate::lru::Class::Opened,
        );
        let mut st = doc.lock();
        let _ = load::fill_first(&mut st, &doc.target, first);
        Arc::new(st.snapshot(&doc.target))
    }
}

#[cfg(test)]
impl Engine {
    pub(crate) fn inner(&self) -> &Arc<EngineInner> {
        &self.inner
    }
}

#[cfg(test)]
impl EngineInner {
    /// The count of tasks submitted and not ended.
    pub(crate) fn in_flight(&self) -> Arc<AtomicUsize> {
        self.in_flight.clone()
    }
}

impl EngineInner {
    pub(crate) fn handle(&self) -> Option<PluginHandle> {
        lock(&self.handle).clone()
    }

    pub(crate) fn busy(&self) -> Option<BusyGuard> {
        self.handle()?.busy()
    }

    /// Submits `work` holding a `BusyGuard`. False when the engine has not
    /// started or the plugin is gone; `work` is then dropped unrun.
    pub(crate) fn spawn(
        &self,
        priority: Priority,
        work: impl Future<Output = ()> + Send + 'static,
    ) -> bool {
        let guard = self.busy();
        self.spawn_with(priority, guard, work)
    }

    /// [`spawn`](Self::spawn) with a guard taken earlier (a watch batch's).
    pub(crate) fn spawn_with(
        &self,
        priority: Priority,
        guard: Option<BusyGuard>,
        work: impl Future<Output = ()> + Send + 'static,
    ) -> bool {
        let (Some(handle), Some(guard)) = (self.handle(), guard) else {
            return false;
        };
        let count = InFlight::new(&self.in_flight);
        let executor = self.executor.clone();
        handle.own_with(move |gate| {
            executor.spawn_gated(&gate, priority, async move {
                let _guard = guard;
                let _count = count;
                work.await;
            })
        })
    }

    /// Submits memory-only batch work (ADR 0033): a `Batch` task holding a
    /// yielding `BackgroundGuard`, which `work` gets. False when the engine
    /// has not started or the plugin is gone; nothing runs then.
    pub(crate) fn spawn_yielding<F, Fut>(&self, work: F) -> bool
    where
        F: FnOnce(BackgroundGuard) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let Some(handle) = self.handle() else {
            return false;
        };
        let Some(guard) = handle.background_yielding() else {
            return false;
        };
        let count = InFlight::new(&self.in_flight);
        let executor = self.executor.clone();
        let work = work(guard);
        handle.own_with(move |gate| {
            executor.spawn_gated(&gate, Priority::Batch, async move {
                let _count = count;
                work.await;
            })
        })
    }

    /// The documents still open.
    pub(crate) fn live_docs(&self) -> Vec<Arc<Doc>> {
        lock(&self.docs)
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|d| !d.is_closed())
            .collect()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn trace(&self, event: Trace) {
        let tracer = lock(&self.trace).clone();
        if let Some(tracer) = tracer {
            tracer(event);
        }
    }

    /// The root's `SafeRoot` if it is open already. Never blocks.
    pub(crate) fn cached_root(
        &self,
        project: &ProjectKey,
        root: &Arc<Path>,
    ) -> Option<Arc<SafeRoot>> {
        lock(&self.roots)
            .get(&(project.clone(), root.clone()))
            .cloned()
    }

    /// The root's `SafeRoot`, opened on first use. Blocking.
    pub(crate) fn root(
        &self,
        project: &ProjectKey,
        root: &Arc<Path>,
    ) -> std::io::Result<Arc<SafeRoot>> {
        let key = (project.clone(), root.clone());
        if let Some(safe) = lock(&self.roots).get(&key) {
            return Ok(safe.clone());
        }
        let safe = Arc::new(SafeRoot::open(root)?);
        lock(&self.roots).insert(key, safe.clone());
        Ok(safe)
    }

    pub(crate) fn partition(&self, project: &ProjectKey) -> Partition {
        match project {
            ProjectKey::Git { common_dir } => self.cache.repo(common_dir),
            ProjectKey::Folder { root } => self.cache.folder(root),
        }
    }

    /// The line checkpoints of `oid`. Blocking (a cache read).
    pub(crate) fn checkpoints(
        &self,
        project: &ProjectKey,
        oid: &BlobId,
    ) -> Option<LineCheckpoints> {
        self.partition(project).get(&LineCheckpoints::key(oid))
    }

    /// Writes the checkpoints unless they are there. Blocking.
    pub(crate) fn store_checkpoints(
        &self,
        project: &ProjectKey,
        oid: &BlobId,
        checkpoints: &LineCheckpoints,
    ) {
        let partition = self.partition(project);
        let key = LineCheckpoints::key(oid);
        if partition.get::<LineCheckpoints>(&key).as_ref() != Some(checkpoints) {
            let _ = partition.put(&key, checkpoints);
        }
    }

    /// The hash kind of a document's raw OID: `expect`'s, else the
    /// repository's (SHA-1 for a folder project). Blocking.
    pub(crate) fn hash_kind(&self, target: &ReadTarget) -> HashKind {
        if let Some(expect) = &target.expect {
            return hash_kind(expect);
        }
        match &target.project {
            ProjectKey::Git { common_dir } => self.git.hash(common_dir).unwrap_or(HashKind::Sha1),
            ProjectKey::Folder { .. } => HashKind::Sha1,
        }
    }

    pub(crate) fn remember(
        &self,
        root: &Arc<Path>,
        path: &Arc<str>,
        identity: FileIdentity,
        oid: BlobId,
    ) {
        let mut known = lock(&self.known);
        known.retain(|k| !(k.root == *root && k.path == *path));
        known.push_back(Known {
            root: root.clone(),
            path: path.clone(),
            identity,
            oid,
        });
        while known.len() > KNOWN_MAX {
            known.pop_front();
        }
    }

    pub(crate) fn known_oid(
        &self,
        root: &Arc<Path>,
        path: &Arc<str>,
        identity: FileIdentity,
    ) -> Option<BlobId> {
        lock(&self.known)
            .iter()
            .find(|k| k.root == *root && k.path == *path && k.identity == identity)
            .map(|k| k.oid)
    }
}

/// Counts a task from submission until it is dropped.
struct InFlight(Arc<AtomicUsize>);

impl InFlight {
    fn new(count: &Arc<AtomicUsize>) -> InFlight {
        count.fetch_add(1, Ordering::SeqCst);
        InFlight(count.clone())
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// An open document. Dropping it cancels its tasks and releases its
/// `OpenFile` and subscription (off this thread); its spans and texts of at
/// most 1 MiB stay in the LRU.
pub struct DocHandle {
    doc: Arc<Doc>,
}

impl DocHandle {
    /// The current snapshot: copies an `Arc` under a lock. Never blocks for
    /// long; callable every frame on the UI thread.
    pub fn snapshot(&self) -> Arc<DocSnapshot> {
        lock(&self.doc.snapshot).clone()
    }

    /// The commands below return at once; their results show in later
    /// snapshots.
    pub fn set_viewport(&self, viewport: Viewport) {
        self.doc.lock().viewport = viewport;
        self.scrolled_again();
    }

    /// Where the view scrolled to: tiers B and C highlight the window
    /// there, tier C reads its pages.
    pub fn scrolled(&self, top: RowPos) {
        self.doc.lock().top = Some(top);
        self.scrolled_again();
    }

    fn scrolled_again(&self) {
        if let Some(engine) = self.doc.engine.upgrade() {
            load::visible_work(&engine, &self.doc);
        }
    }

    /// Finds `query`; a new query cancels the last one.
    pub fn find(&self, query: FindQuery) {
        if let Some(engine) = self.doc.engine.upgrade() {
            load::find(&engine, &self.doc, Some(query));
        }
    }

    pub fn clear_find(&self) {
        if let Some(engine) = self.doc.engine.upgrade() {
            load::find(&engine, &self.doc, None);
        }
    }

    /// Enters edit mode (only when the snapshot's `editable` is `Yes`): a
    /// session over the full content, or another handle to the one that
    /// lives. Copies the content into the buffer's text on the calling
    /// thread (no BOM, `\r\n` as `\n`); the first parse runs in a task.
    pub fn edit(&self) -> Result<EditSession, NotEditable> {
        let engine = self.doc.engine.upgrade().ok_or(NotEditable::NotLoaded)?;
        edit::begin(&engine, &self.doc)
    }
}

#[cfg(test)]
impl DocHandle {
    pub(crate) fn is_watched(&self) -> bool {
        self.doc.lock().watch.is_some()
    }

    /// The window chain's starts, and whether it reached the end.
    pub(crate) fn chain(&self) -> Option<(Vec<u64>, bool)> {
        let st = self.doc.lock();
        st.chain.as_ref().map(|c| (c.starts().to_vec(), c.done))
    }

    /// Starts a reload as an outside change would.
    pub(crate) fn reload(&self) {
        if let Some(engine) = self.doc.engine.upgrade() {
            let at = self.doc.current_line();
            load::reload(&engine, &self.doc, at);
        }
    }
}

impl Drop for DocHandle {
    fn drop(&mut self) {
        self.doc.close();
    }
}

pub(crate) struct Doc {
    pub engine: Weak<EngineInner>,
    pub target: ReadTarget,
    pub root: Arc<Path>,
    notify: Arc<dyn Fn() + Send + Sync>,
    snapshot: Mutex<Arc<DocSnapshot>>,
    st: Mutex<DocMut>,
    closed: AtomicBool,
}

/// The document's mutable state; every snapshot is made from it.
pub(crate) struct DocMut {
    pub generation: u64,
    /// The current load; tasks of another one drop their results.
    pub load: u64,
    /// Where the current load opens: the target's anchor, or the line at
    /// the top when an outside change reopened the document.
    pub at: Anchor,
    pub cancel: Arc<AtomicBool>,
    pub viewport: Viewport,
    pub top: Option<RowPos>,
    pub state: DocState,
    pub size: u64,
    pub oid: Option<BlobId>,
    pub tier: Option<Tier>,
    pub highlight: Highlighting,
    pub lines: LineCount,
    pub widest: u32,
    pub editable: Editable,
    pub anchor: RowPos,
    pub content: Content,
    /// The caller's OID of the content (`ReadTarget::expect`); None after
    /// a reload, since the content it described changed.
    pub expect: Option<BlobId>,
    /// The whole file's spans (tier A), exact.
    pub whole: Option<Arc<WindowSpans>>,
    /// The OID `whole` belongs to, when the first screen took it from the
    /// LRU: the full read drops it unless the content's OID is that one.
    pub whole_from: Option<BlobId>,
    /// The first screen's and the visible windows' spans, newest first:
    /// windows of the chain (exact), or parsed alone where the chain has not
    /// got to yet.
    pub windows: Vec<Arc<WindowSpans>>,
    /// Tiers B and C once the content's OID is known: the window chain's
    /// starts.
    pub chain: Option<ChainState>,
    /// The identity of the file read.
    pub identity: Option<FileIdentity>,
    /// Tier C's open file, kept for page reads and finds.
    pub file: Option<Arc<OpenFile>>,
    pub pages: Option<PageCache>,
    pub lang: Option<&'static str>,
    /// The first screen opened at the file's start for a line target
    /// without checkpoints: the full read moves to the line.
    pub relocate: bool,
    pub find: FindState,
    pub watch: Option<watch::Watches>,
    pub watch_asked: bool,
    pub window_busy: bool,
    pub window_again: bool,
    pub pages_busy: bool,
    /// The edit session, while one lives: fs_watch events go to it instead
    /// of reloading the document.
    pub edit: Option<Weak<edit::Session>>,
}

#[derive(Default)]
pub(crate) struct FindState {
    pub generation: u64,
    pub cancel: Arc<AtomicBool>,
    pub query: Option<FindQuery>,
    pub results: Option<Arc<FindResults>>,
    /// Waiting for the full content.
    pub waiting: bool,
}

impl Doc {
    fn new(
        engine: &Arc<EngineInner>,
        target: ReadTarget,
        root: Arc<Path>,
        viewport: Viewport,
        notify: Arc<dyn Fn() + Send + Sync>,
    ) -> Doc {
        let editable = match target.origin {
            TextOrigin::Worktree => Editable::No(NotEditable::NotLoaded),
            TextOrigin::Object | TextOrigin::Dependency(_) => {
                Editable::No(NotEditable::ReadOnlySource)
            }
        };
        let st = DocMut {
            generation: 0,
            load: 0,
            at: target.at,
            cancel: Arc::new(AtomicBool::new(false)),
            viewport,
            top: None,
            state: DocState::Opening,
            size: 0,
            oid: None,
            tier: None,
            highlight: Highlighting::Pending,
            lines: LineCount::Estimated(0),
            widest: 0,
            editable,
            anchor: RowPos::default(),
            content: Content::Nothing,
            expect: target.expect,
            whole: None,
            whole_from: None,
            windows: Vec::new(),
            chain: None,
            identity: None,
            file: None,
            pages: None,
            lang: None,
            relocate: false,
            find: FindState::default(),
            watch: None,
            watch_asked: false,
            window_busy: false,
            window_again: false,
            pages_busy: false,
            edit: None,
        };
        let snapshot = Arc::new(st.snapshot(&target));
        Doc {
            engine: Arc::downgrade(engine),
            target,
            root,
            notify,
            snapshot: Mutex::new(snapshot),
            st: Mutex::new(st),
            closed: AtomicBool::new(false),
        }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, DocMut> {
        lock(&self.st)
    }

    /// The snapshot published last.
    #[cfg(test)]
    pub(crate) fn current(&self) -> Arc<DocSnapshot> {
        lock(&self.snapshot).clone()
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// The live edit session, if any.
    pub(crate) fn editing(&self) -> Option<Arc<edit::Session>> {
        self.lock()
            .edit
            .as_ref()
            .and_then(Weak::upgrade)
            .filter(|s| !s.is_closed())
    }

    /// What tells the view a new snapshot (or edit state) is there.
    pub(crate) fn notifier(&self) -> Arc<dyn Fn() + Send + Sync> {
        self.notify.clone()
    }

    /// Whether `load` is no longer the current one.
    pub(crate) fn stale(&self, st: &DocMut, load: u64) -> bool {
        self.is_closed() || st.load != load
    }

    /// Publishes a snapshot of `st`, then notifies the view.
    pub(crate) fn commit(&self, st: MutexGuard<'_, DocMut>) {
        let snapshot = Arc::new(st.snapshot(&self.target));
        *lock(&self.snapshot) = snapshot;
        drop(st);
        (self.notify)();
    }

    /// The handle dropped: cancels every task and hands what the document
    /// holds to a task that drops it.
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let garbage = {
            let mut st = self.lock();
            st.cancel.store(true, Ordering::SeqCst);
            st.find.cancel.store(true, Ordering::SeqCst);
            (
                st.file.take(),
                st.watch.take(),
                std::mem::take(&mut st.content),
                st.pages.take(),
                std::mem::take(&mut st.windows),
                st.whole.take(),
                st.chain.take(),
            )
        };
        match self.engine.upgrade() {
            Some(engine) => {
                // The window a batch task stopped in holds this content.
                let parked = chain::forget(&engine, self);
                let garbage = (garbage, parked);
                // The descriptor is closed off this thread when it can be.
                let _ = engine.spawn(Priority::Background, async move { drop(garbage) });
            }
            None => drop(garbage),
        }
    }

    /// The anchor a reload keeps: the line now at the top.
    pub(crate) fn current_line(&self) -> Anchor {
        let snapshot = lock(&self.snapshot).clone();
        let top = self.lock().top.unwrap_or(snapshot.anchor);
        let line = match snapshot.line_number(top.line_start) {
            super::LineNo::Exact(n) | super::LineNo::Estimated(n) => n,
        };
        Anchor::Line {
            line: u32::try_from(line).unwrap_or(u32::MAX),
            column: 0,
        }
    }
}

impl DocMut {
    pub(crate) fn snapshot(&self, target: &ReadTarget) -> DocSnapshot {
        let mut spans: Vec<Arc<WindowSpans>> = Vec::with_capacity(1 + self.windows.len());
        spans.extend(self.whole.clone());
        spans.extend(self.windows.iter().cloned());
        DocSnapshot {
            generation: self.generation,
            target: target.clone(),
            state: self.state.clone(),
            size: self.size,
            oid: self.oid,
            tier: self.tier,
            highlight: self.highlight,
            lines: self.lines,
            widest_cells: self.widest,
            editable: self.editable,
            anchor: self.anchor,
            find: self.find.results.clone(),
            content: self.content.clone(),
            spans: spans.into(),
        }
    }

    /// Moves the anchor, bumping the generation when it changes.
    pub(crate) fn set_anchor(&mut self, anchor: RowPos) {
        if anchor != self.anchor || self.generation == 0 {
            self.anchor = anchor;
            self.generation += 1;
        }
    }
}
