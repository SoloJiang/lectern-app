//! The documents' test rig: real files in a temporary worktree, a manual
//! executor, a kernel host, a temporary cache, the fs_watch manual watcher
//! with `FakeBackend`, and a git stand-in that keeps blobs in memory and
//! counts its calls.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use lectern_cache::{Budgets, Cache, Config as CacheConfig};
use lectern_executor::{Config as ExecutorConfig, Executor};
use lectern_fs_watch::testing::{Driver, FakeBackend, FakeSource, manual};
use lectern_fs_watch::{Config as WatchConfig, Flags, Watcher, WorktreeSource};
use lectern_git::{GitError, HashKind, Oid};
use lectern_kernel::testing::ManualClock;
use lectern_platform::ThreadCpuClock;
use lectern_services::code_intel::TextOrigin;
use lectern_services::project::{BlobId, ProjectKey, WorktreeKey};
use lectern_services::reader::{Anchor, ReadTarget};

use crate::doc::Probe;
use crate::git_access::{GitAccess, Tree, blob_id};
use crate::testing::{TestHost, host};
use crate::{DocHandle, DocSnapshot, Engine, EngineDeps, Languages, Trace, Viewport};

/// The git stand-in.
#[derive(Default)]
pub(crate) struct FakeGit {
    pub blobs: Mutex<HashMap<BlobId, Arc<[u8]>>>,
    pub blob_calls: AtomicUsize,
    pub source: Mutex<Option<Arc<FakeSource>>>,
    /// Set: `watch_source` fails with `GitError::Corrupt` of this text.
    pub watch_error: Mutex<Option<String>>,
}

impl FakeGit {
    /// Stores `bytes` as a blob; its SHA-1 OID.
    pub(crate) fn add_blob(&self, bytes: &[u8]) -> BlobId {
        let oid = blob_id(&Oid::for_blob(HashKind::Sha1, bytes));
        self.blobs.lock().unwrap().insert(oid, Arc::from(bytes));
        oid
    }
}

impl GitAccess for FakeGit {
    fn hash(&self, _common_dir: &Path) -> Result<HashKind, GitError> {
        Ok(HashKind::Sha1)
    }

    fn blob(&self, _common_dir: &Path, oid: BlobId, max: u64) -> Result<Arc<[u8]>, GitError> {
        self.blob_calls.fetch_add(1, Ordering::SeqCst);
        let blobs = self.blobs.lock().unwrap();
        let bytes = blobs
            .get(&oid)
            .ok_or_else(|| GitError::ObjectMissing(crate::git_access::git_oid(oid)))?;
        if bytes.len() as u64 > max {
            return Err(GitError::TooLarge {
                size: bytes.len() as u64,
            });
        }
        Ok(bytes.clone())
    }

    fn watch_source(&self, _tree: &Tree) -> Result<Arc<dyn WorktreeSource>, GitError> {
        if let Some(text) = self.watch_error.lock().unwrap().clone() {
            return Err(GitError::Corrupt(text));
        }
        let source = self.source.lock().unwrap().clone();
        Ok(source.unwrap_or_default())
    }

    fn forget(&self, _common_dir: &Path) {}
}

pub(crate) struct Rig {
    _dir: tempfile::TempDir,
    _cache_dir: tempfile::TempDir,
    _host: TestHost,
    pub engine: Engine,
    pub git: Arc<FakeGit>,
    pub backend: Arc<FakeBackend>,
    pub driver: Driver,
    pub source: Arc<FakeSource>,
    pub root: Arc<Path>,
    pub common: Arc<Path>,
    pub notified: Arc<AtomicUsize>,
}

pub(crate) const VIEW: Viewport = Viewport {
    rows: 40,
    cols: 100,
};

impl Rig {
    pub(crate) fn new() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();
        let root: Arc<Path> = Arc::from(dir.path().canonicalize().unwrap());
        let common: Arc<Path> = Arc::from(root.join(".git"));
        std::fs::create_dir_all(&common).unwrap();
        let executor = Executor::manual();
        let backend = FakeBackend::new();
        let (watcher, driver) = manual(backend.clone(), executor.clone(), WatchConfig::default());
        let cache = Cache::new(CacheConfig {
            root: cache_dir.path().to_path_buf(),
            budgets: Budgets::default(),
            ui_thread: None,
        });
        let git = Arc::new(FakeGit::default());
        let source = FakeSource::new();
        *git.source.lock().unwrap() = Some(source.clone());
        let engine = Engine::with_git(
            EngineDeps {
                executor,
                cache,
                watcher,
                languages: Languages::core(),
            },
            git.clone(),
        );
        let host = host();
        engine.start(host.handle.clone());
        Rig {
            _dir: dir,
            _cache_dir: cache_dir,
            _host: host,
            engine,
            git,
            backend,
            driver,
            source,
            root,
            common,
            notified: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub(crate) fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    pub(crate) fn write(&self, rel: &str, bytes: &[u8]) {
        let path = self.path(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, bytes).unwrap();
        self.source.track(&[rel]);
    }

    pub(crate) fn project(&self) -> ProjectKey {
        ProjectKey::Git {
            common_dir: self.common.clone(),
        }
    }

    pub(crate) fn target(&self, rel: &str, at: Anchor) -> ReadTarget {
        ReadTarget {
            project: self.project(),
            worktree: Some(WorktreeKey(self.common.clone())),
            path: rel.into(),
            origin: TextOrigin::Worktree,
            expect: None,
            at,
        }
    }

    pub(crate) fn object(&self, rel: &str, oid: BlobId, at: Anchor) -> ReadTarget {
        ReadTarget {
            origin: TextOrigin::Object,
            expect: Some(oid),
            ..self.target(rel, at)
        }
    }

    pub(crate) fn open(&self, target: ReadTarget) -> DocHandle {
        let notified = self.notified.clone();
        self.engine.open(
            target,
            self.root.clone(),
            VIEW,
            Arc::new(move || {
                notified.fetch_add(1, Ordering::SeqCst);
            }),
        )
    }

    /// Runs the executor and the watcher until neither has work.
    pub(crate) fn settle(&self) {
        self.driver.run_until_idle();
    }

    /// Opens and settles: the snapshot after every task.
    pub(crate) fn open_settled(&self, target: ReadTarget) -> (DocHandle, Arc<DocSnapshot>) {
        let doc = self.open(target);
        self.settle();
        let snapshot = doc.snapshot();
        (doc, snapshot)
    }

    /// A worktree event on `rel`, delivered.
    pub(crate) fn touch(&self, rel: &str, flags: Flags) {
        self.backend.emit(&self.root, &[(rel, flags)]);
        // Taken in now, delivered once the window is quiet.
        self.settle();
        self.driver.advance(Duration::from_secs(1));
        self.settle();
    }
}

/// An engine on a real executor with `batch` batch threads, for the order
/// of the batch work and the yield: files in a temporary folder project (not
/// watched), the kernel's keepalive slots on a manual clock, and every
/// [`Trace`] event logged with its time and its thread's CPU time
/// ([`Threaded::cpu`]).
pub(crate) struct Threaded {
    pub engine: Engine,
    pub host: TestHost,
    pub executor: Executor,
    /// Moved only by a test: one keepalive slot, then none while the
    /// switch is on (the next slot's timer stays pending).
    pub clock: ManualClock,
    pub root: Arc<Path>,
    events: Arc<Events>,
    _cache_dir: tempfile::TempDir,
    _dir: tempfile::TempDir,
}

/// The events logged, and a signal for each.
type Events = (Mutex<Log>, Condvar);

/// Each event with its time, and the CPU time its thread had used when it
/// was logged (None where the thread's CPU clock cannot be read), by the
/// same index.
#[derive(Default)]
struct Log {
    events: Vec<(Instant, Trace)>,
    cpu: Vec<Option<Duration>>,
}

/// The calling thread's CPU time so far (`platform::ThreadCpuClock`, the
/// clock `health` reads); None where it cannot be read. It does not grow
/// while the thread waits for a CPU, as a batch thread does on a machine
/// busy with other tests and builds.
pub(crate) fn thread_cpu() -> Option<Duration> {
    thread_local! {
        static CLOCK: Option<ThreadCpuClock> = ThreadCpuClock::current().ok();
    }
    CLOCK.with(|clock| clock.as_ref()?.cpu_time().ok())
}

/// How long a thread ran from one reading to a later one on the same
/// thread, each a time and [`thread_cpu`] then: the CPU time between them
/// (as `health` and `SlowWindows` count a parse); the wall time where the
/// clock cannot be read.
pub(crate) fn ran(from: (Instant, Option<Duration>), to: (Instant, Option<Duration>)) -> Duration {
    match (from.1, to.1) {
        (Some(from), Some(to)) => to.saturating_sub(from),
        _ => to.0.duration_since(from.0),
    }
}

/// How long a wait for an event may take before the test fails.
const EVENT_LIMIT: Duration = Duration::from_secs(60);

impl Threaded {
    pub(crate) fn new(batch: usize) -> Threaded {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();
        let root: Arc<Path> = Arc::from(dir.path().canonicalize().unwrap());
        let executor = Executor::new(ExecutorConfig {
            workers: 2,
            batch_workers: batch,
            on_thread_start: None,
        });
        let watcher = Watcher::new(FakeBackend::new(), executor.clone(), WatchConfig::default());
        let cache = Cache::new(CacheConfig {
            root: cache_dir.path().to_path_buf(),
            budgets: Budgets::default(),
            ui_thread: None,
        });
        let engine = Engine::with_git(
            EngineDeps {
                executor: executor.clone(),
                cache,
                watcher,
                languages: Languages::core(),
            },
            Arc::new(FakeGit::default()),
        );
        let host = host();
        let clock = ManualClock::new();
        host.kernel().set_clock(Arc::new(clock.clone()));
        engine.start(host.handle.clone());
        let events: Arc<Events> = Arc::default();
        let logged = events.clone();
        engine.set_trace(move |event| {
            let cpu = thread_cpu();
            let mut log = logged.0.lock().unwrap();
            log.events.push((Instant::now(), event));
            log.cpu.push(cpu);
            drop(log);
            logged.1.notify_all();
        });
        Threaded {
            engine,
            host,
            executor,
            clock,
            root,
            events,
            _cache_dir: cache_dir,
            _dir: dir,
        }
    }

    pub(crate) fn write(&self, rel: &str, bytes: &[u8]) {
        std::fs::write(self.root.join(rel), bytes).unwrap();
    }

    pub(crate) fn target(&self, rel: &str, at: Anchor) -> ReadTarget {
        ReadTarget {
            project: ProjectKey::Folder {
                root: self.root.clone(),
            },
            worktree: None,
            path: rel.into(),
            origin: TextOrigin::Worktree,
            expect: None,
            at,
        }
    }

    pub(crate) fn open(&self, target: ReadTarget) -> DocHandle {
        self.engine
            .open(target, self.root.clone(), VIEW, Arc::new(|| {}))
    }

    /// Every event so far.
    pub(crate) fn events(&self) -> Vec<(Instant, Trace)> {
        self.events.0.lock().unwrap().events.clone()
    }

    /// The CPU time each event's thread had used when the event was logged
    /// ([`thread_cpu`]), by the index of [`Threaded::events`].
    pub(crate) fn cpu(&self) -> Vec<Option<Duration>> {
        self.events.0.lock().unwrap().cpu.clone()
    }

    /// Waits for an event from the `from`th on that `matches`: its index
    /// and time.
    pub(crate) fn wait_for(
        &self,
        from: usize,
        matches: impl Fn(&Trace) -> bool,
    ) -> (usize, Instant) {
        let (log, changed) = &*self.events;
        let deadline = Instant::now() + EVENT_LIMIT;
        let mut log = log.lock().unwrap();
        loop {
            let events = &log.events;
            if let Some(i) = (from..events.len()).find(|i| matches(&events[*i].1)) {
                return (i, events[i].0);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "no such event within {EVENT_LIMIT:?}");
            log = changed.wait_timeout(log, left).unwrap().0;
        }
    }

    /// Waits until `done` holds, which no event signals (polled).
    pub(crate) fn wait_until(&self, what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + EVENT_LIMIT;
        while !done() {
            assert!(
                Instant::now() < deadline,
                "{what}: not within {EVENT_LIMIT:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Turns the yield switch on from inside a batch parse's step, at a
    /// slice boundary: from the probe hook, at the `nth` ask that is `ours`
    /// (counted across steps), after that ask read the switch off. The step
    /// is in flight then; its next ask must see the switch. Counted across
    /// steps, the switch turns on however few asks each step gets: on a busy
    /// machine a step can run out of its wall-time budget after only a few.
    pub(crate) fn flip_inside(
        &self,
        nth: u32,
        ours: impl Fn(&Probe) -> bool + Send + Sync + 'static,
    ) -> Arc<Mutex<Flip>> {
        let flip = Arc::new(Mutex::new(Flip::default()));
        let (log, kernel, events) = (flip.clone(), self.host.kernel(), self.events.clone());
        self.engine.set_probe(move |probe| {
            let asked = probe.asked();
            let mut flip = log.lock().unwrap();
            if let Some((on, _)) = flip.on {
                if ours(&probe) {
                    flip.since.push(probe);
                }
                if !flip.over {
                    // A later step counts from 1 again.
                    if asked > on.asked() {
                        flip.after.push(probe);
                    } else {
                        flip.over = true;
                    }
                }
            } else if ours(&probe) {
                flip.asks += 1;
                if flip.asks == nth {
                    flip.event = events.0.lock().unwrap().events.len();
                    let switched = Instant::now();
                    flip.on_cpu = thread_cpu();
                    kernel.set_yield(true);
                    flip.on = Some((probe, switched));
                }
            }
        });
        flip
    }
}

impl Probe {
    /// The ask of its step, from 1.
    pub(crate) fn asked(self) -> u32 {
        match self {
            Probe::Chain { asked, .. } | Probe::Prefetch { asked } => asked,
        }
    }
}

/// The ask of the yield probe, counted across steps, at which
/// [`Threaded::flip_inside`] turns the switch on: some slices into the
/// parse. Every step tested (a 64 KiB window, an 8 ms slice of a 1 MiB
/// parse) asks hundreds of times on an idle machine.
pub(crate) const FLIP_AT: u32 = 8;

/// The yield switch turned on inside a step ([`Threaded::flip_inside`]).
#[derive(Default)]
pub(crate) struct Flip {
    /// The ask it was turned on at, and the time just before.
    pub on: Option<(Probe, Instant)>,
    /// The CPU time of the step's thread just before ([`thread_cpu`]).
    pub on_cpu: Option<Duration>,
    /// The number of trace events logged when it was turned on: the step's
    /// end is logged at this index or later.
    pub event: usize,
    /// The same step's asks after that one: one, the ask that saw the
    /// switch and stopped the step.
    pub after: Vec<Probe>,
    /// Every ask that is `ours` after the one it was turned on at, of that
    /// step or a later one (asked from 1 again). One ask, the one that
    /// stopped the parse: the same step's next, or, when that step ran out
    /// of its time budget in the very check it was turned on in (a busy
    /// machine), the next step's first.
    pub since: Vec<Probe>,
    /// The asks that are `ours` before it was turned on.
    asks: u32,
    /// That step is over.
    over: bool,
}

impl Flip {
    /// Waits to be turned on again, at the `nth` ask from now: the switch
    /// itself is the caller's to turn off.
    pub(crate) fn rearm(&mut self) {
        *self = Flip::default();
    }
}

impl Drop for Threaded {
    fn drop(&mut self) {
        // The batch work may wait for the switch.
        self.host.kernel().set_yield(false);
        self.executor.shutdown(Duration::from_secs(10));
    }
}

/// `FILE | MODIFIED`.
pub(crate) fn modified() -> Flags {
    Flags(Flags::FILE.0 | Flags::MODIFIED.0)
}

/// `FILE | REMOVED`.
pub(crate) fn removed() -> Flags {
    Flags(Flags::FILE.0 | Flags::REMOVED.0)
}

/// `FILE | CREATED`.
pub(crate) fn created() -> Flags {
    Flags(Flags::FILE.0 | Flags::CREATED.0)
}

/// The open descriptors of this process: `/proc/self/fd` on Linux, the
/// fdescfs `/dev/fd` on macOS (the same choice as platform's `fd_*` tests).
/// The listing's own descriptor is in every count alike. A missing
/// directory panics: a silent 0 would make every comparison wrong. Counts
/// are exact under `just test` (nextest runs each test in its own process).
pub(crate) fn open_fds() -> usize {
    let dir = if cfg!(target_os = "linux") {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    };
    std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{dir}: {e}"))
        .count()
}

/// The text of the rows, `|`-joined.
pub(crate) fn texts(
    snapshot: &DocSnapshot,
    from: crate::RowPos,
    n: usize,
    cols: u32,
) -> Vec<String> {
    snapshot
        .rows(from, n, cols)
        .into_iter()
        .map(|row| match row {
            crate::Row::Text { text, .. } => text.into_owned(),
            crate::Row::Pending { .. } => "<pending>".to_owned(),
        })
        .collect()
}
