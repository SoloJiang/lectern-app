//! The watcher, its handles and `flush_revision`.

use std::cell::Cell;
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime};

use lectern_executor::{Executor, Task};
use lectern_logging::warn;

use crate::backend::{Armed, Backend};
use crate::clock::Clock;
use crate::engine::Engine;
use crate::path::WatchPath;
use crate::queue::{Cmd, Item, Queue, SubId, Wait};
use crate::reflog::RunId;
use crate::source::{Sink, WorktreeSource};
use crate::types::{
    Coalesce, DirBatch, Failure, FlushError, RepoBatch, Revision, WorktreeBatch, WorktreeName,
};
use crate::util::{lock, wait_timeout};

/// Watcher settings.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Config {
    /// At most this many events wait in the fs_watch queue; default 16 384.
    /// A target whose batch does not fit is rescanned.
    pub queue_events: usize,
    /// At most this many paths in one batch; default 10 000. Beyond that the
    /// batch becomes a `rescan` without paths.
    pub max_batch_paths: usize,
    /// How long `flush_revision` waits for the fs_watch thread; default 2 s.
    pub flush_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            queue_events: 16_384,
            max_batch_paths: 10_000,
            flush_timeout: Duration::from_secs(2),
        }
    }
}

pub struct RepoSpec {
    /// The git common directory, absolute and canonical
    /// (`git::Location::common_dir`).
    pub common_dir: PathBuf,
    pub coalesce: Coalesce,
}

pub struct WorktreeSpec {
    pub name: WorktreeName,
    /// The worktree root, absolute (`git::WorktreeInfo::root`).
    pub root: PathBuf,
    /// Where the filter and the config list come from; `git` implements it.
    /// A worktree state uses the source of its first handle only.
    pub source: Arc<dyn WorktreeSource>,
    pub coalesce: Coalesce,
}

pub struct DirSpec {
    pub root: PathBuf,
    pub coalesce: Coalesce,
}

/// `Clone + Send + Sync`. `app` creates one after the startup barrier is
/// released and hands it to the plugins that need it (`project`, `agents`,
/// `review`, `usage`).
///
/// Creating it starts no thread and calls no backend; the fs_watch thread
/// starts on the first `watch_*` (ADR 0017). Dropping the last clone shuts
/// it down.
#[derive(Clone)]
pub struct Watcher {
    owner: Arc<Owner>,
}

struct Owner {
    shared: Arc<Shared>,
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.shared.shutdown();
    }
}

impl fmt::Debug for Watcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Watcher").finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// The fs_watch thread processes the queue.
    Thread,
    /// `testing::Driver` and `flush_revision` process it on the calling
    /// thread.
    Manual,
}

thread_local! {
    /// Set while this thread processes fs_watch items (and so may be inside a
    /// `Sink` callback).
    static IN_ENGINE: Cell<bool> = const { Cell::new(false) };
}

pub(crate) fn in_engine() -> bool {
    IN_ENGINE.with(Cell::get)
}

/// `flush_revision` barriers: the last fence the engine processed.
#[derive(Default)]
pub(crate) struct Fences {
    next: AtomicU64,
    done: Mutex<u64>,
    changed: Condvar,
}

impl Fences {
    pub(crate) fn complete(&self, n: u64) {
        let mut done = lock(&self.done);
        *done = (*done).max(n);
        self.changed.notify_all();
    }

    fn is_done(&self, n: u64) -> bool {
        *lock(&self.done) >= n
    }
}

enum ThreadState {
    NotStarted,
    Running(#[allow(dead_code, reason = "kept so the thread is not cancelled")] Task<()>),
    Stopped,
}

pub(crate) struct Shared {
    me: Weak<Shared>,
    pub(crate) queue: Arc<Queue>,
    engine: Mutex<Engine>,
    /// Worktree states by (common dir, name), with their handle counts, so
    /// that `revision()` reads the shared counter right away.
    registry: Mutex<Registry>,
    fences: Arc<Fences>,
    mode: Mode,
    flush_timeout: Duration,
    executor: Executor,
    thread: Mutex<ThreadState>,
    exited: Arc<(Mutex<bool>, Condvar)>,
    stopping: Arc<AtomicBool>,
    next_sub: AtomicU64,
}

/// Worktree states by (common dir, name), with their handle counts.
type Registry = HashMap<(PathBuf, WorktreeName), (Arc<WorktreeShared>, usize)>;

static NEXT_EPOCH: AtomicU64 = AtomicU64::new(1);
static NEXT_RUN: AtomicU64 = AtomicU64::new(0);

/// A run ID from the wall clock (nanoseconds), hashed with the standard
/// library's per-process random keys (`RandomState`) in place of the process
/// ID; a process-wide counter keeps two watchers created in the same
/// nanosecond apart.
fn run_id() -> RunId {
    use std::hash::{BuildHasher, Hasher};
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(nanos);
    hasher.write_u64(NEXT_RUN.fetch_add(1, SeqCst));
    RunId(hasher.finish())
}

impl Watcher {
    pub fn new(backend: Arc<dyn Backend>, executor: Executor, config: Config) -> Watcher {
        Watcher::with_mode(backend, executor, config, Mode::Thread, Clock::real())
    }

    pub(crate) fn with_mode(
        backend: Arc<dyn Backend>,
        executor: Executor,
        config: Config,
        mode: Mode,
        clock: Clock,
    ) -> Watcher {
        let queue = Arc::new(Queue::new(config.queue_events));
        let fences = Arc::new(Fences::default());
        let stopping = Arc::new(AtomicBool::new(false));
        let engine = Engine::new(
            backend,
            executor.clone(),
            queue.clone(),
            fences.clone(),
            stopping.clone(),
            clock,
            config,
            run_id(),
        );
        Watcher {
            owner: Arc::new(Owner {
                shared: Arc::new_cyclic(|me| Shared {
                    me: me.clone(),
                    queue,
                    engine: Mutex::new(engine),
                    registry: Mutex::new(HashMap::new()),
                    fences,
                    mode,
                    flush_timeout: config.flush_timeout,
                    executor,
                    thread: Mutex::new(ThreadState::NotStarted),
                    exited: Arc::new((Mutex::new(false), Condvar::new())),
                    stopping,
                    next_sub: AtomicU64::new(1),
                }),
            }),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn shared(&self) -> &Arc<Shared> {
        &self.owner.shared
    }

    /// Watches a repository's git metadata. Handles with the same
    /// `common_dir` share one repository state; each has its own `sink` and
    /// coalescing.
    pub fn watch_repo(&self, spec: RepoSpec, sink: Arc<dyn Sink<RepoBatch>>) -> RepoWatch {
        let shared = &self.owner.shared;
        let guard = sink.busy();
        let sub = shared.sub_id();
        let cmd = Cmd::WatchRepo {
            sub,
            common: spec.common_dir.clone(),
            coalesce: spec.coalesce,
            sink,
            guard,
        };
        let live = shared.submit(cmd);
        RepoWatch {
            common: spec.common_dir,
            lease: live.then(|| Lease::new(shared.clone(), sub)),
        }
    }

    /// Watches a directory recursively, unfiltered (the session directories
    /// of `agents`). `root` may not exist yet (docs/design/fs_watch.md
    /// 「目录监听」).
    pub fn watch_dir(&self, spec: DirSpec, sink: Arc<dyn Sink<DirBatch>>) -> DirWatch {
        let shared = &self.owner.shared;
        let guard = sink.busy();
        let sub = shared.sub_id();
        let cmd = Cmd::WatchDir {
            sub,
            root: spec.root,
            coalesce: spec.coalesce,
            sink,
            guard,
        };
        let live = shared.submit(cmd);
        DirWatch {
            _lease: live.then(|| Lease::new(shared.clone(), sub)),
        }
    }

    /// Stops the fs_watch thread and drops every backend watch; later
    /// `watch_*` calls return stopped handles that never deliver. `app`
    /// calls it on exit.
    pub fn shutdown(&self) {
        self.owner.shared.shutdown();
    }
}

impl Shared {
    fn sub_id(&self) -> SubId {
        SubId(self.next_sub.fetch_add(1, SeqCst))
    }

    /// Queues a command and starts the fs_watch thread if needed. False when
    /// the watcher is stopped (the command is dropped).
    fn submit(&self, cmd: Cmd) -> bool {
        if self.stopping.load(SeqCst) {
            return false;
        }
        if let Some(refused) = self.queue.push_cmd(cmd) {
            drop(refused);
            return false;
        }
        self.ensure_started();
        true
    }

    fn ensure_started(&self) {
        if self.mode != Mode::Thread {
            return;
        }
        let mut thread = lock(&self.thread);
        if !matches!(*thread, ThreadState::NotStarted) {
            return;
        }
        let Some(me) = self.me.upgrade() else { return };
        let task = self.executor.run_dedicated("fs-watch", move || me.run());
        *thread = ThreadState::Running(task);
    }

    /// The fs_watch thread.
    fn run(self: Arc<Shared>) {
        let mut timeout = None;
        loop {
            match self.queue.wait(timeout) {
                Wait::Stopped => break,
                Wait::Items(items) => {
                    self.process(items, true);
                }
                Wait::Timeout => {
                    self.process(Vec::new(), true);
                }
            }
            if self.stopping.load(SeqCst) {
                break;
            }
            timeout = self.timeout();
        }
        self.stop_engine();
        let (exited, changed) = &*self.exited;
        *lock(exited) = true;
        changed.notify_all();
    }

    fn timeout(&self) -> Option<Duration> {
        let engine = lock(&self.engine);
        let now = engine.now();
        engine
            .next_deadline()
            .map(|deadline| deadline.saturating_sub(now))
    }

    /// Processes items (and, with `timers`, due coalescing windows) on this
    /// thread. Returns the number of items processed.
    pub(crate) fn process(&self, items: Vec<Item>, timers: bool) -> usize {
        let n = items.len();
        let mut engine = lock(&self.engine);
        IN_ENGINE.with(|flag| flag.set(true));
        for item in items {
            if self.stopping.load(SeqCst) {
                break;
            }
            engine.handle(item);
        }
        let fired = if timers && !self.stopping.load(SeqCst) {
            engine.fire_timers()
        } else {
            0
        };
        IN_ENGINE.with(|flag| flag.set(false));
        if self.stopping.load(SeqCst) {
            engine.shutdown();
        }
        n + fired
    }

    /// Drains the queue and processes it on this thread (manual mode).
    pub(crate) fn pump(&self, timers: bool) -> usize {
        let items = self.queue.drain();
        if items.is_empty() && !timers {
            return 0;
        }
        self.process(items, timers)
    }

    #[cfg(test)]
    pub(crate) fn has_timer(&self) -> bool {
        lock(&self.engine).has_timer()
    }

    fn stop_engine(&self) {
        lock(&self.engine).shutdown();
    }

    fn shutdown(&self) {
        self.stopping.store(true, SeqCst);
        drop(self.queue.stop());
        for (shared, _) in lock(&self.registry).values() {
            shared.fail(Failure::Stopped);
        }
        if in_engine() {
            // Called from a `Sink` callback: the processing loop shuts the
            // engine down when it returns.
            return;
        }
        let running = {
            let mut thread = lock(&self.thread);
            let running = matches!(*thread, ThreadState::Running(_));
            if !running {
                *thread = ThreadState::Stopped;
            }
            running
        };
        if running {
            let (exited, changed) = &*self.exited;
            let deadline = Instant::now() + self.flush_timeout;
            let mut done = lock(exited);
            while !*done {
                let now = Instant::now();
                if now >= deadline {
                    // Shutdown returns anyway; the thread stops once the
                    // item it is processing (a sink's delivery, a reflog
                    // read) returns.
                    warn!(
                        "the fs_watch thread did not stop in time";
                        timeout_us = self.flush_timeout,
                    );
                    break;
                }
                done = wait_timeout(changed, done, deadline - now);
            }
        } else {
            self.stop_engine();
        }
    }

    fn acquire_worktree(
        &self,
        common: &std::path::Path,
        name: &WorktreeName,
    ) -> Arc<WorktreeShared> {
        let mut registry = lock(&self.registry);
        let entry = registry
            .entry((common.to_path_buf(), name.clone()))
            .or_insert_with(|| (Arc::new(WorktreeShared::new()), 0));
        entry.1 += 1;
        entry.0.clone()
    }

    fn release_worktree(&self, key: &(PathBuf, WorktreeName), sub: SubId) {
        let mut registry = lock(&self.registry);
        if let Some(entry) = registry.get_mut(key) {
            entry.1 -= 1;
            if entry.1 == 0 {
                registry.remove(key);
            }
        }
        // Queued under the registry lock, so a later handle's `WatchWorktree`
        // for a new state is always processed after this.
        if let Some(refused) = self.queue.push_cmd(Cmd::Unsubscribe(sub)) {
            drop(refused);
        }
    }
}

/// What a `WorktreeWatch` shares with the engine: the revision counter and
/// whether the state is armed.
pub(crate) struct WorktreeShared {
    pub(crate) epoch: u64,
    n: AtomicU64,
    phase: Mutex<Phase>,
    changed: Condvar,
}

pub(crate) enum Phase {
    Arming,
    /// The state's backend watches, for `flush_revision`.
    Watching(Vec<Weak<dyn Armed>>),
    Failed(Failure),
}

impl WorktreeShared {
    fn new() -> WorktreeShared {
        WorktreeShared {
            epoch: NEXT_EPOCH.fetch_add(1, SeqCst),
            n: AtomicU64::new(0),
            phase: Mutex::new(Phase::Arming),
            changed: Condvar::new(),
        }
    }

    pub(crate) fn revision(&self) -> Revision {
        Revision {
            epoch: self.epoch,
            n: self.n.load(SeqCst),
        }
    }

    pub(crate) fn bump(&self) {
        self.n.fetch_add(1, SeqCst);
    }

    pub(crate) fn set_watching(&self, armed: Vec<Weak<dyn Armed>>) {
        let mut phase = lock(&self.phase);
        if !matches!(*phase, Phase::Failed(_)) {
            *phase = Phase::Watching(armed);
            self.changed.notify_all();
        }
    }

    pub(crate) fn fail(&self, failure: Failure) {
        let mut phase = lock(&self.phase);
        if !matches!(*phase, Phase::Failed(_)) {
            *phase = Phase::Failed(failure);
            self.changed.notify_all();
        }
    }

    fn stopped() -> Arc<WorktreeShared> {
        let shared = WorktreeShared::new();
        shared.fail(Failure::Stopped);
        Arc::new(shared)
    }
}

/// Unsubscribes when dropped.
struct Lease {
    shared: Arc<Shared>,
    sub: SubId,
    /// For worktree handles: the registry key to release.
    worktree: Option<(PathBuf, WorktreeName)>,
}

impl Lease {
    fn new(shared: Arc<Shared>, sub: SubId) -> Lease {
        Lease {
            shared,
            sub,
            worktree: None,
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        match &self.worktree {
            Some(key) => self.shared.release_worktree(key, self.sub),
            None => {
                if let Some(refused) = self.shared.queue.push_cmd(Cmd::Unsubscribe(self.sub)) {
                    drop(refused);
                }
            }
        }
    }
}

/// Dropping a handle unsubscribes it; when the last handle of a state is
/// dropped, the fs_watch thread drops its backend watches.
pub struct RepoWatch {
    common: PathBuf,
    lease: Option<Lease>,
}

pub struct WorktreeWatch {
    shared: Option<Arc<Shared>>,
    worktree: Arc<WorktreeShared>,
    _lease: Option<Lease>,
}

pub struct DirWatch {
    _lease: Option<Lease>,
}

impl RepoWatch {
    /// Watches one worktree of this repository. Handles with the same `name`
    /// share one worktree state.
    pub fn watch_worktree(
        &self,
        spec: WorktreeSpec,
        sink: Arc<dyn Sink<WorktreeBatch>>,
    ) -> WorktreeWatch {
        let Some(lease) = &self.lease else {
            return WorktreeWatch::stopped();
        };
        let shared = &lease.shared;
        if shared.stopping.load(SeqCst) {
            return WorktreeWatch::stopped();
        }
        let guard = sink.busy();
        let sub = shared.sub_id();
        let worktree = shared.acquire_worktree(&self.common, &spec.name);
        let key = (self.common.clone(), spec.name.clone());
        let cmd = Cmd::WatchWorktree {
            sub,
            common: self.common.clone(),
            shared: worktree.clone(),
            name: spec.name,
            root: spec.root,
            source: spec.source,
            coalesce: spec.coalesce,
            sink,
            guard,
        };
        if !shared.submit(cmd) {
            // Stopped meanwhile: give the registry entry back without
            // queueing anything.
            let mut registry = lock(&shared.registry);
            if let Some(entry) = registry.get_mut(&key) {
                entry.1 -= 1;
                if entry.1 == 0 {
                    registry.remove(&key);
                }
            }
            return WorktreeWatch::stopped();
        }
        WorktreeWatch {
            shared: Some(shared.clone()),
            worktree,
            _lease: Some(Lease {
                shared: shared.clone(),
                sub,
                worktree: Some(key),
            }),
        }
    }

    /// Replaces this handle's unfiltered path list (relative to the worktree
    /// root, used for every watched worktree of the repository). The
    /// effective list is the union over the repository's handles. Never
    /// blocks; takes effect from the next raw batch.
    pub fn set_unfiltered(&self, paths: Vec<WatchPath>) {
        if let Some(lease) = &self.lease {
            let cmd = Cmd::SetUnfiltered {
                sub: lease.sub,
                paths,
            };
            if let Some(refused) = lease.shared.queue.push_cmd(cmd) {
                drop(refused);
            }
        }
    }
}

impl WorktreeWatch {
    fn stopped() -> WorktreeWatch {
        WorktreeWatch {
            shared: None,
            worktree: WorktreeShared::stopped(),
            _lease: None,
        }
    }

    /// The current revision, without asking the backend to deliver first.
    /// Never blocks.
    pub fn revision(&self) -> Revision {
        self.worktree.revision()
    }

    /// Asks each backend watch of this worktree to deliver what already
    /// happened (`Armed::flush`), waits until the fs_watch thread has
    /// processed every raw batch queued before that, then reads the
    /// revision. Only counts: a batch being coalesced is not delivered early.
    ///
    /// Blocks (under 5 ms with nothing queued); call it only on an executor
    /// thread, never on the UI thread or in a `Sink` callback (that returns
    /// `WouldDeadlock`). While the worktree state is still arming, waits for
    /// that first, within the same timeout.
    pub fn flush_revision(&self) -> Result<Revision, FlushError> {
        if in_engine() {
            return Err(FlushError::WouldDeadlock);
        }
        let Some(shared) = &self.shared else {
            return Err(FlushError::NotWatching(Failure::Stopped));
        };
        let deadline = Instant::now() + shared.flush_timeout;
        let armed = self.wait_armed(shared, deadline)?;
        for armed in armed.iter().filter_map(Weak::upgrade) {
            if let Err(error) = armed.flush() {
                return Err(FlushError::NotWatching(Failure::Backend(error.to_string())));
            }
        }
        let n = shared.fences.next.fetch_add(1, SeqCst) + 1;
        if let Some(refused) = shared.queue.push_cmd(Cmd::Fence(n)) {
            drop(refused);
            return Err(FlushError::NotWatching(Failure::Stopped));
        }
        match shared.mode {
            Mode::Manual => {
                while !shared.fences.is_done(n) {
                    if shared.pump(false) == 0 && !shared.fences.is_done(n) {
                        return Err(FlushError::Timeout);
                    }
                }
            }
            Mode::Thread => {
                let mut done = lock(&shared.fences.done);
                while *done < n {
                    let now = Instant::now();
                    if now >= deadline {
                        return Err(FlushError::Timeout);
                    }
                    done = wait_timeout(&shared.fences.changed, done, deadline - now);
                }
            }
        }
        if let Phase::Failed(failure) = &*lock(&self.worktree.phase) {
            return Err(FlushError::NotWatching(failure.clone()));
        }
        Ok(self.worktree.revision())
    }

    fn wait_armed(
        &self,
        shared: &Shared,
        deadline: Instant,
    ) -> Result<Vec<Weak<dyn Armed>>, FlushError> {
        loop {
            {
                let mut phase = lock(&self.worktree.phase);
                loop {
                    match &*phase {
                        Phase::Watching(armed) => return Ok(armed.clone()),
                        Phase::Failed(failure) => {
                            return Err(FlushError::NotWatching(failure.clone()));
                        }
                        Phase::Arming if shared.mode == Mode::Manual => break,
                        Phase::Arming => {
                            let now = Instant::now();
                            if now >= deadline {
                                return Err(FlushError::Timeout);
                            }
                            phase = wait_timeout(&self.worktree.changed, phase, deadline - now);
                        }
                    }
                }
            }
            // Manual mode: arm on this thread.
            if shared.pump(false) == 0 {
                return Err(FlushError::Timeout);
            }
        }
    }
}

impl fmt::Debug for RepoWatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RepoWatch")
            .field("common", &self.common)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for WorktreeWatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorktreeWatch")
            .field("revision", &self.revision())
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for DirWatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DirWatch").finish_non_exhaustive()
    }
}
