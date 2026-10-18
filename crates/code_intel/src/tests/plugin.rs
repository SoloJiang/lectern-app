//! `plugin_*`: the `code-intel` plugin on a real kernel (`TestKernel` with
//! a mocked `dyn Projects`) and `Executor::manual()`: no IO between
//! `activate` and the first project; it needs `dyn Projects`; `Results`
//! events in generation order; the kernel busy until a query's last batch;
//! teardown leaves no subscription, and a closed project cancels its
//! semantic requests.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use lectern_cache::{Budgets, Cache, Config};
use lectern_executor::Executor;
use lectern_fs_watch::testing::{Driver, FakeBackend, manual};
use lectern_fs_watch::{Config as WatchConfig, Watcher};
use lectern_kernel::testing::TestKernel;
use lectern_kernel::{Kernel, PluginError};
use lectern_services::code_intel::{
    CodeIntel, CodeIntelEvent, LangName, NavigationProvider, Query, QueryKind, QueryState,
    SemanticReply, SemanticRequest, SemanticRequestId, TextOrigin, TextRef,
};
use lectern_services::project::{ProjectEvent, Projects};

use crate::plugin::{CodeIntelDeps, CodeIntelPlugin, CodeIntelService, Services};
use crate::testing::{FakeGit, FakeGrammars, FakePower, FakeProjects, ManualClock};

use super::support::{pos_in, quiescent};

/// One kernel running the plugin.
pub(crate) struct Run {
    pub kernel: TestKernel,
    pub services: Services,
    pub executor: Executor,
    pub git: Arc<FakeGit>,
    pub projects: Arc<FakeProjects>,
    pub power: Arc<FakePower>,
    pub grammars: Arc<FakeGrammars>,
    pub backend: Arc<FakeBackend>,
    pub driver: Driver,
    _clock: ManualClock,
    _watcher: Watcher,
    pub cache_root: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

impl Run {
    /// The plugin over `git`, not activated yet; `projects` mocked or not.
    pub(crate) fn new(git: Arc<FakeGit>, with_projects: bool) -> Run {
        let executor = Executor::manual();
        let projects = FakeProjects::new();
        let power = FakePower::new(false);
        let grammars = Arc::new(FakeGrammars::new());
        let backend = FakeBackend::new();
        let (watcher, driver) = manual(backend.clone(), Executor::manual(), WatchConfig::default());
        let dir = tempfile::tempdir().expect("temp dir");
        let cache_root = dir.path().join("cache");
        let cache = Cache::with_clock(
            Config {
                root: cache_root.clone(),
                budgets: Budgets::default(),
                ui_thread: None,
            },
            lectern_cache::testing::ManualClock::new(SystemTime::now()),
        );
        let clock = ManualClock::new();
        let plugin = CodeIntelPlugin::with_fake_git(
            CodeIntelDeps {
                executor: executor.clone(),
                cache,
                watcher: watcher.clone(),
                power: power.clone(),
                grammars: grammars.clone(),
            },
            git.clone(),
        )
        .with_clock(Arc::new(clock.clone()))
        .with_batch_tasks(1);
        let services = plugin.services();
        let mut kernel = TestKernel::new(plugin);
        if with_projects {
            kernel = kernel.mock::<dyn Projects>(projects.clone());
        }
        Run {
            kernel,
            services,
            executor,
            git,
            projects,
            power,
            grammars,
            backend,
            driver,
            _clock: clock,
            _watcher: watcher,
            cache_root,
            _dir: dir,
        }
    }

    pub(crate) fn kernel(&self) -> Arc<Kernel> {
        self.kernel.inner().expect("a kernel")
    }

    pub(crate) fn service(&self) -> Arc<CodeIntelService> {
        self.services.get().expect("an active service")
    }

    /// Publishes the project's snapshot and emits the event `project` would.
    pub(crate) fn open(&self) {
        let event = self.projects.set(self.git.snapshot(1, "main", Vec::new()));
        self.emit(event);
        self.settle();
    }

    pub(crate) fn emit(&self, event: ProjectEvent) {
        self.kernel().emitter().emit(event);
    }

    pub(crate) fn settle(&self) {
        loop {
            let a = self.driver.run_until_idle();
            self.driver.advance(Duration::from_secs(5));
            let b = self.executor.run_until_idle();
            if a == 0 && b == 0 && self.driver.run_until_idle() == 0 {
                return;
            }
        }
    }

    pub(crate) fn results_events(&self) -> Vec<(u64, u32)> {
        self.kernel
            .emitted::<CodeIntelEvent>()
            .into_iter()
            .filter_map(|e| match e {
                CodeIntelEvent::Results { id, generation } => Some((id.0, generation)),
                CodeIntelEvent::Names { .. } => None,
            })
            .collect()
    }
}

/// Every file name under `dir`, recursively (a snapshot of what exists).
fn listing(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            out.push(e.path());
            out.extend(listing(&e.path()));
        }
    }
    out.sort();
    out
}

fn repo() -> Arc<FakeGit> {
    let git = Arc::new(FakeGit::new());
    git.add(
        "main",
        &[
            ("a.ts", b"export function run() { return item(); }\n"),
            ("b.ts", b"export function item() { return 1; }\n"),
        ],
    );
    git.commit("main");
    git
}

#[test]
fn plugin_activate_does_no_io_before_a_project() {
    let run = Run::new(repo(), true);
    let before = listing(run.cache_root.parent().expect("dir"));
    run.kernel.activate().expect("activates");
    run.settle();
    assert!(run.git.calls().is_empty(), "{:?}", run.git.calls());
    assert_eq!(
        listing(run.cache_root.parent().unwrap()),
        before,
        "the cache is untouched"
    );
    assert!(run.backend.targets().is_empty(), "nothing watched");
    // The first project: now it works.
    run.open();
    assert!(!run.git.calls().is_empty());
}

#[test]
fn plugin_requires_projects() {
    let run = Run::new(repo(), false);
    assert_eq!(
        run.kernel.activate(),
        Err(PluginError::DependencyUnavailable)
    );
}

/// `Results` events of one query come in generation order, the last one
/// the results' generation, and the kernel is busy from the query until its
/// last batch is written.
#[test]
fn plugin_results_in_order_and_busy_until_the_last_batch() {
    let run = Run::new(repo(), true);
    run.kernel.activate().expect("activates");
    run.open();
    let service = run.service();
    let kernel = run.kernel();
    assert!(quiescent(&kernel));
    let text = b"export function run() { return item(); }\n";
    let id = service.query(Query {
        project: run.git.key(),
        kind: QueryKind::Definition {
            at: TextRef {
                worktree: Some(run.git.worktree_key("main")),
                path: "a.ts".into(),
                oid: run.git.oid(text),
                origin: TextOrigin::Worktree,
            },
            position: pos_in(text, "item", 0),
        },
    });
    assert!(!quiescent(&kernel), "busy from the query");
    run.settle();
    let r = service.results(id).expect("kept");
    assert!(matches!(r.state, QueryState::Done(_)));
    assert!(quiescent(&kernel), "quiescent once done");
    let generations: Vec<u32> = run
        .results_events()
        .into_iter()
        .filter(|(i, _)| *i == id.0)
        .map(|(_, g)| g)
        .collect();
    let want: Vec<u32> = (1..=r.generation).collect();
    assert_eq!(generations, want);
}

struct Recorder {
    cancelled: Mutex<Vec<SemanticRequestId>>,
    held: Mutex<Vec<SemanticReply>>,
}

impl NavigationProvider for Recorder {
    fn id(&self) -> &str {
        "jdtls"
    }

    fn handles(&self, lang: &LangName) -> bool {
        &*lang.0 == "java"
    }

    fn head_only(&self) -> bool {
        false
    }

    fn definition(&self, _request: SemanticRequest, reply: SemanticReply) {
        self.held.lock().unwrap().push(reply);
    }

    fn references(&self, _request: SemanticRequest, reply: SemanticReply) {
        self.held.lock().unwrap().push(reply);
    }

    fn cancel(&self, request: SemanticRequestId) {
        self.cancelled.lock().unwrap().push(request);
    }
}

/// Teardown: closing the project cancels its unanswered semantic requests
/// and drops its fs_watch subscription; deactivation drops the power and
/// language pack subscriptions.
#[test]
fn plugin_teardown_unsubscribes_and_cancels() {
    let git = Arc::new(FakeGit::new());
    let a = "class A {\n  int run(B b) {\n    return b.size();\n  }\n}\n";
    git.add(
        "main",
        &[
            ("A.java", a.as_bytes()),
            ("B.java", b"class B {\n  int size() { return 1; }\n}\n"),
        ],
    );
    git.commit("main");
    let run = Run::new(git, true);
    run.kernel.activate().expect("activates");
    assert_eq!(run.power.subscribers(), 1);
    assert_eq!(run.grammars.subscribers(), 1);
    run.open();
    let root = run.git.root("main");
    assert!(run.backend.is_armed(&root), "the worktree is watched");
    let service = run.service();
    let recorder = Arc::new(Recorder {
        cancelled: Mutex::default(),
        held: Mutex::default(),
    });
    // The provider's activation is the kernel's mock plugin's? Any live
    // token works: the host's.
    let host = crate::testing::host();
    let _reg = service.register_provider(recorder.clone(), host.handle.token());
    service.query(Query {
        project: run.git.key(),
        kind: QueryKind::Definition {
            at: TextRef {
                worktree: Some(run.git.worktree_key("main")),
                path: "A.java".into(),
                oid: run.git.oid(a.as_bytes()),
                origin: TextOrigin::Worktree,
            },
            position: pos_in(a.as_bytes(), "size", 0),
        },
    });
    run.settle();
    assert_eq!(recorder.held.lock().unwrap().len(), 1, "asked");
    run.emit(run.projects.close(&run.git.key()));
    run.settle();
    assert_eq!(
        recorder.cancelled.lock().unwrap().len(),
        1,
        "cancelled on close"
    );
    assert!(
        !run.backend.is_armed(&root),
        "the worktree is no longer watched"
    );
    drop(service);
    run.kernel.deactivate();
    assert_eq!(run.power.subscribers(), 0);
    assert_eq!(run.grammars.subscribers(), 0);
    assert!(run.services.get().is_none(), "the activation is gone");
}
