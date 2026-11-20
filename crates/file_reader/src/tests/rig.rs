//! The test rig: fake services, a session over a real `Engine` on a manual
//! executor, and temporary folders with synthetic files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use lectern_cache::{Budgets, Cache, Config as CacheConfig};
use lectern_executor::Executor;
use lectern_fs_watch::testing::{Driver, FakeBackend, manual};
use lectern_fs_watch::{Config as WatchConfig, Watcher};
use lectern_kernel::ActivationToken;
use lectern_kernel::Name;
use lectern_services::code_intel::{
    CodeIntel, Coverage, Focus, Names, NavigationProvider, ProviderRegistration, Query, QueryId,
    QueryResults, TextOrigin,
};
use lectern_services::project::{
    ActiveTree, BlobId, FileKind, FileList, Head, Location, OpenError, OpenRequest, PackageMatch,
    ProjectKey, ProjectSnapshot, Projects, ResolveError, Resolved, Scan, ScanError, Worktree,
    WorktreeKey,
};
use lectern_services::reader::{Anchor, ReadTarget, ReaderEvent};
use lectern_services::workspace::{
    CommandHandler, Contribution, Contributions, LeaveGuard, Milestone, Registration, Route,
    SidebarEntry, WindowId, WorkspaceCommand,
};
use lectern_syntax::testing::{TestHost, host};
use lectern_syntax::{DocSnapshot, Engine, EngineDeps, Languages};

use crate::{Host, Session};

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// `dyn Projects`: snapshots it is given, and the projects it was asked to
/// close.
#[derive(Default)]
pub(crate) struct FakeProjects {
    snapshots: Mutex<HashMap<ProjectKey, Arc<ProjectSnapshot>>>,
}

impl FakeProjects {
    pub(crate) fn set(&self, snapshot: ProjectSnapshot) {
        lock(&self.snapshots).insert(snapshot.key.clone(), Arc::new(snapshot));
    }

    pub(crate) fn remove(&self, key: &ProjectKey) {
        lock(&self.snapshots).remove(key);
    }
}

impl Projects for FakeProjects {
    fn open(&self, _: &Path) -> OpenRequest {
        OpenRequest(0)
    }

    fn close(&self, _: &ProjectKey) {}

    fn set_active_worktree(&self, _: &ProjectKey, _: &WorktreeKey) {}

    fn snapshot(&self, project: &ProjectKey) -> Option<Arc<ProjectSnapshot>> {
        lock(&self.snapshots).get(project).cloned()
    }

    fn open_projects(&self) -> Vec<ProjectKey> {
        lock(&self.snapshots).keys().cloned().collect()
    }

    fn locate(&self, _: &Path) -> Option<Location> {
        None
    }

    fn resolve(&self, _: &Path) -> Result<Resolved, ResolveError> {
        Err(OpenError::NotFound)
    }

    fn resolve_package(
        &self,
        _: &ProjectKey,
        _: &WorktreeKey,
        _: &str,
    ) -> Result<PackageMatch, ScanError> {
        Err(ScanError::RootMissing)
    }
}

/// One `set_focus` call.
pub(crate) type FocusCall = (ProjectKey, Option<WorktreeKey>, Focus);

/// `dyn CodeIntel`: the focus it was given, in order.
#[derive(Default)]
pub(crate) struct FakeCodeIntel {
    focus: Mutex<Vec<FocusCall>>,
}

impl FakeCodeIntel {
    pub(crate) fn calls(&self) -> Vec<FocusCall> {
        lock(&self.focus).clone()
    }

    /// The `Current` focus calls, in order.
    pub(crate) fn current(&self) -> Vec<Focus> {
        self.calls().into_iter().map(|(_, _, f)| f).collect()
    }
}

impl CodeIntel for FakeCodeIntel {
    fn set_focus(&self, project: &ProjectKey, worktree: Option<&WorktreeKey>, focus: Focus) {
        lock(&self.focus).push((project.clone(), worktree.cloned(), focus));
    }

    fn query(&self, _: Query) -> QueryId {
        unimplemented!("the reader does not query")
    }

    fn results(&self, _: QueryId) -> Option<Arc<QueryResults>> {
        None
    }

    fn close(&self, _: QueryId) {}

    fn prefetch(&self, _: Query) {}

    fn names(&self, _: &ProjectKey) -> Names {
        Names::Unavailable
    }

    fn register_provider(
        &self,
        _: Arc<dyn NavigationProvider>,
        _: ActivationToken,
    ) -> ProviderRegistration {
        ProviderRegistration::new(|| {})
    }

    fn coverage(&self, _: &ProjectKey) -> Option<Coverage> {
        None
    }
}

/// `dyn Contributions`: the plugin requires it, and step 6 uses none of it.
#[derive(Default)]
pub(crate) struct FakeContributions {
    pub(crate) registered: AtomicUsize,
}

impl FakeContributions {
    fn registration(&self) -> Registration {
        self.registered.fetch_add(1, Ordering::SeqCst);
        Registration::new(|_| {}, || {})
    }
}

impl Contributions for FakeContributions {
    fn register(&self, _: Contribution, _: Option<ActivationToken>) -> Registration {
        self.registration()
    }

    fn handle_command(
        &self,
        _: Name,
        _: CommandHandler,
        _: Option<ActivationToken>,
    ) -> Registration {
        self.registration()
    }

    fn register_leave_guard(
        &self,
        _: Arc<dyn LeaveGuard>,
        _: Option<ActivationToken>,
    ) -> Registration {
        self.registration()
    }

    fn dispatch(&self, _: WorkspaceCommand) {}

    fn sidebar_entries(&self) -> Arc<[SidebarEntry]> {
        Arc::from(Vec::new())
    }

    fn current_route(&self, _: WindowId) -> Option<Route> {
        None
    }

    fn report_milestone(&self, _: Milestone) {}
}

/// The session's `Host` over the fakes: events are kept.
pub(crate) struct FakeHost {
    pub(crate) projects: Arc<FakeProjects>,
    pub(crate) code_intel: Arc<FakeCodeIntel>,
    pub(crate) events: Mutex<Vec<ReaderEvent>>,
}

impl FakeHost {
    pub(crate) fn events(&self) -> Vec<ReaderEvent> {
        lock(&self.events).clone()
    }
}

impl Host for FakeHost {
    fn snapshot(&self, project: &ProjectKey) -> Option<Arc<ProjectSnapshot>> {
        self.projects.snapshot(project)
    }

    fn set_focus(&self, project: &ProjectKey, worktree: Option<&WorktreeKey>, focus: Focus) {
        self.code_intel.set_focus(project, worktree, focus);
    }

    fn emit(&self, event: ReaderEvent) {
        lock(&self.events).push(event);
    }
}

/// The resources `app` hands to the plugin, over a temporary cache and the
/// manual executor.
pub(crate) struct Resources {
    pub(crate) executor: Executor,
    pub(crate) cache: Cache,
    pub(crate) watcher: Watcher,
    pub(crate) driver: Driver,
    _cache_dir: tempfile::TempDir,
}

impl Resources {
    pub(crate) fn new() -> Resources {
        let cache_dir = tempfile::tempdir().unwrap();
        let executor = Executor::manual();
        let (watcher, driver) =
            manual(FakeBackend::new(), executor.clone(), WatchConfig::default());
        let cache = Cache::new(CacheConfig {
            root: cache_dir.path().to_path_buf(),
            budgets: Budgets::default(),
            ui_thread: None,
        });
        Resources {
            executor,
            cache,
            watcher,
            driver,
            _cache_dir: cache_dir,
        }
    }

    pub(crate) fn deps(&self) -> crate::FileReaderDeps {
        crate::FileReaderDeps {
            executor: self.executor.clone(),
            cache: self.cache.clone(),
            watcher: self.watcher.clone(),
        }
    }

    /// Runs the executor and the watcher until neither has work.
    pub(crate) fn settle(&self) {
        self.driver.run_until_idle();
    }
}

/// A temporary folder with files, canonical.
pub(crate) struct Tree {
    _dir: tempfile::TempDir,
    pub(crate) root: Arc<Path>,
}

impl Tree {
    pub(crate) fn new() -> Tree {
        let dir = tempfile::tempdir().unwrap();
        let root: Arc<Path> = Arc::from(dir.path().canonicalize().unwrap());
        Tree { _dir: dir, root }
    }

    pub(crate) fn write(&self, rel: &str, bytes: &[u8]) -> PathBuf {
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// The folder project of this tree.
    pub(crate) fn folder(&self) -> ProjectKey {
        ProjectKey::Folder {
            root: self.root.clone(),
        }
    }

    /// A git project whose main worktree is this tree (nothing is read from
    /// the repository: the key and the snapshot only name it).
    pub(crate) fn git(&self) -> (ProjectKey, WorktreeKey) {
        let common: Arc<Path> = Arc::from(self.root.join(".git"));
        (
            ProjectKey::Git {
                common_dir: common.clone(),
            },
            WorktreeKey(common),
        )
    }
}

pub(crate) fn folder_snapshot(key: &ProjectKey, root: &Arc<Path>) -> ProjectSnapshot {
    ProjectSnapshot {
        key: key.clone(),
        generation: 1,
        name: "folder".into(),
        worktrees: Arc::from(Vec::new()),
        active: Some(ActiveTree {
            root: root.clone(),
            worktree: None,
            opened_at: "".into(),
            files: Scan::Pending,
            packages: Scan::Pending,
            dirs: Scan::Pending,
        }),
    }
}

/// A git project with one worktree and its file list (the index's OIDs).
pub(crate) fn git_snapshot(
    key: &ProjectKey,
    worktree: &WorktreeKey,
    root: &Arc<Path>,
    files: &[(&str, BlobId)],
) -> ProjectSnapshot {
    let mut list = FileList::builder();
    for (path, oid) in files {
        list.push_with_oid(path, FileKind::File, *oid);
    }
    ProjectSnapshot {
        key: key.clone(),
        generation: 1,
        name: "repo".into(),
        worktrees: Arc::from(vec![Worktree {
            key: worktree.clone(),
            root: root.clone(),
            is_main: true,
            head: Head::Unknown,
            locked: false,
            prunable: false,
        }]),
        active: Some(ActiveTree {
            root: root.clone(),
            worktree: Some(worktree.clone()),
            opened_at: "".into(),
            files: Scan::Ready(Arc::new(list.build())),
            packages: Scan::Pending,
            dirs: Scan::Pending,
        }),
    }
}

/// A target in a folder project: a workspace file.
pub(crate) fn file(key: &ProjectKey, rel: &str, at: Anchor) -> ReadTarget {
    ReadTarget {
        project: key.clone(),
        worktree: None,
        path: rel.into(),
        origin: TextOrigin::Worktree,
        expect: None,
        at,
    }
}

/// A session over a real engine (manual executor, started on a kernel
/// host's handle) and the fake host.
pub(crate) struct Rig {
    pub(crate) res: Resources,
    pub(crate) host: Arc<FakeHost>,
    pub(crate) session: Arc<Session>,
    pub(crate) woken: Arc<AtomicUsize>,
    pub(crate) kernel: TestHost,
}

impl Rig {
    pub(crate) fn new() -> Rig {
        let res = Resources::new();
        let kernel = host();
        let engine = Engine::new(EngineDeps {
            executor: res.executor.clone(),
            cache: res.cache.clone(),
            watcher: res.watcher.clone(),
            languages: Languages::core(),
        });
        engine.start(kernel.handle.clone());
        let host = Arc::new(FakeHost {
            projects: Arc::default(),
            code_intel: Arc::default(),
            events: Mutex::new(Vec::new()),
        });
        let session = Arc::new(Session::new(engine, host.clone()));
        let woken = Arc::new(AtomicUsize::new(0));
        let count = woken.clone();
        session.set_waker(Some(Arc::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
        })));
        Rig {
            res,
            host,
            session,
            woken,
            kernel,
        }
    }

    /// A folder project over a new tree, open in the fake `Projects`.
    pub(crate) fn folder(&self) -> (Tree, ProjectKey) {
        let tree = Tree::new();
        let key = tree.folder();
        self.host.projects.set(folder_snapshot(&key, &tree.root));
        (tree, key)
    }

    /// Takes the next request and applies it, as the view does once the
    /// leave guard passed; runs the tasks.
    pub(crate) fn show(&self, target: ReadTarget) -> Arc<DocSnapshot> {
        use lectern_services::reader::Reader;
        self.session.open(target);
        let request = self.session.next_request().expect("queued");
        self.session.apply(request);
        self.res.settle();
        self.session.sync().expect("a document is shown")
    }
}
