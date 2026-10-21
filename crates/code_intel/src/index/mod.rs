//! The indexer (docs/design/code_intel.md「blob → 路径映射」「输入代数与派生
//! 状态」「建索引的分层」「调度、批次与取消」「打开项目与热启动」「增量更新」
//! 「语言包的变化」): per open project, the blob → path map of the current
//! worktree, kept up to date from `ProjectEvent`s and the worktree's
//! `WorktreeBatch`es by one reconcile task, and the five tiers that extract
//! what the cache is missing.
//!
//! Events only bump input generations and request a reconcile; every
//! derived state is recomputed there, in dependency order, and installed
//! only if the generations it read are still current.

pub(crate) mod batch;
pub(crate) mod chain;
pub(crate) mod host;
mod reconcile;
pub(crate) mod state;
mod watch;

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use lectern_cache::Cache;
use lectern_executor::{Executor, Priority};
use lectern_fs_watch::Watcher;
use lectern_kernel::{ActivationToken, PluginHandle};
use lectern_services::code_intel::{
    CodeIntel, Coverage, Focus, Names, NavigationProvider, ProviderRegistration, Query, QueryId,
    QueryResults, TierState,
};
use lectern_services::project::{ProjectEvent, ProjectKey, Projects, WorktreeKey};

use crate::LangName;
use crate::extract::{Deadline, Extraction, Lang, LanguageTable, extract};
use crate::git_access::{GitAccess, LecternGit};
use crate::grammar::{GrammarChange, GrammarSource};
use crate::map::{HeadDiff, LiveMap};
use crate::store::Store;
use crate::watchdog::Watchdog;

pub(crate) use batch::Scanned;
pub(crate) use host::Host;
use state::{Project, State};

/// What the indexer is built from. `app` creates the executor, the cache
/// and the watcher once (ADR 0025); the plugin passes its handle and its
/// watchdog.
pub struct IndexDeps {
    pub executor: Executor,
    pub cache: Cache,
    pub watcher: Watcher,
    pub grammars: Arc<dyn GrammarSource>,
    pub projects: Arc<dyn Projects>,
    /// The activation the tasks belong to and take their guards from.
    pub handle: PluginHandle,
    pub watchdog: Watchdog,
    /// Long-running batch tasks, one per batch thread
    /// ([`default_batch_tasks`]).
    pub batch_tasks: usize,
}

/// The executor's default number of batch threads, `clamp(parallelism / 4,
/// 1, 3)` (ADR 0029): one batch task each.
pub fn default_batch_tasks() -> usize {
    let parallelism = std::thread::available_parallelism().map_or(1, |n| n.get());
    (parallelism / 4).clamp(1, 3)
}

/// The extraction the tiers call; tests count and steer it.
pub(crate) type ExtractFn =
    Arc<dyn Fn(&Lang, &[u8], &Deadline, &Arc<AtomicBool>) -> Extraction + Send + Sync>;

/// Counters of the recomputations, for the `derived_*` tests (「测试」's
/// counting hooks) and the benchmarks.
#[derive(Default, Debug)]
pub struct Stats {
    pub base_builds: AtomicUsize,
    pub compares: AtomicUsize,
    /// Paths read again for the overlay.
    pub rereads: AtomicUsize,
    pub cursor_builds: AtomicUsize,
    pub extractions: AtomicUsize,
    /// Existence-check calls (one per language and chunk of up to 500).
    pub exists_checks: AtomicUsize,
    pub reconciles: AtomicUsize,
    /// Passes over the reconcile's steps (each reconcile makes at least one).
    pub reconcile_passes: AtomicUsize,
    pub grammar_rounds: AtomicUsize,
    /// Jumps' steps 1–3 run (a prefetch reused runs none).
    pub prepares: AtomicUsize,
    /// Windows of opened large files extracted (「大文件」), widened ones
    /// counted once.
    pub windows: AtomicUsize,
    /// Units marked as used: the batch tasks' queued hits and the queries'
    /// units.
    pub touched: AtomicUsize,
}

/// A snapshot of [`Stats`].
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct StatCounts {
    pub base_builds: usize,
    pub compares: usize,
    pub rereads: usize,
    pub cursor_builds: usize,
    pub extractions: usize,
    pub exists_checks: usize,
    pub reconciles: usize,
    pub reconcile_passes: usize,
    pub grammar_rounds: usize,
    pub prepares: usize,
    pub windows: usize,
    pub touched: usize,
}

impl Stats {
    fn counts(&self) -> StatCounts {
        let get = |a: &AtomicUsize| a.load(Ordering::Acquire);
        StatCounts {
            base_builds: get(&self.base_builds),
            compares: get(&self.compares),
            rereads: get(&self.rereads),
            cursor_builds: get(&self.cursor_builds),
            extractions: get(&self.extractions),
            exists_checks: get(&self.exists_checks),
            reconciles: get(&self.reconciles),
            reconcile_passes: get(&self.reconcile_passes),
            grammar_rounds: get(&self.grammar_rounds),
            prepares: get(&self.prepares),
            windows: get(&self.windows),
            touched: get(&self.touched),
        }
    }
}

pub(crate) fn bump(counter: &AtomicUsize, by: usize) {
    counter.fetch_add(by, Ordering::AcqRel);
}

/// The current language table, and what the last grammar round saw.
pub(crate) struct Langs {
    pub table: LanguageTable,
    /// Grows by one whenever the table changes (the language-pack input).
    pub generation: u64,
    /// The newest `GrammarSource::generation` handled.
    pub handled: u64,
    /// Languages disabled for this run: their grammar or queries do not
    /// compile.
    pub disabled: HashSet<LangName>,
}

/// The grammar rounds: at most one at a time, notifications during one
/// merged into the next.
#[derive(Default)]
struct Rounds {
    running: bool,
    again: bool,
    langs: HashSet<LangName>,
}

#[derive(Default)]
pub(crate) struct BatchRun {
    pub running: usize,
    /// Work may have arrived since a running task last looked.
    kicked: bool,
    /// Tasks waiting at a yielding boundary: woken by every kick, so tier
    /// 3 work never waits for a keepalive slot (「交互让路」).
    pub(crate) waiters: Vec<std::task::Waker>,
    /// The most tasks running at once (tests).
    #[cfg(test)]
    pub peak: usize,
}

/// [`Inner::after_base`] (tests).
#[cfg(test)]
pub(crate) type AfterBase = Box<dyn FnOnce(&Inner) + Send>;

pub(crate) struct Inner {
    pub(crate) me: Weak<Inner>,
    pub host: Host,
    pub cache: Cache,
    pub watcher: Watcher,
    pub packs: Arc<dyn GrammarSource>,
    pub projects: Arc<dyn Projects>,
    pub watchdog: Watchdog,
    pub git: Arc<dyn GitAccess>,
    batch_tasks: usize,
    pub langs: Mutex<Langs>,
    open: Mutex<Vec<Arc<Project>>>,
    /// The project that got the latest `set_focus` (or else was opened
    /// last): only it runs tiers 3–5.
    pub(crate) batch_project: Mutex<Option<ProjectKey>>,
    /// Some `set_focus` happened: opening a project no longer picks it.
    focused_once: AtomicBool,
    pub(crate) batch: Mutex<BatchRun>,
    rounds: Mutex<Rounds>,
    pub extract: Mutex<ExtractFn>,
    /// One batch task at a time extracts a large file.
    pub(crate) large: Arc<batch::LargeSlot>,
    pub stats: Stats,
    next_session: AtomicUsize,
    /// The queries of this activation (`crate::query`).
    pub(crate) queries: Mutex<crate::query::Queries>,
    /// The parsed current texts kept for jumps (ADR 0037).
    pub(crate) parse_cache: Mutex<crate::query::ParseCache>,
    /// When the due entry that expires kept parses fires, if scheduled.
    pub(crate) parse_expiry: Mutex<Option<std::time::Instant>>,
    /// Held around replacing a query's results and emitting its event, so
    /// one query's events go out in generation order.
    pub(crate) publishing: Mutex<()>,
    /// The resolution extension point's implementations (none in #17: the
    /// empty rules answer for every language).
    pub(crate) resolvers: Mutex<Vec<Arc<dyn crate::resolve::Resolver>>>,
    /// Registered semantic providers (#26).
    pub(crate) providers: Mutex<crate::semantic::Providers>,
    pub(crate) replies: Mutex<crate::semantic::Replies>,
    pub(crate) next_request: std::sync::atomic::AtomicU64,
    /// A task compiling `ParsedFile`'s queries is submitted or running
    /// (`warm_site_queries`).
    pub(crate) warming: std::sync::atomic::AtomicBool,
    /// The files of each extraction batch of tiers 3–5, in order (tests).
    #[cfg(test)]
    pub batch_log: Mutex<Vec<usize>>,
    /// Run once by the next query that reaches its step (tests: the
    /// interleaving matrix injects a background change there).
    #[cfg(test)]
    pub step_hook: Mutex<Option<crate::query::StepHook>>,
    /// Run once by the next reconcile pass that rebuilt the base map, before
    /// the overlay is read (tests: a query snapshot in that window).
    #[cfg(test)]
    pub after_base: Mutex<Option<AfterBase>>,
    /// Tests: no text is warmed (`Inner::warm_parse`), so no kept parse's
    /// due entry is scheduled (tests that count the watchdog's entries).
    #[cfg(test)]
    pub no_warm: std::sync::atomic::AtomicBool,
    /// Tests: find references parses this path as if the parse took this
    /// long: past its deadline, it times out.
    #[cfg(test)]
    pub parse_takes: Mutex<Option<(String, std::time::Duration)>>,
    /// Tests: the full-mode statements the accountings' walks were allowed
    /// to make outside the scope (`Stops::due` asked).
    #[cfg(test)]
    pub verify_checks: std::sync::atomic::AtomicUsize,
    /// Another registration of `code_intel.tags` for new projects (tests).
    #[cfg(test)]
    pub tables: Mutex<Option<&'static lectern_cache::Tables>>,
    /// Run once by the next tier 1 or 2 task, before its existence check
    /// (tests: the batch tasks run, and stop, in that window).
    #[cfg(test)]
    pub before_focus_check: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Run at every yield check of a yielding extraction, before it asks
    /// the kernel (tests: the `yield_*` tests move the kernel's clock past
    /// the slot's deadline, or turn the switch on, there).
    #[cfg(test)]
    pub yield_hook: YieldHook,
    /// The wall clock the racy-timestamp rule compares a file's status
    /// change time with (「大文件」); `SystemTime::now` when unset (tests
    /// and benchmarks set it).
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) wall: Mutex<Option<WallClock>>,
    /// Run after every window a chain writes (tests: a pause, a change of
    /// the file, an eviction between two windows).
    #[cfg(test)]
    pub window_hook: Mutex<Option<WindowHook>>,
    /// Tests: a jump's window parse of a large text runs out of its 8 ms.
    #[cfg(test)]
    pub window_parse_times_out: AtomicBool,
}

/// [`Inner::window_hook`] (tests): given each written window's end.
#[cfg(test)]
pub(crate) type WindowHook = Box<dyn FnMut(u64) + Send>;

/// [`Inner::wall`].
#[cfg(any(test, feature = "test-support"))]
pub(crate) type WallClock = Arc<dyn Fn() -> std::time::SystemTime + Send + Sync>;

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// [`Inner::yield_hook`] (tests).
#[cfg(test)]
pub(crate) type YieldHook = Arc<Mutex<Option<Box<dyn FnMut() + Send>>>>;

/// The index of a plugin activation. Every method returns at once and may
/// be called on any thread; the work runs on the executor.
pub struct Indexer {
    inner: Arc<Inner>,
}

impl Indexer {
    /// No IO.
    pub fn new(deps: IndexDeps) -> Indexer {
        Indexer::with_git(deps, Arc::new(LecternGit::new()))
    }

    /// The same indexer over `testing::FakeGit`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_fake_git(deps: IndexDeps, git: Arc<crate::testing::FakeGit>) -> Indexer {
        Indexer::with_git(deps, git)
    }

    pub(crate) fn with_git(deps: IndexDeps, git: Arc<dyn GitAccess>) -> Indexer {
        let inner = Arc::new_cyclic(|me| Inner {
            me: me.clone(),
            host: Host::new(deps.handle, deps.executor),
            cache: deps.cache,
            watcher: deps.watcher,
            packs: deps.grammars,
            projects: deps.projects,
            watchdog: deps.watchdog,
            git,
            batch_tasks: deps.batch_tasks.max(1),
            langs: Mutex::new(Langs {
                table: LanguageTable::core(),
                generation: 0,
                handled: 0,
                disabled: HashSet::new(),
            }),
            open: Mutex::new(Vec::new()),
            batch_project: Mutex::new(None),
            focused_once: AtomicBool::new(false),
            batch: Mutex::new(BatchRun::default()),
            rounds: Mutex::new(Rounds::default()),
            extract: Mutex::new(Arc::new(extract)),
            large: Arc::default(),
            stats: Stats::default(),
            next_session: AtomicUsize::new(1),
            queries: Mutex::new(crate::query::Queries::default()),
            parse_cache: Mutex::new(crate::query::ParseCache::default()),
            parse_expiry: Mutex::new(None),
            publishing: Mutex::new(()),
            resolvers: Mutex::new(Vec::new()),
            providers: Mutex::new(crate::semantic::Providers::default()),
            replies: Mutex::new(crate::semantic::Replies::default()),
            next_request: std::sync::atomic::AtomicU64::new(0),
            warming: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            batch_log: Mutex::new(Vec::new()),
            #[cfg(test)]
            step_hook: Mutex::new(None),
            #[cfg(test)]
            after_base: Mutex::new(None),
            #[cfg(test)]
            no_warm: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            parse_takes: Mutex::new(None),
            #[cfg(test)]
            verify_checks: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            tables: Mutex::new(None),
            #[cfg(test)]
            before_focus_check: Mutex::new(None),
            #[cfg(test)]
            yield_hook: Arc::default(),
            #[cfg(any(test, feature = "test-support"))]
            wall: Mutex::new(None),
            #[cfg(test)]
            window_hook: Mutex::new(None),
            #[cfg(test)]
            window_parse_times_out: AtomicBool::new(false),
        });
        Indexer { inner }
    }

    /// Replaces the wall clock of the racy-timestamp rule (「大文件」):
    /// tests put files' status changes in the past or the present, and
    /// benchmarks open a file just written as one written long ago.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_wall_clock(&self, f: impl Fn() -> std::time::SystemTime + Send + Sync + 'static) {
        *lock(&self.inner.wall) = Some(Arc::new(f));
    }

    /// Subscribes to the language packs; the plugin hands the returned
    /// values to `PluginHandle::own`, so they are dropped (unsubscribed) on
    /// deactivation. Picks up packs already installed.
    pub fn subscribe(&self) -> Vec<Box<dyn Send>> {
        let grammars = {
            let me = Arc::downgrade(&self.inner);
            self.inner.packs.subscribe(Box::new(move |change| {
                if let Some(inner) = me.upgrade() {
                    inner.grammar_changed(Some(change));
                }
            }))
        };
        if self.inner.packs.generation() > 0 || !self.inner.packs.installed().is_empty() {
            self.inner.grammar_changed(None);
        }
        vec![grammars]
    }

    /// Handles a `ProjectEvent`: only bumps inputs and requests a reconcile.
    pub fn project_event(&self, event: &ProjectEvent) {
        self.inner.project_event(event);
    }

    /// Starts on the projects already open (`started`).
    pub fn open_existing(&self) {
        for key in self.inner.projects.open_projects() {
            self.inner.touch(&key);
        }
    }

    /// `CodeIntel::set_focus`: replaces one kind of focus. Tiers 1 and 2
    /// index it; it picks the project that runs tiers 3–5 and the anchor
    /// of tier 3.
    pub fn set_focus(&self, project: &ProjectKey, worktree: Option<&WorktreeKey>, focus: Focus) {
        self.inner.set_focus(project, worktree, focus);
    }

    /// The language packs changed (`GrammarSource::subscribe`).
    pub fn grammar_changed(&self, change: GrammarChange) {
        self.inner.grammar_changed(Some(change));
    }

    /// `CodeIntel::coverage`: memory only, never blocks.
    pub fn coverage(&self, project: &ProjectKey) -> Option<Coverage> {
        self.inner.coverage(project)
    }

    /// The current map of a project: one consistent view (for queries).
    pub fn map(&self, project: &ProjectKey) -> Option<LiveMap> {
        let p = self.inner.project(project)?;
        Some(lock(&p.state).map.clone())
    }

    /// The last worktree comparison's HEAD difference.
    pub fn head_diff(&self, project: &ProjectKey) -> Option<Arc<HeadDiff>> {
        let p = self.inner.project(project)?;
        Some(lock(&p.state).head_diff.clone())
    }

    /// Whether a path's current content equals HEAD (「依赖 git、fs_watch 与
    /// cache 的接口」): its OID in HEAD is known and equals its effective
    /// OID, and the last comparison saw the current HEAD, index and file
    /// list. Otherwise false, until the reconcile compares again.
    pub fn head_clean(&self, project: &ProjectKey, path: &str) -> bool {
        self.inner.head_clean(project, path)
    }

    /// The project's store, for queries and tests.
    pub fn store(&self, project: &ProjectKey) -> Option<Store> {
        Some(self.inner.project(project)?.store.clone())
    }

    /// Counters of recomputations so far.
    pub fn stats(&self) -> StatCounts {
        self.inner.stats.counts()
    }

    /// Marks the project's namespace as full, as the batch layer does when
    /// its own write evicts units (`Batch::full`): the hints' stamp freezes
    /// at the walks' generation (tests and the full-mode profile).
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_namespace_full(&self, project: &ProjectKey) {
        let Some(p) = self.inner.project(project) else {
            return;
        };
        let eviction = p.evictions();
        let mut st = lock(&p.state);
        let walked = st.walk_eviction(eviction);
        st.batch.full = Some(walked);
    }

    /// Forgets the project's cached walks (`Project::walks`), so the next
    /// query walks every file, as the first one after a change does (the
    /// profiles of the first jump).
    #[cfg(any(test, feature = "test-support"))]
    pub fn clear_walks(&self, project: &ProjectKey) {
        if let Some(p) = self.inner.project(project) {
            lock(&p.walks).clear();
        }
    }

    /// Another handle to the same activation (benchmarks: an event listener
    /// reads the results it is told about).
    #[cfg(any(test, feature = "test-support"))]
    pub fn handle(&self) -> Indexer {
        Indexer {
            inner: self.inner.clone(),
        }
    }

    /// The kept parses of current texts (ADR 0037): how many, and their
    /// estimated bytes (tests and benchmarks).
    #[cfg(any(test, feature = "test-support"))]
    pub fn kept_parses(&self) -> (usize, usize) {
        let cache = lock(&self.inner.parse_cache);
        (cache.len(), cache.bytes())
    }

    /// Forgets every kept parse, so the next jump parses its text, as the
    /// first one in a text does (benchmarks of the cold path).
    #[cfg(any(test, feature = "test-support"))]
    pub fn clear_kept_parses(&self) {
        let dropped = lock(&self.inner.parse_cache).without_all();
        drop(dropped);
    }

    /// Forgets the large texts' recorded identities and checkpoints
    /// (「大文件」), so the next jump in one streams it (benchmarks of the
    /// cold path).
    #[cfg(any(test, feature = "test-support"))]
    pub fn clear_large_texts(&self) {
        lock(&self.inner.parse_cache).without_large();
    }

    /// The number of large texts whose identity and checkpoints are kept.
    #[cfg(any(test, feature = "test-support"))]
    pub fn kept_large_texts(&self) -> usize {
        lock(&self.inner.parse_cache).large_len()
    }

    /// The number of the project's cached walks (tests).
    #[cfg(test)]
    pub(crate) fn walks(&self, project: &ProjectKey) -> usize {
        self.inner
            .project(project)
            .map_or(0, |p| lock(&p.walks).len())
    }

    /// Replaces the extraction the tiers call (tests count, block or steer
    /// it).
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_extractor(
        &self,
        f: impl Fn(&Lang, &[u8], &Deadline, &Arc<AtomicBool>) -> Extraction + Send + Sync + 'static,
    ) {
        *lock(&self.inner.extract) = Arc::new(f);
    }

    /// The input generations of a project, for tests.
    #[cfg(test)]
    pub(crate) fn gens(&self, project: &ProjectKey) -> Option<state::Gens> {
        Some(lock(&self.inner.project(project)?.state).gens)
    }

    /// The stamp a hint of the current map matches (its provenance and the
    /// walks' eviction generation), for tests.
    #[cfg(test)]
    pub(crate) fn stamp(&self, project: &ProjectKey) -> Option<crate::map::HintStamp> {
        let p = self.inner.project(project)?;
        let eviction = p.evictions();
        Some(lock(&p.state).stamp(eviction))
    }

    #[cfg(test)]
    pub(crate) fn inner(&self) -> &Arc<Inner> {
        &self.inner
    }

    /// Tier 3's scope in the current cursor ("whole", "empty" or the
    /// directory), for tests.
    #[cfg(test)]
    pub(crate) fn cursor_scope(&self, project: &ProjectKey) -> Option<String> {
        let p = self.inner.project(project)?;
        let st = lock(&p.state);
        let cursor = st.batch.cursor.as_ref()?;
        Some(match &cursor.scope {
            state::Scope::Whole => "whole".into(),
            state::Scope::Empty => "empty".into(),
            state::Scope::Dir(d) => d.to_string(),
        })
    }

    /// Whether the last comparison is current, for tests.
    #[cfg(test)]
    pub(crate) fn compared_current(&self, project: &ProjectKey) -> bool {
        self.inner.project(project).is_some_and(|p| {
            let grammar = self.inner.grammar();
            let st = lock(&p.state);
            st.compare_for == Some(st.gens.compare_deps(grammar))
        })
    }

    /// `CodeIntel::query`: returns at once; the results come in batches.
    pub fn query(&self, query: Query) -> QueryId {
        self.inner.query(query)
    }

    /// `CodeIntel::results`: one lock, one `Arc` clone.
    pub fn results(&self, id: QueryId) -> Option<Arc<QueryResults>> {
        self.inner.results(id)
    }

    /// `CodeIntel::close`.
    pub fn close(&self, id: QueryId) {
        self.inner.close(id);
    }

    /// `CodeIntel::prefetch`.
    pub fn prefetch(&self, query: Query) {
        self.inner.prefetch(query);
    }

    /// `CodeIntel::names`.
    pub fn names(&self, project: &ProjectKey) -> Names {
        self.inner.names(project)
    }

    /// `CodeIntel::register_provider`.
    pub fn register_provider(
        &self,
        provider: Arc<dyn NavigationProvider>,
        token: ActivationToken,
    ) -> ProviderRegistration {
        self.inner.register_provider(provider, token)
    }

    /// Installs a resolver (tests of the extension point).
    #[cfg(test)]
    pub(crate) fn set_resolver(&self, resolver: Arc<dyn crate::resolve::Resolver>) {
        lock(&self.inner.resolvers).push(resolver);
    }

    /// Ends every project's session: tasks stop, extractions in flight are
    /// cancelled, subscriptions to fs_watch dropped, every query closed (its
    /// unanswered semantic requests cancelled).
    pub fn close_all(&self) {
        let projects = std::mem::take(&mut *lock(&self.inner.open));
        for p in projects {
            lock(&p.state).end();
        }
        let ended = self.inner.close_queries(None);
        self.inner.finish_ended(ended);
    }
}

impl Drop for Indexer {
    fn drop(&mut self) {
        self.close_all();
    }
}

impl CodeIntel for Indexer {
    fn set_focus(&self, project: &ProjectKey, worktree: Option<&WorktreeKey>, focus: Focus) {
        Indexer::set_focus(self, project, worktree, focus);
    }

    fn query(&self, query: Query) -> QueryId {
        Indexer::query(self, query)
    }

    fn results(&self, id: QueryId) -> Option<Arc<QueryResults>> {
        Indexer::results(self, id)
    }

    fn close(&self, id: QueryId) {
        Indexer::close(self, id);
    }

    fn prefetch(&self, query: Query) {
        Indexer::prefetch(self, query);
    }

    fn names(&self, project: &ProjectKey) -> Names {
        Indexer::names(self, project)
    }

    fn register_provider(
        &self,
        provider: Arc<dyn NavigationProvider>,
        token: ActivationToken,
    ) -> ProviderRegistration {
        Indexer::register_provider(self, provider, token)
    }

    fn coverage(&self, project: &ProjectKey) -> Option<Coverage> {
        Indexer::coverage(self, project)
    }
}

impl Inner {
    pub(crate) fn arc(&self) -> Option<Arc<Inner>> {
        self.me.upgrade()
    }

    /// The wall clock (`Inner::wall`).
    pub(crate) fn wall_now(&self) -> std::time::SystemTime {
        #[cfg(any(test, feature = "test-support"))]
        if let Some(f) = lock(&self.wall).as_ref() {
            return f();
        }
        std::time::SystemTime::now()
    }

    /// Lock order: `batch_project`, then `open`, then a project's state;
    /// `open` is released before a state is locked.
    pub(crate) fn project(&self, key: &ProjectKey) -> Option<Arc<Project>> {
        lock(&self.open).iter().find(|p| &p.key == key).cloned()
    }

    pub(crate) fn is_open(&self, project: &Arc<Project>) -> bool {
        lock(&self.open).iter().any(|p| Arc::ptr_eq(p, project))
    }

    pub(crate) fn head_clean(&self, key: &ProjectKey, path: &str) -> bool {
        let grammar = self.grammar();
        self.project(key)
            .is_some_and(|p| reconcile::head_clean(&lock(&p.state), path, grammar))
    }

    /// Bumps one input generation and requests a reconcile, as an event
    /// would (tests that change an input while a reconcile computes).
    #[cfg(test)]
    pub(crate) fn bump(&self, key: &ProjectKey, f: impl FnOnce(&mut state::Gens)) {
        let Some(p) = self.project(key) else {
            return;
        };
        f(&mut lock(&p.state).gens);
        if let Some(me) = self.arc() {
            me.request_reconcile(&p);
        }
    }

    #[cfg(not(test))]
    fn store_for(&self, partition: &lectern_cache::Partition) -> Store {
        Store::new(partition)
    }

    /// Tests may register `code_intel.tags` with a smaller quota.
    #[cfg(test)]
    fn store_for(&self, partition: &lectern_cache::Partition) -> Store {
        match *lock(&self.tables) {
            Some(tables) => Store::with_tables(partition, tables),
            None => Store::new(partition),
        }
    }

    /// Runs a reconcile of the project on this thread now (tests that
    /// change the session while an extraction is in flight).
    #[cfg(test)]
    pub(crate) fn reconcile_now(self: &Arc<Self>, key: &ProjectKey) {
        if let Some(p) = self.project(key) {
            lock(&p.state).run.running = true;
            self.clone().reconcile(p);
        }
    }

    fn new_session(&self) -> u64 {
        self.next_session.fetch_add(1, Ordering::AcqRel) as u64
    }

    /// The project, opened if it was not, and a reconcile requested.
    fn touch(&self, key: &ProjectKey) -> Arc<Project> {
        let (project, created) = {
            let mut open = lock(&self.open);
            match open.iter().find(|p| &p.key == key) {
                Some(p) => (p.clone(), false),
                None => {
                    let partition = match key {
                        ProjectKey::Git { common_dir } => self.cache.repo(common_dir),
                        ProjectKey::Folder { root } => self.cache.folder(root),
                    };
                    let (table, grammar) = {
                        let langs = lock(&self.langs);
                        (langs.table.clone(), langs.generation)
                    };
                    let p = Arc::new(Project {
                        key: key.clone(),
                        store: self.store_for(&partition),
                        commits: Mutex::new(()),
                        replaced: std::sync::atomic::AtomicU64::new(0),
                        replacing: std::sync::atomic::AtomicU64::new(0),
                        steady: std::sync::atomic::AtomicU64::new(0),
                        state: Mutex::new(State::new(self.new_session(), &table, grammar)),
                        names: Mutex::new(crate::names::NameState::default()),
                        walks: Mutex::new(Vec::new()),
                    });
                    open.push(p.clone());
                    (p, true)
                }
            }
        };
        // Until the first `set_focus`, the project opened last runs tiers 3–5.
        if created && !self.focused_once.load(Ordering::Acquire) {
            *lock(&self.batch_project) = Some(key.clone());
        }
        if let Some(me) = self.arc() {
            me.request_reconcile(&project);
        }
        project
    }

    pub(crate) fn project_event(&self, event: &ProjectEvent) {
        match event {
            ProjectEvent::Opened {
                result: Ok(key), ..
            } => {
                self.touch(key);
            }
            ProjectEvent::Opened { result: Err(_), .. } => {}
            ProjectEvent::Changed { project, parts, .. } => {
                let p = self.project(project).unwrap_or_else(|| self.touch(project));
                // A new current worktree ends the old session here, not in
                // the reconcile: from now on no query reads the new
                // worktree's files against the old map, and nothing of the
                // old session is published.
                if parts.active
                    && let Some(snapshot) = self.projects.snapshot(project)
                {
                    self.switch_session(&p, &snapshot);
                }
                {
                    let mut st = lock(&p.state);
                    // The worktree list or a HEAD changed: tier 5 is not
                    // walked again for that (an index change of one other
                    // worktree arrives in a `RepoBatch`, and only that
                    // worktree is walked again); a new worktree is not in
                    // `done` yet.
                    if parts.worktrees {
                        st.check_head = true;
                    }
                    if parts.files {
                        st.check_index = true;
                    }
                    // A failed watch (or root) is tried again on the next
                    // event; a folder project has no watch.
                    if st.watches.is_none() && matches!(project, ProjectKey::Git { .. }) {
                        st.root = None;
                        st.root_tried = false;
                    }
                }
                if let Some(me) = self.arc() {
                    me.request_reconcile(&p);
                }
            }
            ProjectEvent::Closed { project } => {
                let removed = {
                    let mut open = lock(&self.open);
                    let at = open.iter().position(|p| &p.key == project);
                    at.map(|i| open.remove(i))
                };
                let ended = {
                    // Ordered against publication: nothing of the session
                    // is published after this.
                    let _order = lock(&self.publishing);
                    if let Some(p) = &removed {
                        lock(&p.state).end();
                    }
                    self.close_queries(Some(project))
                };
                // Their providers' `cancel`, outside the lock.
                self.finish_ended(ended);
                // Its current text is no longer current.
                if let Some(me) = self.arc() {
                    me.focus_parse(project, None);
                }
                {
                    let mut batch = lock(&self.batch_project);
                    if batch.as_ref() == Some(project) {
                        *batch = lock(&self.open).last().map(|p| p.key.clone());
                    }
                }
                // The project that runs tiers 3–5 now may have work left.
                if let Some(me) = self.arc() {
                    me.kick_batch();
                }
            }
        }
    }

    fn set_focus(&self, key: &ProjectKey, worktree: Option<&WorktreeKey>, focus: Focus) {
        let Some(project) = self.project(key) else {
            return;
        };
        // A text that becomes the current one: its parse is kept for the
        // jumps in it while it is current (ADR 0037), made in the background.
        let warm = match &focus {
            Focus::Current(Some(text)) => Some(text.clone()),
            _ => None,
        };
        let current = match &focus {
            Focus::Current(text) => Some(text.as_ref().map(|t| t.oid)),
            _ => None,
        };
        {
            let mut st = lock(&project.state);
            let wt = worktree.cloned();
            match focus {
                Focus::Current(text) => st.focus.current = text.map(|t| (wt, t)),
                Focus::Drawer(paths) => st.focus.drawer = Some((wt, paths)),
                Focus::Review(paths) => st.focus.review = Some((wt, paths)),
                Focus::Touched(paths) => st.focus.touched = Some((wt, paths)),
            }
            st.gens.focus += 1;
        }
        self.focused_once.store(true, Ordering::Release);
        *lock(&self.batch_project) = Some(key.clone());
        if let Some(me) = self.arc() {
            me.enqueue_focus(&project);
            me.kick_batch();
            if let Some(oid) = current {
                me.focus_parse(key, oid);
            }
            if let Some(text) = warm {
                me.warm_parse(key, &text);
            }
        }
    }

    /// Queues a grammar round; `change` None asks for one without a
    /// notification (packs installed before the subscription).
    fn grammar_changed(&self, change: Option<GrammarChange>) {
        {
            let mut rounds = lock(&self.rounds);
            if let Some(change) = change {
                if change.generation <= lock(&self.langs).handled {
                    return;
                }
                rounds.langs.insert(change.lang);
            }
            if rounds.running {
                rounds.again = true;
                return;
            }
            rounds.running = true;
        }
        let Some(me) = self.arc() else {
            return;
        };
        let spawned = self
            .host
            .spawn_busy("grammar", Priority::Background, move || me.grammar_round());
        if !spawned {
            lock(&self.rounds).running = false;
        }
    }

    /// One grammar round (「语言包的变化」): reads the packs' current state,
    /// compares it with the table and updates the table; the language
    /// generation then makes the map, the hints and the cursors follow.
    pub(crate) fn grammar_round(self: Arc<Self>) {
        loop {
            bump(&self.stats.grammar_rounds, 1);
            let (asked, mut table, handled) = {
                let mut rounds = lock(&self.rounds);
                // This round reads the current state: notifications so far
                // are merged into it.
                rounds.again = false;
                let asked = std::mem::take(&mut rounds.langs);
                let langs = lock(&self.langs);
                (asked, langs.table.clone(), langs.handled)
            };
            let generation = self.packs.generation();
            let mut names: Vec<LangName> = table
                .iter()
                .filter(|l| !l.lease().is_static())
                .map(|l| l.name().clone())
                .chain(asked)
                // Packs installed before activation, or whose notification
                // was missed: a round reads the whole installed set.
                .chain(self.packs.installed())
                .collect();
            names.sort_by(|a, b| a.0.cmp(&b.0));
            names.dedup();
            let mut changed = Vec::new();
            for name in names {
                let current = table.get(&name.0).cloned();
                match (self.packs.grammar(&name), current) {
                    (Some(grammar), current) => {
                        let mut version = [0u8; 8];
                        version.copy_from_slice(&grammar.artifact_sha256[..8]);
                        let version = u64::from_be_bytes(version);
                        let same = current.as_ref().is_some_and(|l| {
                            l.grammar_version() == version && !l.lease().is_static()
                        });
                        if !same && current.as_ref().is_none_or(|l| !l.lease().is_static()) {
                            table = table.with_pack(name.clone(), grammar);
                            changed.push(name);
                        }
                    }
                    (None, Some(current)) if !current.lease().is_static() => {
                        table = table.without(&name.0);
                        changed.push(name);
                    }
                    (None, _) => {}
                }
            }
            // The table and its generation change while `open` is held
            // (`open`, then `langs`, as in `touch`): a project opened
            // concurrently is either in `projects` or starts with the new
            // table.
            let projects: Vec<Arc<Project>> = {
                let open = lock(&self.open);
                let mut langs = lock(&self.langs);
                langs.handled = langs.handled.max(generation).max(handled);
                if !changed.is_empty() {
                    langs.table = table;
                    langs.generation += 1;
                }
                open.clone()
            };
            if !changed.is_empty() {
                // Kept parses of a changed language hold its old grammar's
                // lease: dropped now (here, a background task), not when
                // they expire.
                let dropped = lock(&self.parse_cache).without(&changed);
                drop(dropped);
                for p in &projects {
                    let st = lock(&p.state);
                    // In-flight extractions of a changed language give up;
                    // their results would be dropped before the write anyway.
                    for ((_, lang), owner) in &st.in_flight {
                        if changed.contains(lang) {
                            owner.flag.store(true, Ordering::Release);
                        }
                    }
                }
                for p in &projects {
                    self.request_reconcile(p);
                }
            }
            let mut rounds = lock(&self.rounds);
            if rounds.again {
                rounds.again = false;
                continue;
            }
            rounds.running = false;
            return;
        }
    }

    fn coverage(&self, key: &ProjectKey) -> Option<Coverage> {
        let project = self.project(key)?;
        let eviction = project.evictions();
        let grammar = self.grammar();
        let st = lock(&project.state);
        let focus = if st.focused.queue.iter().all(|q| q.is_empty())
            && st.focused.running.iter().all(|r| *r == 0)
        {
            TierState::Done
        } else {
            TierState::Pending
        };
        // The walks are of the current map (built from the current file
        // list and grammar, with no path left to read and, for a git
        // project, its comparison current) with the current scope.
        let git = matches!(key, ProjectKey::Git { .. });
        let read = st.dirty.is_empty() && st.dirty_dirs.is_empty() && st.probe.is_empty();
        let compared = !git || st.compare_for == Some(st.gens.compare_deps(grammar));
        let current = read
            && compared
            && st.base_for == Some((st.gens.files, grammar))
            && st.batch.cursor.as_ref().is_some_and(|c| {
                c.deps == st.cursor_deps(eviction) && c.scope == batch::subproject(&st)
            });
        let tier = |t: usize| {
            let walked = current && st.batch.cursor.as_ref().is_some_and(|c| c.walks[t].done);
            if walked
                && st.batch.resume[t].is_empty()
                && st.batch.aside[t].is_empty()
                && st.batch.active[t] == 0
            {
                TierState::Done
            } else {
                TierState::Pending
            }
        };
        let others = {
            let o = &st.batch.others;
            let all_done = st.snapshot.as_ref().is_some_and(|s| {
                let active = st.worktree();
                s.worktrees
                    .iter()
                    .filter(|w| !w.prunable && Some(&w.key) != active)
                    .all(|w| o.done.contains(&w.key))
            });
            let stamp = st.stamp(eviction);
            let deps = Some((st.session, stamp.eviction, stamp.grammar));
            if o.deps == deps
                && all_done
                && o.current.is_none()
                && st.batch.resume[2].is_empty()
                && st.batch.aside[2].is_empty()
                && st.batch.active[2] == 0
            {
                TierState::Done
            } else {
                TierState::Pending
            }
        };
        Some(Coverage {
            generation: st.map.provenance.generation(),
            focus,
            subproject: tier(0),
            worktree: tier(1),
            other_worktrees: others,
        })
    }
}
