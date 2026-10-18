//! The `code-intel` plugin (docs/design/code_intel.md「本 crate 的公开项」):
//! provides `dyn CodeIntel`, requires `dyn Projects`, activates after the
//! first frame.
//!
//! - `activate` provides the service and subscribes to `ProjectEvent`; no
//!   IO, no grammar loaded, no task.
//! - `started` builds the index on the activation's handle, starts the
//!   projects already open and hands the power and language pack
//!   subscriptions and the watchdog's stop guard to `PluginHandle::own`.
//! - The watchdog's thread starts on its first registration, through
//!   `own_with` and `run_dedicated_gated`, with its `stop` registered in
//!   `Executor::on_shutdown`: every deactivation (and the executor's
//!   shutdown) ends it.
//! - Deactivation drops the guards and the tasks; `Active`'s drop also
//!   stops the watchdog and closes every project and query.

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use lectern_cache::Cache;
use lectern_executor::Executor;
use lectern_fs_watch::Watcher;
use lectern_kernel::{
    Activation, Active, ActivationToken, Context, Manifest, Plugin, PluginError, PluginHandle,
    ServiceRef, service,
};
use lectern_services::code_intel::{
    CodeIntel, Coverage, Focus, NavigationProvider, Names, ProviderRegistration, Query,
    QueryError, QueryId, QueryResults, SemanticState,
};
use lectern_services::project::{
    Location, OpenRequest, PackageMatch, ProjectEvent, ProjectKey, ProjectSnapshot, Projects,
    ResolveError, Resolved, ScanError, WorktreeKey,
};

use crate::git_access::{GitAccess, LecternGit};
use crate::grammar::GrammarSource;
use crate::index::{IndexDeps, Indexer, default_batch_tasks};
use crate::power::PowerSource;
use crate::watchdog::{Clock, SystemClock, WATCHDOG_THREAD, Watchdog};

/// What `app` creates once and hands to the plugin (ADR 0025).
pub struct CodeIntelDeps {
    pub executor: Executor,
    pub cache: Cache,
    pub watcher: Watcher,
    /// The power source: `app` wraps `platform`'s; `AlwaysOnAc` on Linux.
    pub power: Arc<dyn PowerSource>,
    /// Language packs (ADR 0018); `NoGrammarPacks` until the marketplace.
    pub grammars: Arc<dyn GrammarSource>,
}

/// The plugin (ID "code-intel"), put in the `PluginSet` by `app`.
pub struct CodeIntelPlugin {
    deps: Arc<CodeIntelDeps>,
    git: Arc<dyn GitAccess>,
    clock: Arc<dyn Clock>,
    batch_tasks: usize,
    /// The latest activation's service (tests).
    last: Arc<Mutex<Option<Arc<CodeIntelService>>>>,
}

impl CodeIntelPlugin {
    /// No IO.
    pub fn new(deps: CodeIntelDeps) -> CodeIntelPlugin {
        CodeIntelPlugin::with_git(deps, Arc::new(LecternGit::new()))
    }

    /// The same plugin over `testing::FakeGit`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_fake_git(deps: CodeIntelDeps, git: Arc<crate::testing::FakeGit>) -> CodeIntelPlugin {
        CodeIntelPlugin::with_git(deps, git)
    }

    fn with_git(deps: CodeIntelDeps, git: Arc<dyn GitAccess>) -> CodeIntelPlugin {
        CodeIntelPlugin {
            deps: Arc::new(deps),
            git,
            clock: Arc::new(SystemClock),
            batch_tasks: default_batch_tasks(),
            last: Arc::default(),
        }
    }

    /// Deadlines and the search's time box on `clock` (tests).
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> CodeIntelPlugin {
        self.clock = clock;
        self
    }

    /// This many batch tasks (tests: one, on a manual executor).
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_batch_tasks(mut self, n: usize) -> CodeIntelPlugin {
        self.batch_tasks = n.max(1);
        self
    }

    /// The service of the latest activation (tests and benchmarks).
    #[cfg(any(test, feature = "test-support"))]
    pub fn service(&self) -> Option<Arc<CodeIntelService>> {
        self.last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Plugin for CodeIntelPlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: vec![service::<dyn CodeIntel>()],
            requires: vec![service::<dyn Projects>()],
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    /// An empty project table, the service, the `ProjectEvent` listener. No
    /// IO, no grammar.
    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        let projects: Arc<dyn Projects> = Arc::new(ProjectsRef(ctx.service::<dyn Projects>()));
        let service = Arc::new(CodeIntelService {
            indexer: OnceLock::new(),
            watchdog: OnceLock::new(),
        });
        ctx.provide::<dyn CodeIntel>(service.clone())?;
        let listener = service.clone();
        ctx.on::<ProjectEvent>(move |event, _| {
            if let Some(indexer) = listener.indexer.get() {
                indexer.project_event(event);
            }
        });
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) = Some(service.clone());
        Ok(Box::new(CodeIntelActive {
            service,
            deps: self.deps.clone(),
            git: self.git.clone(),
            clock: self.clock.clone(),
            batch_tasks: self.batch_tasks,
            projects,
        }))
    }
}

/// One activation.
struct CodeIntelActive {
    service: Arc<CodeIntelService>,
    deps: Arc<CodeIntelDeps>,
    git: Arc<dyn GitAccess>,
    clock: Arc<dyn Clock>,
    batch_tasks: usize,
    projects: Arc<dyn Projects>,
}

impl Active for CodeIntelActive {
    fn started(&mut self, handle: PluginHandle) {
        let watchdog = watchdog(&handle, &self.deps.executor, self.clock.clone());
        handle.own(watchdog.stop_guard());
        let indexer = Indexer::with_git(
            IndexDeps {
                executor: self.deps.executor.clone(),
                cache: self.deps.cache.clone(),
                watcher: self.deps.watcher.clone(),
                power: self.deps.power.clone(),
                grammars: self.deps.grammars.clone(),
                projects: self.projects.clone(),
                handle: handle.clone(),
                watchdog: watchdog.clone(),
                batch_tasks: self.batch_tasks,
            },
            self.git.clone(),
        );
        for subscription in indexer.subscribe() {
            handle.own(subscription);
        }
        let _ = self.service.watchdog.set(watchdog);
        if self.service.indexer.set(indexer).is_ok()
            && let Some(indexer) = self.service.indexer.get()
        {
            indexer.open_existing();
        }
    }
}

impl Drop for CodeIntelActive {
    fn drop(&mut self) {
        if let Some(watchdog) = self.service.watchdog.get() {
            watchdog.stop();
        }
        if let Some(indexer) = self.service.indexer.get() {
            indexer.close_all();
        }
    }
}

/// This activation's watchdog: its thread runs on the executor's dedicated
/// thread [`WATCHDOG_THREAD`], submitted through `own_with` (so a dead
/// activation starts none), with its `stop` registered in
/// `Executor::on_shutdown` until the thread exits.
fn watchdog(handle: &PluginHandle, executor: &Executor, clock: Arc<dyn Clock>) -> Watchdog {
    let handle = handle.clone();
    let executor = executor.clone();
    Watchdog::new(
        clock,
        Box::new(move |body| {
            let stop = body.watchdog();
            let hook = executor.on_shutdown(Box::new(move || stop.stop()));
            let ex = executor.clone();
            handle.own_with(move |gate| {
                ex.run_dedicated_gated(&gate, WATCHDOG_THREAD, move || {
                    body.run();
                    drop(hook);
                })
            });
        }),
    )
}

/// `dyn CodeIntel` of one activation. Until `started` (the kernel admits no
/// call before) it answers as with no project open.
pub struct CodeIntelService {
    indexer: OnceLock<Indexer>,
    watchdog: OnceLock<Watchdog>,
}

impl CodeIntelService {
    /// The index (tests and benchmarks).
    #[cfg(any(test, feature = "test-support"))]
    pub fn indexer(&self) -> Option<&Indexer> {
        self.indexer.get()
    }

    /// This activation's watchdog (tests).
    #[cfg(any(test, feature = "test-support"))]
    pub fn watchdog(&self) -> Option<&Watchdog> {
        self.watchdog.get()
    }
}

impl CodeIntel for CodeIntelService {
    fn set_focus(&self, project: &ProjectKey, worktree: Option<&WorktreeKey>, focus: Focus) {
        if let Some(indexer) = self.indexer.get() {
            indexer.set_focus(project, worktree, focus);
        }
    }

    fn query(&self, query: Query) -> QueryId {
        match self.indexer.get() {
            Some(indexer) => indexer.query(query),
            None => QueryId(0),
        }
    }

    fn results(&self, id: QueryId) -> Option<Arc<QueryResults>> {
        match self.indexer.get() {
            Some(indexer) => indexer.results(id),
            None if id == QueryId(0) => Some(Arc::new(QueryResults {
                id,
                generation: 0,
                symbol: None,
                candidates: Arc::from(Vec::new()),
                callers: Arc::from(Vec::new()),
                same_name: Arc::from(Vec::new()),
                jump: None,
                preselect: None,
                semantic: SemanticState::NotRequested,
                state: lectern_services::code_intel::QueryState::Failed(QueryError::NotOpen),
            })),
            None => None,
        }
    }

    fn close(&self, id: QueryId) {
        if let Some(indexer) = self.indexer.get() {
            indexer.close(id);
        }
    }

    fn prefetch(&self, query: Query) {
        if let Some(indexer) = self.indexer.get() {
            indexer.prefetch(query);
        }
    }

    fn names(&self, project: &ProjectKey) -> Names {
        match self.indexer.get() {
            Some(indexer) => indexer.names(project),
            None => Names::Unavailable,
        }
    }

    fn register_provider(
        &self,
        provider: Arc<dyn NavigationProvider>,
        token: ActivationToken,
    ) -> ProviderRegistration {
        match self.indexer.get() {
            Some(indexer) => indexer.register_provider(provider, token),
            None => ProviderRegistration::new(|| {}),
        }
    }

    fn coverage(&self, project: &ProjectKey) -> Option<Coverage> {
        self.indexer.get()?.coverage(project)
    }
}

/// `dyn Projects` through the kernel: every call takes the provider's
/// admission; once `project` is gone it answers as with nothing open.
struct ProjectsRef(ServiceRef<dyn Projects>);

impl Projects for ProjectsRef {
    fn open(&self, path: &Path) -> OpenRequest {
        self.0.get().map_or(OpenRequest(0), |p| p.open(path))
    }

    fn close(&self, project: &ProjectKey) {
        if let Some(p) = self.0.get() {
            p.close(project);
        }
    }

    fn set_active_worktree(&self, project: &ProjectKey, worktree: &WorktreeKey) {
        if let Some(p) = self.0.get() {
            p.set_active_worktree(project, worktree);
        }
    }

    fn snapshot(&self, project: &ProjectKey) -> Option<Arc<ProjectSnapshot>> {
        self.0.get()?.snapshot(project)
    }

    fn open_projects(&self) -> Vec<ProjectKey> {
        self.0.get().map(|p| p.open_projects()).unwrap_or_default()
    }

    fn locate(&self, path: &Path) -> Option<Location> {
        self.0.get()?.locate(path)
    }

    fn resolve(&self, path: &Path) -> Result<Resolved, ResolveError> {
        match self.0.get() {
            Some(p) => p.resolve(path),
            None => Err(lectern_services::project::OpenError::NotFound),
        }
    }

    fn resolve_package(
        &self,
        project: &ProjectKey,
        worktree: &WorktreeKey,
        rel: &str,
    ) -> Result<PackageMatch, ScanError> {
        match self.0.get() {
            Some(p) => p.resolve_package(project, worktree, rel),
            None => Err(ScanError::RootMissing),
        }
    }
}
