//! The engine (one per process) and the open documents' shared state.
//!
//! A document's tasks are numbered by load: opening, and every reload after
//! an outside change, starts a new load and cancels the old one's tasks
//! (their results are dropped when they land). Every task goes through the
//! plugin's handle (`own_with`) holding a `BusyGuard`.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use lectern_cache::{Cache, Partition};
use lectern_executor::{Executor, Priority};
use lectern_fs_watch::Watcher;
use lectern_git::HashKind;
use lectern_kernel::{BusyGuard, PluginHandle};
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

use super::content::{Content, PageCache};
use super::{
    DocSnapshot, DocState, Editable, Highlighting, LineCount, NotEditable, RowPos, Tier, Viewport,
    load, watch,
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
    /// Runs once right after the next first screen is published (tests).
    #[cfg(test)]
    pub after_first: Mutex<Option<Hook>>,
}

#[cfg(test)]
pub(crate) type Hook = Box<dyn FnOnce() + Send>;

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
                #[cfg(test)]
                after_first: Mutex::new(None),
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

    /// Registers object database content someone holds (the review's diff
    /// sides) until the returned handle drops: opening that OID's `Object`
    /// target then reads no object. Never blocks.
    pub fn share_object(&self, oid: BlobId, bytes: Arc<[u8]>) -> ObjectShare {
        self.inner.shares.register(oid, bytes)
    }

    /// The project closed: drops its roots, its repository handle and its
    /// documents' subscriptions. Never blocks.
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
            .filter(|d| &d.target.project == project)
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

    /// The first screen of `target` on the calling thread, as `open`'s
    /// first task computes it, with no task after it: for the benchmarks.
    pub fn first_screen(
        &self,
        target: ReadTarget,
        root: Arc<Path>,
        viewport: Viewport,
    ) -> Arc<DocSnapshot> {
        let doc = Doc::new(&self.inner, target, root, viewport, Arc::new(|| {}));
        let cancel = AtomicBool::new(false);
        let first = load::first_screen(
            &self.inner,
            &doc.target,
            &doc.root,
            doc.target.at,
            viewport.rows,
            &cancel,
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
}

#[cfg(test)]
impl DocHandle {
    pub(crate) fn is_watched(&self) -> bool {
        self.doc.lock().watch.is_some()
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
    /// The whole file's spans (tier A), exact.
    pub whole: Option<Arc<WindowSpans>>,
    /// The first screen's and the visible windows' spans, newest first.
    pub windows: Vec<Arc<WindowSpans>>,
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
            TextOrigin::Object => Editable::No(NotEditable::ReadOnlySource),
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
            whole: None,
            windows: Vec::new(),
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

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
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
            )
        };
        match self.engine.upgrade() {
            Some(engine) => {
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
