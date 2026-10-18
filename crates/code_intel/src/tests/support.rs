//! The indexer's test harness: a host kernel, a manual executor, a
//! `FakeGit` repository, `FakeProjects`, a manual fs_watch over a fake
//! backend, a temporary cache and the watchdog on a `ManualClock`.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use lectern_cache::{Budgets, Cache, Config};
use lectern_executor::Executor;
use lectern_fs_watch::testing::{Driver, FakeBackend, manual};
use lectern_fs_watch::{Config as WatchConfig, Flags, Watcher};
use lectern_services::code_intel::{
    CodeIntelEvent, Coverage, Evidence, Focus, Position, Query, QueryKind, QueryResults,
    TextOrigin, TextRef, TierState,
};
use lectern_services::project::{BlobId, Package, ProjectKey, WorktreeKey};

use crate::extract::{Deadline, Lang, extract};
use crate::index::{IndexDeps, Indexer, StatCounts};
use crate::map::LiveMap;
use crate::testing::{
    FakeGit, FakeGrammars, FakePower, FakeProjects, ManualClock, TestHost, WatchdogTask, host,
    watchdog,
};

pub(crate) const CHANGED: Flags = Flags(Flags::FILE.0 | Flags::MODIFIED.0);
pub(crate) const CREATED: Flags = Flags(Flags::FILE.0 | Flags::CREATED.0);
pub(crate) const REMOVED: Flags = Flags(Flags::FILE.0 | Flags::REMOVED.0);
pub(crate) const DIR_CHANGED: Flags = Flags(Flags::DIR.0 | Flags::MODIFIED.0);

type Hook = Arc<dyn Fn(&Lang, &[u8]) + Send + Sync>;

/// What the extractor hook saw: every extraction's language and the OID of
/// its bytes, in order.
#[derive(Default)]
pub(crate) struct Seen {
    pub calls: Mutex<Vec<(String, BlobId)>>,
    pub hook: Mutex<Option<Hook>>,
    /// Each extraction's OID and status, once it returned.
    pub results: Mutex<Vec<(BlobId, crate::extract::ExtractStatus)>>,
}

pub(crate) struct Options {
    pub battery: bool,
    pub batch_tasks: usize,
    /// Language packs installed before the indexer subscribes.
    pub grammars: Option<Arc<FakeGrammars>>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            battery: false,
            batch_tasks: 1,
            grammars: None,
        }
    }
}

pub(crate) struct Harness {
    pub host: TestHost,
    pub executor: Executor,
    pub git: Arc<FakeGit>,
    pub projects: Arc<FakeProjects>,
    pub power: Arc<FakePower>,
    pub grammars: Arc<FakeGrammars>,
    pub clock: ManualClock,
    pub backend: Arc<FakeBackend>,
    pub driver: Driver,
    _watcher: Watcher,
    _cache: Cache,
    pub indexer: Indexer,
    pub seen: Arc<Seen>,
    _watchdog_task: WatchdogTask,
    _subscriptions: Vec<Box<dyn Send>>,
    _cache_dir: Option<tempfile::TempDir>,
    generation: AtomicUsize,
}

impl Harness {
    pub(crate) fn new() -> Harness {
        Harness::with(Options::default())
    }

    pub(crate) fn with(options: Options) -> Harness {
        Harness::with_parts(options, Arc::new(FakeGit::new()), None)
    }

    /// A new indexer (and kernel, executor, watcher) over the same
    /// repository and cache directory: the next run of the app.
    pub(crate) fn reopen(mut self, options: Options) -> Harness {
        self.indexer.close_all();
        let dir = self._cache_dir.take().expect("the cache directory");
        let git = self.git.clone();
        drop(self);
        Harness::with_parts(options, git, Some(dir))
    }

    fn with_parts(options: Options, git: Arc<FakeGit>, dir: Option<tempfile::TempDir>) -> Harness {
        let host = host();
        let executor = Executor::manual();
        let projects = FakeProjects::new();
        let power = FakePower::new(options.battery);
        let grammars = options
            .grammars
            .clone()
            .unwrap_or_else(|| Arc::new(FakeGrammars::new()));
        let clock = ManualClock::new();
        let backend = FakeBackend::new();
        let (watcher, driver) = manual(backend.clone(), Executor::manual(), WatchConfig::default());
        let dir = dir.unwrap_or_else(|| tempfile::tempdir().expect("temp dir"));
        let cache = cache(dir.path());
        let (wd, watchdog_task) = watchdog(&executor, Arc::new(clock.clone()));
        let indexer = Indexer::with_fake_git(
            IndexDeps {
                executor: executor.clone(),
                cache: cache.clone(),
                watcher: watcher.clone(),
                power: power.clone(),
                grammars: grammars.clone(),
                projects: projects.clone(),
                handle: host.handle.clone(),
                watchdog: wd,
                batch_tasks: options.batch_tasks,
            },
            git.clone(),
        );
        let seen = Arc::new(Seen::default());
        let s = seen.clone();
        let hash = git.hash();
        indexer.set_extractor(
            move |lang: &Lang, bytes: &[u8], deadline: &Deadline, cancel: &Arc<AtomicBool>| {
                let oid = crate::git_access::hash_blob(hash, bytes);
                s.calls
                    .lock()
                    .unwrap()
                    .push((lang.name().0.to_string(), oid));
                let hook = s.hook.lock().unwrap().clone();
                if let Some(hook) = hook {
                    hook(lang, bytes);
                }
                let extraction = extract(lang, bytes, deadline, cancel);
                s.results.lock().unwrap().push((oid, extraction.status));
                extraction
            },
        );
        let subscriptions = indexer.subscribe();
        Harness {
            host,
            executor,
            git,
            projects,
            power,
            grammars,
            clock,
            backend,
            driver,
            _watcher: watcher,
            _cache: cache,
            indexer,
            seen,
            _watchdog_task: watchdog_task,
            _subscriptions: subscriptions,
            _cache_dir: Some(dir),
            generation: AtomicUsize::new(1),
        }
    }

    pub(crate) fn key(&self) -> ProjectKey {
        self.git.key()
    }

    fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::AcqRel) as u64
    }

    /// Opens (or reopens) the project with the main worktree active.
    pub(crate) fn open(&self) {
        self.open_with("main", Vec::new());
    }

    pub(crate) fn open_with(&self, active: &str, packages: Vec<Package>) {
        self.publish(active, packages);
        self.settle();
    }

    /// Publishes a snapshot with `active` as the current worktree and
    /// hands the event to the indexer, without running anything.
    pub(crate) fn publish(&self, active: &str, packages: Vec<Package>) {
        let snapshot = self.git.snapshot(self.next_generation(), active, packages);
        let event = self.projects.set(snapshot);
        self.indexer.project_event(&event);
    }

    /// Publishes the index as the new file list (as `project` does after an
    /// index change).
    pub(crate) fn refresh_files(&self) {
        let event = self
            .projects
            .set_files(&self.key(), self.git.file_list(&self.active()));
        self.indexer.project_event(&event);
        self.settle();
    }

    pub(crate) fn set_packages(&self, dirs: &[&str]) {
        let event = self.projects.set_packages(&self.key(), packages(dirs));
        self.indexer.project_event(&event);
        self.settle();
    }

    fn active(&self) -> String {
        let snapshot = self.projects_snapshot();
        let active = snapshot.active.as_ref().and_then(|a| a.worktree.clone());
        for name in ["main"] {
            if Some(self.git.worktree_key(name)) == active {
                return name.into();
            }
        }
        "main".into()
    }

    fn projects_snapshot(&self) -> Arc<lectern_services::project::ProjectSnapshot> {
        use lectern_services::project::Projects;
        self.projects
            .snapshot(&self.key())
            .expect("an open project")
    }

    /// Runs fs_watch, the executor and the clock past every coalescing
    /// window until nothing is left to do.
    pub(crate) fn settle(&self) {
        loop {
            let a = self.driver.run_until_idle();
            self.driver.advance(Duration::from_secs(5));
            let b = self.executor.run_until_idle();
            let c = self.driver.run_until_idle();
            if a == 0 && b == 0 && c == 0 {
                // Nothing runs any more: either no batch work is left, or
                // the pool waits (paused) at full size; never work nobody
                // will pick up.
                self.assert_live("after settling");
                return;
            }
        }
    }

    /// The liveness rule of `Inner::kick_batch`, after an operation.
    pub(crate) fn assert_live(&self, after: &str) {
        let inner = self.indexer.inner();
        assert!(
            inner.live(),
            "{after}: runnable batch work with {} of the pool's tasks running",
            inner.batch.lock().unwrap().running
        );
    }

    /// Emits worktree events (paths relative to the main root).
    pub(crate) fn emit(&self, events: &[(&str, Flags)]) {
        self.backend.emit(&self.git.root("main"), events);
    }

    /// Emits events in the git directory (`index`, `HEAD`, `refs/...`).
    pub(crate) fn emit_git(&self, events: &[(&str, Flags)]) {
        self.backend.emit(self.git.common_dir(), events);
    }

    pub(crate) fn map(&self) -> LiveMap {
        self.indexer.map(&self.key()).expect("an open project")
    }

    pub(crate) fn coverage(&self) -> Coverage {
        self.indexer.coverage(&self.key()).expect("an open project")
    }

    pub(crate) fn stats(&self) -> StatCounts {
        self.indexer.stats()
    }

    /// The OIDs extracted so far, in order.
    pub(crate) fn extracted(&self) -> Vec<BlobId> {
        self.seen
            .calls
            .lock()
            .unwrap()
            .iter()
            .map(|(_, o)| *o)
            .collect()
    }

    pub(crate) fn extractions(&self) -> usize {
        self.seen.calls.lock().unwrap().len()
    }

    pub(crate) fn set_hook(&self, hook: impl Fn(&Lang, &[u8]) + Send + Sync + 'static) {
        *self.seen.hook.lock().unwrap() = Some(Arc::new(hook));
    }

    /// Whether the store has a unit for `oid` in `lang`'s current version.
    pub(crate) fn stored(&self, oid: BlobId, lang: &str) -> bool {
        let store = self.indexer.store(&self.key()).expect("an open project");
        let table = crate::extract::LanguageTable::core();
        let lang = table.get(lang).expect("a core language");
        let version = crate::store::Version::of(lang);
        store
            .exists(&version, &[oid])
            .is_some_and(|found| !found.is_empty())
    }

    /// Whether `path`'s hint matches its current effective content.
    pub(crate) fn hinted(&self, path: &str) -> bool {
        let map = self.map();
        let Some(eff) = map.effective(path) else {
            return false;
        };
        let stamp = self.indexer.stamp(&self.key()).expect("an open project");
        map.hints.matches(&eff.slot, &eff.oid, stamp)
    }

    pub(crate) fn set_focus(&self, focus: Focus) {
        let wt = self.git.worktree_key("main");
        self.indexer.set_focus(&self.key(), Some(&wt), focus);
        self.assert_live("after set_focus");
    }

    pub(crate) fn current(&self, path: &str) {
        let oid = self
            .map()
            .effective(path)
            .map(|e| e.oid)
            .expect("a path in the map");
        self.set_focus(Focus::Current(Some(TextRef {
            worktree: Some(self.git.worktree_key("main")),
            path: path.into(),
            oid,
            origin: TextOrigin::Worktree,
        })));
    }

    /// Whether the kernel is quiescent once its thread has handled what is
    /// queued (event dispatches count as busy while they run).
    pub(crate) fn quiescent(&self) -> bool {
        let kernel = self.host.kernel();
        kernel.barrier();
        // The kernel thread counts the barrier request itself down only
        // after it has replied, so for a moment the barrier still reads as
        // busy; a guard that is really held stays busy past this wait.
        let start = std::time::Instant::now();
        while !kernel.is_quiescent() {
            if start.elapsed() > Duration::from_millis(200) {
                return false;
            }
            std::thread::yield_now();
        }
        true
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.indexer.close_all();
        self.indexer.inner().watchdog.stop();
    }
}

pub(crate) fn cache(dir: &Path) -> Cache {
    Cache::with_clock(
        Config {
            root: dir.join("cache"),
            budgets: Budgets::default(),
            ui_thread: None,
        },
        lectern_cache::testing::ManualClock::new(SystemTime::now()),
    )
}

/// Sub-projects at these directories.
pub(crate) fn packages(dirs: &[&str]) -> Vec<Package> {
    dirs.iter()
        .map(|d| Package {
            dir: (*d).into(),
            name: (*d).into(),
            manifests: Arc::from(Vec::new()),
            declared: false,
            workspace_root: false,
            parent: None,
            files: 0,
        })
        .collect()
}

/// A small TypeScript module with one function named after `name`.
pub(crate) fn ts(name: &str) -> Vec<u8> {
    format!("export function {name}(a: number): number {{\n  return a + 1;\n}}\n").into_bytes()
}

/// Every tier's coverage.
pub(crate) fn all(state: TierState) -> [TierState; 4] {
    [state; 4]
}

pub(crate) fn tiers(c: &Coverage) -> [TierState; 4] {
    [c.focus, c.subproject, c.worktree, c.other_worktrees]
}

/// Lets a test wait until a hooked extraction started, and the hook wait
/// until the test opens it.
#[derive(Default)]
pub(crate) struct Gate {
    pub entered: AtomicBool,
    pub open: AtomicBool,
}

impl Gate {
    pub(crate) fn wait_entered(&self) {
        let start = std::time::Instant::now();
        while !self.entered.load(Ordering::Acquire) {
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "the extraction never started"
            );
            std::thread::yield_now();
        }
    }
}

/// The position of the `nth` (from 0) occurrence of `needle` in `text`.
pub(crate) fn pos_in(text: &[u8], needle: &str, nth: usize) -> Position {
    let mut from = 0;
    let mut found = None;
    for _ in 0..=nth {
        let at = text[from..]
            .windows(needle.len())
            .position(|w| w == needle.as_bytes())
            .map(|i| i + from)
            .unwrap_or_else(|| panic!("{needle:?} #{nth} is not in the text"));
        found = Some(at);
        from = at + 1;
    }
    let at = found.expect("found");
    let line = text[..at].iter().filter(|b| **b == b'\n').count() as u32;
    let start = text[..at]
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |i| i + 1);
    Position {
        line,
        column: (at - start) as u32,
    }
}

impl Harness {
    pub(crate) fn wt(&self) -> WorktreeKey {
        self.git.worktree_key("main")
    }

    /// The worktree text at `path`: its effective OID in the map, else the
    /// OID of the bytes on disk.
    pub(crate) fn text(&self, path: &str) -> TextRef {
        let oid = self.map().effective(path).map(|e| e.oid).unwrap_or_else(|| {
            let bytes = std::fs::read(self.git.root("main").join(path)).expect("the file");
            self.git.oid(&bytes)
        });
        TextRef {
            worktree: Some(self.wt()),
            path: path.into(),
            oid,
            origin: TextOrigin::Worktree,
        }
    }

    /// The position of the `nth` `needle` in the worktree file `path`.
    pub(crate) fn pos(&self, path: &str, needle: &str, nth: usize) -> Position {
        let bytes = std::fs::read(self.git.root("main").join(path)).expect("the file");
        pos_in(&bytes, needle, nth)
    }

    /// Asks a query and runs everything until it is done.
    pub(crate) fn ask(&self, kind: QueryKind) -> Arc<QueryResults> {
        let id = self.indexer.query(Query {
            project: self.key(),
            kind,
        });
        self.settle();
        self.indexer.results(id).expect("the query is kept")
    }

    /// Jumps from the `nth` `needle` in `path`.
    pub(crate) fn goto(&self, path: &str, needle: &str, nth: usize) -> Arc<QueryResults> {
        let at = self.text(path);
        let position = self.pos(path, needle, nth);
        self.ask(QueryKind::Definition { at, position })
    }

    /// The candidates as (path, line, evidence), for compact assertions.
    pub(crate) fn found(r: &QueryResults) -> Vec<(String, u32, Evidence)> {
        r.candidates
            .iter()
            .map(|c| {
                (
                    c.definition.location.path.to_string(),
                    c.definition.location.line,
                    c.evidence,
                )
            })
            .collect()
    }

    /// Pauses the kernel's background work: tiers 3–5 stop at their next
    /// boundary, so files stay unindexed.
    pub(crate) fn pause(&self) {
        self.host.kernel().pause();
    }

    pub(crate) fn resume(&self) {
        self.host.kernel().resume();
    }

    /// The events emitted so far (the log keeps name tables alive).
    pub(crate) fn events(&self) -> Vec<CodeIntelEvent> {
        self.indexer.inner().host.events.lock().unwrap().clone()
    }

    pub(crate) fn clear_events(&self) {
        self.indexer.inner().host.events.lock().unwrap().clear();
    }

    /// The file counts of the tiers 3–5 extraction batches so far.
    pub(crate) fn batches(&self) -> Vec<usize> {
        self.indexer.inner().batch_log.lock().unwrap().clone()
    }

    /// The status the extraction of `oid` returned last.
    pub(crate) fn status(&self, oid: BlobId) -> Option<crate::extract::ExtractStatus> {
        self.seen
            .results
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(o, _)| *o == oid)
            .map(|(_, s)| *s)
    }
}
