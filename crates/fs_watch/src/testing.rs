//! Test support (`test-support` feature): an in-memory backend, a manually
//! driven watcher with a fake clock, a recording sink and an in-memory
//! filter and config source.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use lectern_executor::Executor;
use lectern_kernel::BusyGuard;

use crate::backend::{Armed, Backend, BackendError, EventSink, Flags, RawBatch, RawEvent, Target};
use crate::clock::Clock;
use crate::path::WatchPath;
use crate::source::{PathFilter, Sink, SourceError, Tracked, WorktreeSource};
use crate::util::lock;
use crate::watcher::{Config, Mode, Shared, Watcher};

// --- FakeBackend -------------------------------------------------------------

struct FakeTarget {
    id: u64,
    target: Target,
    sink: EventSink,
}

#[derive(Default)]
struct FakeState {
    next: u64,
    armed: Vec<FakeTarget>,
    held: HashSet<PathBuf>,
    /// Events held back per armed target, delivered by the next flush.
    pending: HashMap<u64, Vec<RawBatch>>,
    fail_next: HashMap<PathBuf, BackendError>,
    flushes: HashMap<PathBuf, usize>,
    on_arm: HashMap<PathBuf, Vec<RawBatch>>,
    before_arm: HashMap<PathBuf, Vec<ArmHook>>,
    history: Vec<String>,
}

type ArmHook = Box<dyn FnOnce() + Send>;

/// An in-memory backend: records arms, drops and flushes; tests inject raw
/// batches per target. `arm` returns `NotFound` when the path does not exist
/// on disk, as the real backends do.
pub struct FakeBackend {
    me: Weak<FakeBackend>,
    state: Mutex<FakeState>,
}

impl FakeBackend {
    pub fn new() -> Arc<FakeBackend> {
        Arc::new_cyclic(|me| FakeBackend {
            me: me.clone(),
            state: Mutex::new(FakeState::default()),
        })
    }

    /// The targets armed now, by path (then scope).
    pub fn targets(&self) -> Vec<Target> {
        let mut targets: Vec<Target> = lock(&self.state)
            .armed
            .iter()
            .map(|armed| armed.target.clone())
            .collect();
        targets.sort_by(|a, b| {
            a.path
                .cmp(&b.path)
                .then_with(|| format!("{:?}", a.scope).cmp(&format!("{:?}", b.scope)))
        });
        targets
    }

    /// Whether a target with this path is armed.
    pub fn is_armed(&self, target: &Path) -> bool {
        lock(&self.state)
            .armed
            .iter()
            .any(|armed| armed.target.path == target)
    }

    /// Injects one raw batch into every armed target at `target`. Panics
    /// when none is armed.
    pub fn emit(&self, target: &Path, events: &[(&str, Flags)]) {
        self.emit_batch(target, raw(events));
    }

    /// Like `emit`, with a raw batch built by the caller: a benchmark builds
    /// its events outside the clock and times only what fs_watch does with
    /// them.
    pub fn emit_batch(&self, target: &Path, batch: RawBatch) {
        let mut sinks: Vec<EventSink> = {
            let mut state = lock(&self.state);
            let ids: Vec<(u64, EventSink)> = state
                .armed
                .iter()
                .filter(|armed| armed.target.path == target)
                .map(|armed| (armed.id, armed.sink.clone()))
                .collect();
            assert!(
                !ids.is_empty(),
                "no target armed at {}; armed: {:?}",
                target.display(),
                state.armed.iter().map(|a| &a.target).collect::<Vec<_>>()
            );
            if state.held.contains(target) {
                for (id, _) in ids {
                    state.pending.entry(id).or_default().push(batch.clone());
                }
                return;
            }
            ids.into_iter().map(|(_, sink)| sink).collect()
        };
        let last = sinks.pop().expect("at least one target, asserted above");
        for sink in sinks {
            sink.send(batch.clone());
        }
        last.send(batch);
    }

    /// Events injected at `target` from now on stay in the backend until the
    /// next `Armed::flush`, like FSEvents' latency.
    pub fn hold(&self, target: &Path) {
        lock(&self.state).held.insert(target.to_path_buf());
    }

    /// The next `arm` of `target` fails with `error`.
    pub fn fail_next_arm(&self, target: &Path, error: BackendError) {
        lock(&self.state)
            .fail_next
            .insert(target.to_path_buf(), error);
    }

    /// When `target` is next armed, these events are delivered from inside
    /// `arm`, before it returns.
    pub fn emit_on_arm(&self, target: &Path, events: &[(&str, Flags)]) {
        lock(&self.state)
            .on_arm
            .entry(target.to_path_buf())
            .or_default()
            .push(raw(events));
    }

    /// Runs `hook` inside the next `arm` of `target`, before the watch
    /// exists: a change made there happens between whatever the caller did
    /// before arming and the watch starting.
    pub fn before_arm(&self, target: &Path, hook: impl FnOnce() + Send + 'static) {
        lock(&self.state)
            .before_arm
            .entry(target.to_path_buf())
            .or_default()
            .push(Box::new(hook));
    }

    /// How many times the watches at `target` were flushed.
    pub fn flush_count(&self, target: &Path) -> usize {
        lock(&self.state).flushes.get(target).copied().unwrap_or(0)
    }

    /// Every arm and drop so far, as `arm <path> <scope>` / `drop <path>
    /// <scope>` lines.
    pub fn history(&self) -> Vec<String> {
        lock(&self.state).history.clone()
    }

    fn flush(&self, id: u64) {
        let (sink, batches) = {
            let mut state = lock(&self.state);
            let Some(armed) = state.armed.iter().find(|armed| armed.id == id) else {
                return;
            };
            let (path, sink) = (armed.target.path.clone(), armed.sink.clone());
            *state.flushes.entry(path).or_default() += 1;
            (sink, state.pending.remove(&id).unwrap_or_default())
        };
        for batch in batches {
            sink.send(batch);
        }
    }

    fn disarm(&self, id: u64) {
        let mut state = lock(&self.state);
        if let Some(index) = state.armed.iter().position(|armed| armed.id == id) {
            let armed = state.armed.remove(index);
            let line = format!("drop {}", describe(&armed.target));
            state.history.push(line);
            state.pending.remove(&id);
        }
    }
}

fn raw(events: &[(&str, Flags)]) -> RawBatch {
    RawBatch {
        events: events
            .iter()
            .map(|(path, flags)| RawEvent {
                path: path.as_bytes().into(),
                flags: *flags,
            })
            .collect(),
    }
}

fn describe(target: &Target) -> String {
    match &target.scope {
        crate::backend::Scope::Tree => format!("{} tree", target.path.display()),
        crate::backend::Scope::Entries { names } => format!(
            "{} entries {:?}",
            target.path.display(),
            names
                .iter()
                .map(|name| name.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        ),
    }
}

impl Backend for FakeBackend {
    fn arm(&self, target: &Target, sink: EventSink) -> Result<Arc<dyn Armed>, BackendError> {
        let hooks = lock(&self.state)
            .before_arm
            .remove(&target.path)
            .unwrap_or_default();
        for hook in hooks {
            hook();
        }
        let (id, on_arm) = {
            let mut state = lock(&self.state);
            if let Some(error) = state.fail_next.remove(&target.path) {
                state
                    .history
                    .push(format!("fail {}: {error}", describe(target)));
                return Err(error);
            }
            if fs::metadata(&target.path).is_err() {
                return Err(BackendError::NotFound);
            }
            state.next += 1;
            let id = state.next;
            state.armed.push(FakeTarget {
                id,
                target: target.clone(),
                sink: sink.clone(),
            });
            let line = format!("arm {}", describe(target));
            state.history.push(line);
            (id, state.on_arm.remove(&target.path).unwrap_or_default())
        };
        for batch in on_arm {
            sink.send(batch);
        }
        Ok(Arc::new(FakeArmed {
            backend: self.me.clone(),
            id,
        }))
    }
}

struct FakeArmed {
    backend: Weak<FakeBackend>,
    id: u64,
}

impl Armed for FakeArmed {
    fn flush(&self) -> Result<(), BackendError> {
        if let Some(backend) = self.backend.upgrade() {
            backend.flush(self.id);
        }
        Ok(())
    }
}

impl Drop for FakeArmed {
    fn drop(&mut self) {
        if let Some(backend) = self.backend.upgrade() {
            backend.disarm(self.id);
        }
    }
}

// --- the manual watcher --------------------------------------------------------

/// A watcher that starts no thread, with a fake clock. `flush_revision`
/// processes the queue on the calling thread. `executor` should be
/// `Executor::manual()`: `Driver` runs it.
pub fn manual(backend: Arc<dyn Backend>, executor: Executor, config: Config) -> (Watcher, Driver) {
    let clock = Clock::Fake(Arc::new(Mutex::new(Duration::ZERO)));
    let watcher = Watcher::with_mode(
        backend,
        executor.clone(),
        config,
        Mode::Manual,
        clock.clone(),
    );
    let driver = Driver {
        shared: watcher.shared().clone(),
        clock,
        executor,
    };
    (watcher, driver)
}

/// Drives a manual watcher.
pub struct Driver {
    shared: Arc<Shared>,
    clock: Clock,
    executor: Executor,
}

impl Driver {
    /// Processes the fs_watch queue and `executor.run_until_idle()` in turn
    /// until neither has work left. Returns the number of items processed.
    pub fn run_until_idle(&self) -> usize {
        let mut total = 0;
        loop {
            let n = self.shared.pump(true) + self.executor.run_until_idle();
            if n == 0 {
                return total;
            }
            total += n;
        }
    }

    /// Advances the fake clock, delivers the windows that ended, then
    /// `run_until_idle`.
    pub fn advance(&self, by: Duration) {
        self.clock.advance(by);
        self.run_until_idle();
    }

    #[cfg(test)]
    pub(crate) fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    /// Advances the fake clock without processing anything.
    #[cfg(test)]
    pub(crate) fn advance_clock(&self, by: Duration) {
        self.clock.advance(by);
    }
}

// --- RecordingSink -------------------------------------------------------------

type BusyFactory = Arc<dyn Fn() -> Option<BusyGuard> + Send + Sync>;

/// Records the batches it receives and whether each came with a guard. The
/// guards are kept until `take`, as a consumer's task would.
pub struct RecordingSink<B> {
    busy: Option<BusyFactory>,
    batches: Mutex<Vec<(B, Option<BusyGuard>)>>,
}

impl<B: Send + 'static> RecordingSink<B> {
    pub fn new(busy: Option<BusyFactory>) -> Arc<RecordingSink<B>> {
        Arc::new(RecordingSink {
            busy,
            batches: Mutex::new(Vec::new()),
        })
    }

    /// The batches so far, with whether each had a guard; drops the guards.
    pub fn take(&self) -> Vec<(B, bool)> {
        let batches = std::mem::take(&mut *lock(&self.batches));
        batches
            .into_iter()
            .map(|(batch, guard)| (batch, guard.is_some()))
            .collect()
    }

    /// How many batches are waiting to be taken.
    pub fn len(&self) -> usize {
        lock(&self.batches).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<B: Send + 'static> Sink<B> for RecordingSink<B> {
    fn deliver(&self, batch: B, busy: Option<BusyGuard>) {
        lock(&self.batches).push((batch, busy));
    }

    fn busy(&self) -> Option<BusyGuard> {
        self.busy.as_ref().and_then(|busy| busy())
    }
}

// --- FakeSource ----------------------------------------------------------------

#[derive(Default)]
struct SourceState {
    tracked: BTreeSet<Vec<u8>>,
    gitlinks: BTreeSet<Vec<u8>>,
    ignores: Vec<String>,
    rule_files: Vec<WatchPath>,
    config_files: Vec<PathBuf>,
    filter_error: Option<String>,
    config_error: Option<String>,
    gitignores: Vec<Vec<WatchPath>>,
}

/// An in-memory filter and config source. The filter answers from the
/// tracked paths, submodules and ignore patterns set when it was built (so
/// changes here only show after a reload, like a real index):
///
/// - `*.ext` ignores any path whose last component ends with `.ext`;
/// - `name` ignores any path whose last component is `name`; with a
///   trailing `/` only directories;
/// - a pattern with an inner `/` ignores exactly that path.
///
/// Counts the calls and records the `gitignores` argument of each filter
/// load.
#[derive(Default)]
pub struct FakeSource {
    state: Mutex<SourceState>,
    filter_calls: AtomicUsize,
    config_calls: AtomicUsize,
}

impl FakeSource {
    pub fn new() -> Arc<FakeSource> {
        Arc::new(FakeSource::default())
    }

    /// Adds tracked files.
    pub fn track(&self, paths: &[&str]) {
        let mut state = lock(&self.state);
        state
            .tracked
            .extend(paths.iter().map(|path| path.as_bytes().to_vec()));
    }

    /// Removes tracked files.
    pub fn untrack(&self, paths: &[&str]) {
        let mut state = lock(&self.state);
        for path in paths {
            state.tracked.remove(path.as_bytes());
        }
    }

    /// Adds submodules (gitlinks).
    pub fn gitlink(&self, paths: &[&str]) {
        let mut state = lock(&self.state);
        state
            .gitlinks
            .extend(paths.iter().map(|path| path.as_bytes().to_vec()));
    }

    /// Adds ignore patterns.
    pub fn ignore(&self, patterns: &[&str]) {
        let mut state = lock(&self.state);
        state
            .ignores
            .extend(patterns.iter().map(|pattern| (*pattern).to_owned()));
    }

    /// Removes ignore patterns.
    pub fn unignore(&self, patterns: &[&str]) {
        lock(&self.state)
            .ignores
            .retain(|pattern| !patterns.contains(&pattern.as_str()));
    }

    /// Sets the `.gitignore` files the filter reports as read.
    pub fn rule_files(&self, paths: &[&str]) {
        let mut files: Vec<WatchPath> = paths
            .iter()
            .filter_map(|path| WatchPath::new(path.as_bytes()))
            .collect();
        files.sort();
        lock(&self.state).rule_files = files;
    }

    pub fn set_config_files(&self, files: &[PathBuf]) {
        lock(&self.state).config_files = files.to_vec();
    }

    /// Makes later filter loads fail (Some) or succeed (None).
    pub fn fail_filter(&self, error: Option<&str>) {
        lock(&self.state).filter_error = error.map(str::to_owned);
    }

    /// Makes later config loads fail (Some) or succeed (None).
    pub fn fail_config(&self, error: Option<&str>) {
        lock(&self.state).config_error = error.map(str::to_owned);
    }

    pub fn filter_calls(&self) -> usize {
        self.filter_calls.load(SeqCst)
    }

    pub fn config_calls(&self) -> usize {
        self.config_calls.load(SeqCst)
    }

    /// The `gitignores` argument of every filter load so far.
    pub fn gitignores(&self) -> Vec<Vec<WatchPath>> {
        lock(&self.state).gitignores.clone()
    }
}

impl WorktreeSource for FakeSource {
    fn filter(
        &self,
        gitignores: &[WatchPath],
        _cancel: &AtomicBool,
    ) -> Result<Arc<dyn PathFilter>, SourceError> {
        self.filter_calls.fetch_add(1, SeqCst);
        let mut state = lock(&self.state);
        state.gitignores.push(gitignores.to_vec());
        if let Some(error) = &state.filter_error {
            return Err(SourceError(error.clone()));
        }
        let mut dirs = BTreeSet::new();
        for path in state.tracked.iter().chain(state.gitlinks.iter()) {
            let mut end = path.len();
            while let Some(slash) = path[..end].iter().rposition(|&b| b == b'/') {
                dirs.insert(path[..slash].to_vec());
                end = slash;
            }
        }
        Ok(Arc::new(FakeFilter {
            tracked: state.tracked.clone(),
            dirs,
            gitlinks: state.gitlinks.clone(),
            ignores: state.ignores.clone(),
            rule_files: state.rule_files.clone(),
        }))
    }

    fn config_files(&self, _cancel: &AtomicBool) -> Result<Vec<PathBuf>, SourceError> {
        self.config_calls.fetch_add(1, SeqCst);
        let state = lock(&self.state);
        match &state.config_error {
            Some(error) => Err(SourceError(error.clone())),
            None => Ok(state.config_files.clone()),
        }
    }
}

struct FakeFilter {
    tracked: BTreeSet<Vec<u8>>,
    dirs: BTreeSet<Vec<u8>>,
    gitlinks: BTreeSet<Vec<u8>>,
    ignores: Vec<String>,
    rule_files: Vec<WatchPath>,
}

impl PathFilter for FakeFilter {
    fn tracked(&self, path: &[u8]) -> Tracked {
        if self.gitlinks.contains(path) {
            Tracked::Gitlink
        } else if self.tracked.contains(path) {
            Tracked::File
        } else if self.dirs.contains(path) {
            Tracked::Dir
        } else {
            Tracked::No
        }
    }

    fn ignored(&self, path: &[u8], is_dir: bool) -> bool {
        let name = crate::path::file_name(path);
        self.ignores.iter().any(|pattern| {
            let (pattern, dir_only) = match pattern.strip_suffix('/') {
                Some(pattern) => (pattern, true),
                None => (pattern.as_str(), false),
            };
            if dir_only && !is_dir {
                return false;
            }
            if let Some(ext) = pattern.strip_prefix("*.") {
                name.len() > ext.len()
                    && name.ends_with(ext.as_bytes())
                    && name[name.len() - ext.len() - 1] == b'.'
            } else if pattern.contains('/') {
                path == pattern.as_bytes()
            } else {
                name == pattern.as_bytes()
            }
        })
    }

    fn rule_files(&self) -> &[WatchPath] {
        &self.rule_files
    }
}
