//! `project/resident_cloudprint_bytes`, `project/resident_100k_bytes`
//! (docs/design/project.md「性能」): what the `project` plugin holds at
//! rest, in live bytes (`CountingAlloc`), measured the way kernel「性能」
//! measures a plugin's resident memory but in one process: a kernel with
//! the plugin's dependency closure (empty: `project` requires nothing),
//! then a kernel with the plugin, both at rest; the difference is the
//! plugin's. In the second the plugin is active, the fixture is open with
//! its watches armed and their first batches handled, and `resolve_package`
//! has filled the sub-project cache of every inactive worktree (10 in the
//! `cloudprint` shape, 16 in the large one). The first arms the same
//! watches without the plugin: the fs_watch subscriptions (fs_watch's
//! share, `fs_watch/resident*`) and the filter, with the active worktree's
//! parsed index (`TrackedPaths`) it shares with the handle `project` keeps
//! (git's share, `git/filter_*_bytes`, ADR 0035), cancel out. What
//! remains is `project`'s own: the file list with
//! its blob OIDs, the sub-projects and their caches, the facts the kernel
//! keeps, the parse cache; also a consumer plugin holding the service, a
//! few hundred bytes, counted against `project`.
//!
//! The executor and the watcher are the app's, created before the count.
//! The first sample is a warm-up (process-wide lazy state of gix and the
//! kernel) and is not reported.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lectern_bench::{Bench, BenchError, CountingAlloc, Ctx, Measured, Value};
use lectern_executor::Executor;
use lectern_fs_watch::testing::{Driver, FakeBackend, RecordingSink, manual};
use lectern_fs_watch::{
    Coalesce, Config, RepoBatch, RepoSpec, RepoWatch, WorktreeBatch, WorktreeName, WorktreeSpec,
    WorktreeWatch,
};
use lectern_git::WorktreeId;
use lectern_kernel::{
    Activation, Active, Context, Kernel, Manifest, Plugin, PluginError, PluginSet, ServiceRef,
    service,
};
use lectern_project::testing::Shape;
use lectern_project::{ProjectDeps, ProjectPlugin};
use lectern_services::project::{PackageMatch, Projects};

use crate::project_repo::{self, ProjectRepo};

const SAMPLES: usize = 3;

/// Longer than every fs_watch coalescing window.
const LONG: Duration = Duration::from_secs(5);

/// Inactive worktrees whose sub-projects the large shape caches: the cache
/// holds 16 (docs/design/project.md「数据流与线程」).
const CACHED_WORKTREES: usize = 16;

pub const RESIDENT_CLOUDPRINT: Bench = Bench {
    id: "project/resident_cloudprint_bytes",
    run: resident_cloudprint,
};

fn resident_cloudprint(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let repo = project_repo::build(Shape::cloudprint(), 11)?;
    let samples = resident(ctx, &repo)?;
    Ok(vec![Measured::new(
        "project/resident_cloudprint_bytes",
        Value::Bytes(samples),
    )])
}

pub const RESIDENT_100K: Bench = Bench {
    id: "project/resident_100k_bytes",
    run: resident_100k,
};

fn resident_100k(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (files, manifests, worktrees) = ctx.scale((100_000, 2_000, 17), (2_000, 40, 3));
    let repo = project_repo::build(Shape::monorepo(files, manifests), worktrees)?;
    let samples = resident(ctx, &repo)?;
    Ok(vec![Measured::new(
        "project/resident_100k_bytes",
        Value::Bytes(samples),
    )])
}

type Slot = Arc<Mutex<Option<ServiceRef<dyn Projects>>>>;

/// Requires `dyn Projects` and keeps it, as `project_home` would.
struct Consumer(Slot);

impl Plugin for Consumer {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: Vec::new(),
            requires: vec![service::<dyn Projects>()],
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(ctx.service::<dyn Projects>());
        Ok(Box::new(()))
    }
}

/// The app's executor and watcher.
struct App {
    executor: Executor,
    driver: Driver,
    watcher: lectern_fs_watch::Watcher,
}

impl App {
    fn new() -> App {
        let (watcher, driver) = manual(FakeBackend::new(), Executor::manual(), Config::default());
        App {
            executor: Executor::manual(),
            driver,
            watcher,
        }
    }

    /// Runs fs_watch, the plugin's work and the kernel's in turn until none
    /// of them has work left, then waits for the kernel to be quiescent.
    fn settle(&self, kernel: &Kernel) -> Result<(), BenchError> {
        loop {
            self.driver.advance(LONG);
            let ran = self.executor.run_until_idle();
            kernel.barrier();
            if ran == 0 && self.driver.run_until_idle() == 0 {
                break;
            }
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while !kernel.is_quiescent() {
            if Instant::now() > deadline {
                return Err(BenchError::new("the kernel did not come to rest"));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    }
}

fn resident(ctx: &mut Ctx, repo: &ProjectRepo) -> Result<Vec<u64>, BenchError> {
    let app = App::new();
    let mut samples = Vec::new();
    for round in 0..=ctx.scale(SAMPLES, 1) {
        let closure = at_rest(&app, repo, false)?;
        let with = at_rest(&app, repo, true)?;
        if round > 0 {
            samples.push(with.saturating_sub(closure));
        }
    }
    Ok(samples)
}

/// Live bytes a kernel holds at rest: without `project`, or with it and
/// the fixture open as the module comment says.
fn at_rest(app: &App, repo: &ProjectRepo, with_project: bool) -> Result<u64, BenchError> {
    let before = CountingAlloc::live();
    let slot = Slot::default();
    let mut set = PluginSet::new();
    if with_project {
        let deps = ProjectDeps {
            executor: app.executor.clone(),
            watcher: app.watcher.clone(),
            home: repo.t.base().join("home"),
        };
        set = set
            .with("project", ProjectPlugin::with_repository_config_only(deps))
            .with("consumer", Consumer(slot.clone()));
    }
    let kernel = Kernel::start(set);
    kernel.barrier();
    let (opened, watching) = if with_project {
        (open_everything(app, &kernel, &slot, repo), None)
    } else {
        match watch_like_project(app, repo) {
            Ok(watching) => (Ok(()), Some(watching)),
            Err(e) => (Err(e), None),
        }
    };
    let held = opened.and_then(|()| app.settle(&kernel)).map(|()| {
        if let Some(w) = &watching {
            // Delivered batches wait in the recording sinks.
            w.repo_sink.take();
            w.tree_sink.take();
        }
        CountingAlloc::live().saturating_sub(before) as u64
    });
    drop(watching);
    drop(slot.lock().unwrap_or_else(|e| e.into_inner()).take());
    let (done, wait) = std::sync::mpsc::channel();
    kernel.shutdown(move || {
        let _ = done.send(());
    });
    let _ = wait.recv_timeout(Duration::from_secs(10));
    app.executor.run_until_idle();
    app.driver.run_until_idle();
    held
}

/// The watches `project` arms for the open fixture, armed without it: the
/// repository and the main worktree, its filter built from a `lectern-git`
/// worktree handle's source (the handle `project` keeps shares its parsed
/// index with that filter). fs_watch's and git's shares, not `project`'s.
struct Watching {
    _tree: WorktreeWatch,
    _repo: RepoWatch,
    _handle: lectern_git::Worktree,
    repo_sink: Arc<RecordingSink<RepoBatch>>,
    tree_sink: Arc<RecordingSink<WorktreeBatch>>,
}

fn watch_like_project(app: &App, repo: &ProjectRepo) -> Result<Watching, BenchError> {
    let found = lectern_git::testing::discover(repo.root(), &[])?
        .ok_or_else(|| BenchError::new("no repository"))?;
    let handle = found.repo.worktree(&WorktreeId::Main)?;
    let repo_sink = RecordingSink::new(None);
    let tree_sink = RecordingSink::new(None);
    let repo_watch = app.watcher.watch_repo(
        RepoSpec {
            common_dir: found.repo.location().common_dir.clone(),
            coalesce: Coalesce::REPO,
        },
        repo_sink.clone(),
    );
    let tree = repo_watch.watch_worktree(
        WorktreeSpec {
            name: WorktreeName::Main,
            root: handle.root().to_path_buf(),
            source: handle.watch_source(),
            coalesce: Coalesce::WORKTREE,
        },
        tree_sink.clone(),
    );
    Ok(Watching {
        _tree: tree,
        _repo: repo_watch,
        _handle: handle,
        repo_sink,
        tree_sink,
    })
}

fn open_everything(
    app: &App,
    kernel: &Kernel,
    slot: &Slot,
    repo: &ProjectRepo,
) -> Result<(), BenchError> {
    let service = slot
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .ok_or_else(|| BenchError::new("the consumer is not active"))?;
    let projects = service
        .get()
        .ok_or_else(|| BenchError::new("no Projects provider"))?;
    projects.open(repo.root());
    drop(projects);
    app.settle(kernel)?;
    let projects = service
        .get()
        .ok_or_else(|| BenchError::new("no Projects provider"))?;
    let key = projects
        .open_projects()
        .pop()
        .ok_or_else(|| BenchError::new("the project did not open"))?;
    let snapshot = projects
        .snapshot(&key)
        .ok_or_else(|| BenchError::new("no snapshot"))?;
    check_ready(&snapshot, repo)?;
    let (manifest, _) = repo
        .shape
        .manifests
        .iter()
        .rfind(|(p, _)| p.contains('/'))
        .ok_or_else(|| BenchError::new("no sub-project manifest"))?;
    let dir = Path::new(manifest)
        .parent()
        .and_then(Path::to_str)
        .unwrap_or("");
    let rel = format!("{dir}/src/index.ts");
    for worktree in snapshot
        .worktrees
        .iter()
        .filter(|w| !w.is_main)
        .take(CACHED_WORKTREES)
    {
        match projects.resolve_package(&key, &worktree.key, &rel) {
            Ok(PackageMatch::Found(found)) if *found == *dir => {}
            other => {
                return Err(BenchError::new(format!(
                    "resolve_package in {}: {other:?}",
                    worktree.root.display()
                )));
            }
        }
    }
    Ok(())
}

fn check_ready(
    snapshot: &lectern_services::project::ProjectSnapshot,
    repo: &ProjectRepo,
) -> Result<(), BenchError> {
    use lectern_services::project::Scan;
    let active = snapshot
        .active
        .as_ref()
        .ok_or_else(|| BenchError::new("no active tree"))?;
    match (&active.files, &active.packages) {
        (Scan::Ready(f), Scan::Ready(p))
            if f.len() == repo.shape.paths.len() && p.len() == repo.shape.packages =>
        {
            Ok(())
        }
        _ => Err(BenchError::new("the active tree is not ready")),
    }
}
