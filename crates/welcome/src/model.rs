//! The snapshot the Welcome screen reads and the model that owns it
//! (docs/design/welcome.md「接口」「数据流与线程」).
//!
//! Every method here is callable from any thread, only touches memory under
//! a short lock and returns at once. The files and the session lists are
//! handled in tasks that go through `PluginHandle::own_with` to the
//! executor, each holding a `BusyGuard`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Instant;

use lectern_executor::{Executor, Priority};
use lectern_kernel::{PluginHandle, ServiceRef, ServiceSet};
use lectern_logging::{Value, debug, info, warn};
use lectern_services::project::{OpenError, ProjectEvent, ProjectKey, Projects};
use lectern_services::session::{SessionList, SessionSource, Timestamp};

use crate::discover::{self, DiscoveredFolder};
use crate::recent::store::{self, Unreadable};
use crate::recent::{self, RecentEntry};

/// What the view reads. Replaced as a whole, as a new `Arc`, on every change.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WelcomeSnapshot {
    /// Incremented on every replacement.
    pub generation: u64,
    pub recent: RecentState,
    pub discovered: DiscoveredState,
    /// An open this screen started that has not seen its `Opened` yet:
    /// [`WelcomeModel::begin_open`] sets it, `Opened` (either result) clears
    /// it. The view does not send the same path twice meanwhile.
    pub opening: Option<Opening>,
    /// The line under the button; cleared by the next `begin_open`.
    pub notice: Option<Notice>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Opening {
    pub path: PathBuf,
    pub source: OpenSource,
}

/// Where an open came from; the `source` field of the log record.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpenSource {
    Recent,
    Discovered,
    Prompt,
}

impl OpenSource {
    fn name(self) -> &'static str {
        match self {
            OpenSource::Recent => "recent",
            OpenSource::Discovered => "discovered",
            OpenSource::Prompt => "prompt",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Notice {
    /// The kind of a failed `ProjectEvent::Opened`.
    OpenFailed(OpenFailure),
    /// The language preference could not be written.
    LanguageNotSaved,
}

/// An `OpenError` without its text: one of the five `welcome-open-failed-*`
/// strings.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpenFailure {
    NotFound,
    NotADirectory,
    PermissionDenied,
    RepoUnreadable,
    Io,
}

impl OpenFailure {
    pub(crate) fn of(error: &OpenError) -> OpenFailure {
        match error {
            OpenError::NotFound => OpenFailure::NotFound,
            OpenError::NotADirectory => OpenFailure::NotADirectory,
            OpenError::PermissionDenied => OpenFailure::PermissionDenied,
            OpenError::RepoUnreadable(_) => OpenFailure::RepoUnreadable,
            OpenError::Io(_) => OpenFailure::Io,
        }
    }

    /// The value of the `error` log field.
    fn name(self) -> &'static str {
        match self {
            OpenFailure::NotFound => "not-found",
            OpenFailure::NotADirectory => "not-a-directory",
            OpenFailure::PermissionDenied => "permission-denied",
            OpenFailure::RepoUnreadable => "repo-unreadable",
            OpenFailure::Io => "io",
        }
    }

    /// Whether the folder itself is gone: the entry is marked missing.
    fn folder_gone(self) -> bool {
        matches!(self, OpenFailure::NotFound | OpenFailure::NotADirectory)
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RecentState {
    /// The file is not read yet (from `started` until the first task ends).
    Loading,
    /// Newest first, at most 10.
    Ready(Arc<[RecentEntry]>),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DiscoveredState {
    /// `welcome-discovery` is not active (the initial value, and the value
    /// after it is deactivated). The view shows the empty state, no skeleton.
    Unavailable,
    /// Active, the first rebuild is not done.
    Pending,
    /// Newest activity first, at most 50.
    Ready(Arc<[DiscoveredFolder]>),
}

/// A change to the list, kept until the file is read when it comes earlier.
enum Change {
    Record(RecentEntry),
    Remove(PathBuf),
}

impl Change {
    fn apply(self, entries: &mut Vec<RecentEntry>) -> bool {
        match self {
            Change::Record(entry) => {
                recent::push_front(entries, entry);
                true
            }
            Change::Remove(path) => recent::remove(entries, &path),
        }
    }
}

/// The state of a kind of work that runs as one task at a time: the flags are
/// read and set under the model's lock.
#[derive(Default)]
struct Pump {
    /// A task is submitted or running.
    queued: bool,
    /// Something changed after the running task took its input.
    dirty: bool,
}

struct WelcomeSide {
    handle: PluginHandle,
    projects: ServiceRef<dyn Projects>,
}

#[derive(Clone)]
struct DiscoverySide {
    handle: PluginHandle,
    projects: ServiceRef<dyn Projects>,
    sources: ServiceSet<dyn SessionSource>,
}

struct State {
    snapshot: Arc<WelcomeSnapshot>,
    entries: Vec<RecentEntry>,
    /// The file is read. Until then changes only queue.
    loaded: bool,
    queue: Vec<Change>,
    discovered: DiscoveredState,
    opening: Option<Opening>,
    notice: Option<Notice>,
    write: Pump,
    rebuild: Pump,
    welcome: Option<WelcomeSide>,
    discovery: Option<DiscoverySide>,
}

type Wake = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct Subscribers {
    next: u64,
    list: Vec<(u64, Wake)>,
}

/// Unregisters a wake callback when dropped.
pub struct WakeGuard {
    subscribers: Weak<Mutex<Subscribers>>,
    id: u64,
}

impl Drop for WakeGuard {
    fn drop(&mut self) {
        if let Some(subscribers) = self.subscribers.upgrade() {
            lock(&subscribers).list.retain(|(id, _)| *id != self.id);
        }
    }
}

type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// Holds the snapshot. `Send + Sync`: both plugins and the view share one
/// `Arc`.
pub struct WelcomeModel {
    state: Mutex<State>,
    subscribers: Arc<Mutex<Subscribers>>,
    executor: Executor,
    data_dir: PathBuf,
    /// UTC milliseconds.
    clock: Clock,
    me: Weak<WelcomeModel>,
    #[cfg(test)]
    pub(crate) stats: Stats,
}

/// What the tests count.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct Stats {
    pub(crate) rebuilds: std::sync::atomic::AtomicUsize,
    pub(crate) writes: std::sync::atomic::AtomicUsize,
    pub(crate) wakes: std::sync::atomic::AtomicUsize,
    /// Runs once, in the next write task, after it took the list and before
    /// it writes: a change "while the write runs".
    pub(crate) during_write: Mutex<Option<Hook>>,
    /// Runs once, in the next rebuild task, after it took its input and
    /// before it applies the result.
    pub(crate) during_rebuild: Mutex<Option<Hook>>,
}

#[cfg(test)]
pub(crate) type Hook = Box<dyn FnOnce() + Send>;

#[cfg(test)]
impl Stats {
    fn fire(hook: &Mutex<Option<Hook>>) {
        let hook = lock(hook).take();
        if let Some(hook) = hook {
            hook();
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn system_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

impl WelcomeModel {
    /// Builds the empty model; no IO.
    pub(crate) fn new(executor: Executor, data_dir: PathBuf) -> Arc<WelcomeModel> {
        WelcomeModel::with_clock(executor, data_dir, Arc::new(system_millis))
    }

    pub(crate) fn with_clock(
        executor: Executor,
        data_dir: PathBuf,
        clock: Clock,
    ) -> Arc<WelcomeModel> {
        Arc::new_cyclic(|me| {
            let state = State {
                snapshot: Arc::new(WelcomeSnapshot {
                    generation: 0,
                    recent: RecentState::Loading,
                    discovered: DiscoveredState::Unavailable,
                    opening: None,
                    notice: None,
                }),
                entries: Vec::new(),
                loaded: false,
                queue: Vec::new(),
                discovered: DiscoveredState::Unavailable,
                opening: None,
                notice: None,
                write: Pump::default(),
                rebuild: Pump::default(),
                welcome: None,
                discovery: None,
            };
            WelcomeModel {
                state: Mutex::new(state),
                subscribers: Arc::default(),
                executor,
                data_dir,
                clock,
                me: me.clone(),
                #[cfg(test)]
                stats: Stats::default(),
            }
        })
    }

    /// UTC milliseconds by the model's clock: the view's "now" for the
    /// relative times, so that a test fixes it with the clock.
    pub(crate) fn now_millis(&self) -> i64 {
        (self.clock)()
    }

    /// The current snapshot: copies an `Arc` under the lock.
    pub fn snapshot(&self) -> Arc<WelcomeSnapshot> {
        lock(&self.state).snapshot.clone()
    }

    /// Registers `wake`, called once after every replacement of the
    /// snapshot, on the thread that replaced it and without any lock held.
    /// The view registers when it is built; dropping the guard unregisters.
    pub fn subscribe(&self, wake: Box<dyn Fn() + Send + Sync>) -> WakeGuard {
        let mut subscribers = lock(&self.subscribers);
        subscribers.next += 1;
        let id = subscribers.next;
        subscribers.list.push((id, Arc::from(wake)));
        WakeGuard {
            subscribers: Arc::downgrade(&self.subscribers),
            id,
        }
    }

    /// Before the view dispatches an open of `path`: clears the notice and
    /// records the open in flight.
    pub fn begin_open(&self, path: PathBuf, source: OpenSource) {
        let mut state = lock(&self.state);
        state.notice = None;
        state.opening = Some(Opening { path, source });
        self.publish(state);
    }

    /// Clears the notice (Open Folder and its shortcut do not go through
    /// `begin_open`: the path comes from the system panel).
    pub fn clear_notice(&self) {
        let mut state = lock(&self.state);
        state.notice = None;
        self.publish(state);
    }

    /// Removes a recent project: the list and the snapshot change at once
    /// in the calling thread, the file follows in a task. A path that is not
    /// in the list is ignored.
    pub fn remove_recent(&self, path: &Path) {
        let mut state = lock(&self.state);
        if !state.loaded {
            state.queue.push(Change::Remove(path.to_path_buf()));
            return;
        }
        if !recent::remove(&mut state.entries, path) {
            return;
        }
        debug!("recent entry removed");
        self.publish(state);
        self.schedule_write();
        self.request_rebuild();
    }

    /// The language preference could not be written.
    pub fn language_not_saved(&self) {
        warn!("language override not saved"; error = "write-failed");
        let mut state = lock(&self.state);
        state.notice = Some(Notice::LanguageNotSaved);
        self.publish(state);
    }

    /// Replaces the snapshot when something the view reads changed, then
    /// wakes the subscribers outside the lock. Returns whether it changed.
    fn publish(&self, mut state: MutexGuard<'_, State>) -> bool {
        let recent = if state.loaded {
            RecentState::Ready(state.entries.iter().cloned().collect())
        } else {
            RecentState::Loading
        };
        let old = &state.snapshot;
        if old.recent == recent
            && old.discovered == state.discovered
            && old.opening == state.opening
            && old.notice == state.notice
        {
            return false;
        }
        let snapshot = Arc::new(WelcomeSnapshot {
            generation: old.generation + 1,
            recent,
            discovered: state.discovered.clone(),
            opening: state.opening.clone(),
            notice: state.notice,
        });
        state.snapshot = snapshot;
        drop(state);
        let wakes: Vec<Wake> = lock(&self.subscribers)
            .list
            .iter()
            .map(|(_, wake)| wake.clone())
            .collect();
        #[cfg(test)]
        self.stats
            .wakes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        for wake in wakes {
            wake();
        }
        true
    }

    /// Submits `work` to the executor through `handle`, with a `BusyGuard`.
    /// False when the activation is gone.
    fn spawn(
        &self,
        handle: &PluginHandle,
        work: impl FnOnce(&WelcomeModel) + Send + 'static,
    ) -> bool {
        let Some(busy) = handle.busy() else {
            return false;
        };
        let executor = self.executor.clone();
        let me = self.me.clone();
        handle.own_with(move |gate| {
            executor.run_gated(&gate, Priority::Background, move || {
                let _busy = busy;
                if let Some(model) = me.upgrade() {
                    work(&model);
                }
            })
        })
    }

    // The "welcome" plugin.

    /// `started` of "welcome": keeps the handle and reads the file.
    pub(crate) fn welcome_started(&self, handle: PluginHandle, projects: ServiceRef<dyn Projects>) {
        lock(&self.state).welcome = Some(WelcomeSide {
            handle: handle.clone(),
            projects,
        });
        if !self.spawn(&handle, WelcomeModel::load) {
            lock(&self.state).welcome = None;
        }
    }

    /// "welcome" was deactivated: nothing is submitted any more.
    pub(crate) fn welcome_stopped(&self) {
        let mut state = lock(&self.state);
        state.welcome = None;
        state.write = Pump::default();
    }

    /// Step 1: reads the file, applies what queued meanwhile, records the
    /// projects that opened before the plugin went live.
    fn load(&self) {
        let started = Instant::now();
        let read = store::read(&self.data_dir);
        let projects = lock(&self.state)
            .welcome
            .as_ref()
            .map(|side| side.projects.clone());
        let now = (self.clock)();
        let open_now: Vec<RecentEntry> = projects
            .and_then(|projects| {
                let projects = projects.get()?;
                Some(
                    projects
                        .open_projects()
                        .iter()
                        .filter_map(|key| projects.snapshot(key))
                        .map(|snapshot| recent::entry_from_snapshot(&snapshot, now))
                        .collect(),
                )
            })
            .unwrap_or_default();
        let entries = match read {
            store::Read::Missing => Vec::new(),
            store::Read::Loaded(entries) => {
                info!(
                    "recent list loaded";
                    entries = entries.len(),
                    took_us = started.elapsed()
                );
                entries
            }
            store::Read::Unreadable(Unreadable::Io(error)) => {
                warn!("recent list unreadable"; reason = "io", io = Value::io(&error));
                Vec::new()
            }
            store::Read::Unreadable(Unreadable::Json) => {
                warn!("recent list unreadable"; reason = "json");
                Vec::new()
            }
            store::Read::Unreadable(Unreadable::Version(version)) => {
                warn!("recent list unreadable"; reason = "version", version = version);
                Vec::new()
            }
        };
        let mut state = lock(&self.state);
        state.entries = entries;
        state.loaded = true;
        let queue = std::mem::take(&mut state.queue);
        let changed = !queue.is_empty() || !open_now.is_empty();
        for change in queue {
            change.apply(&mut state.entries);
        }
        for entry in open_now {
            recent::push_front(&mut state.entries, entry);
        }
        self.publish(state);
        if changed {
            self.schedule_write();
        }
        self.request_rebuild();
    }

    /// `ProjectEvent` for "welcome": a successful open goes to a task, a
    /// failed one only changes memory.
    pub(crate) fn welcome_project_event(&self, event: &ProjectEvent, handle: &PluginHandle) {
        let ProjectEvent::Opened { result, .. } = event else {
            return;
        };
        match result {
            Ok(key) => {
                let key = key.clone();
                self.spawn(handle, move |model| model.record_opened(&key));
            }
            Err(error) => self.open_failed(error),
        }
    }

    /// Step 2: a project opened.
    pub(crate) fn record_opened(&self, key: &ProjectKey) {
        let projects = lock(&self.state)
            .welcome
            .as_ref()
            .map(|side| side.projects.clone());
        let snapshot = projects.and_then(|projects| projects.get()?.snapshot(key));
        let entry = snapshot.map(|snapshot| recent::entry_from_snapshot(&snapshot, (self.clock)()));
        if entry.is_none() {
            debug!("opened project has no snapshot");
        }
        let mut state = lock(&self.state);
        let opening = state.opening.take();
        let recorded = entry.is_some();
        if let Some(entry) = entry {
            if state.loaded {
                recent::push_front(&mut state.entries, entry);
            } else {
                state.queue.push(Change::Record(entry));
            }
        }
        let loaded = state.loaded;
        self.publish(state);
        if let Some(opening) = opening {
            info!("project opened from welcome"; source = opening.source.name());
        }
        if recorded && loaded {
            self.schedule_write();
            self.request_rebuild();
        }
    }

    /// `Opened { Err }`: the notice, the `missing` mark, `opening` cleared.
    fn open_failed(&self, error: &OpenError) {
        let failure = OpenFailure::of(error);
        let mut state = lock(&self.state);
        let from_welcome = state.opening.is_some();
        if let Some(opening) = state.opening.take()
            && failure.folder_gone()
        {
            for entry in state.entries.iter_mut().filter(|e| e.path == opening.path) {
                entry.missing = true;
            }
        }
        state.notice = Some(Notice::OpenFailed(failure));
        self.publish(state);
        warn!("project open failed"; error = failure.name(), from_welcome = from_welcome);
    }

    /// Asks for the file to be written: at most one write task at any time,
    /// and a change that arrives meanwhile makes it run once more.
    fn schedule_write(&self) {
        let handle = {
            let mut state = lock(&self.state);
            if !state.loaded {
                return;
            }
            let Some(side) = &state.welcome else {
                return;
            };
            let handle = side.handle.clone();
            state.write.dirty = true;
            if state.write.queued {
                return;
            }
            state.write.queued = true;
            handle
        };
        if !self.spawn(&handle, WelcomeModel::write_task) {
            lock(&self.state).write.queued = false;
        }
    }

    fn write_task(&self) {
        loop {
            let (bytes, skipped) = {
                let mut state = lock(&self.state);
                state.write.dirty = false;
                recent::encode(&state.entries)
            };
            for _ in 0..skipped {
                warn!("recent entry skipped"; reason = "non-utf8-path");
            }
            #[cfg(test)]
            {
                self.stats
                    .writes
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Stats::fire(&self.stats.during_write);
            }
            match store::write(&self.data_dir, &bytes) {
                Ok(()) => debug!("recent list written"; bytes = bytes.len()),
                Err((step, error)) => {
                    warn!("recent list not written"; step = step.name(), io = Value::io(&error));
                }
            }
            let mut state = lock(&self.state);
            if state.write.dirty && state.welcome.is_some() {
                continue;
            }
            state.write.queued = false;
            return;
        }
    }

    // The "welcome-discovery" plugin.

    /// `started` of "welcome-discovery": the list is pending until the first
    /// rebuild is done.
    pub(crate) fn discovery_started(
        &self,
        handle: PluginHandle,
        projects: ServiceRef<dyn Projects>,
        sources: ServiceSet<dyn SessionSource>,
    ) {
        let mut state = lock(&self.state);
        state.discovery = Some(DiscoverySide {
            handle,
            projects,
            sources,
        });
        state.discovered = DiscoveredState::Pending;
        self.publish(state);
        self.request_rebuild();
    }

    /// "welcome-discovery" was deactivated: the list is unavailable and the
    /// counts on the recent rows go back to 0.
    pub(crate) fn discovery_stopped(&self) {
        let mut state = lock(&self.state);
        state.discovery = None;
        state.rebuild = Pump::default();
        for entry in &mut state.entries {
            entry.sessions = 0;
            entry.running = 0;
        }
        let was_available = state.discovered != DiscoveredState::Unavailable;
        state.discovered = DiscoveredState::Unavailable;
        self.publish(state);
        if was_available {
            info!("discovery unavailable");
        }
    }

    /// Asks for a rebuild of the discovered list: at most one rebuild task
    /// at any time, and a request that arrives meanwhile makes it run once
    /// more.
    pub(crate) fn request_rebuild(&self) {
        let handle = {
            let mut state = lock(&self.state);
            let Some(side) = &state.discovery else {
                return;
            };
            let handle = side.handle.clone();
            state.rebuild.dirty = true;
            if state.rebuild.queued {
                return;
            }
            state.rebuild.queued = true;
            handle
        };
        if !self.spawn(&handle, WelcomeModel::rebuild_task) {
            lock(&self.state).rebuild.queued = false;
        }
    }

    /// Step 3.
    fn rebuild_task(&self) {
        loop {
            let started = Instant::now();
            let (side, recent_keys) = {
                let mut state = lock(&self.state);
                state.rebuild.dirty = false;
                let Some(side) = state.discovery.clone() else {
                    state.rebuild.queued = false;
                    return;
                };
                let keys: Vec<ProjectKey> = state.entries.iter().map(|e| e.key.clone()).collect();
                (side, keys)
            };
            let mut lists: Vec<Arc<SessionList>> = Vec::new();
            side.sources
                .for_each(|source| lists.push(source.sessions()));
            let open = side
                .projects
                .get()
                .map(|projects| projects.open_projects())
                .unwrap_or_default();
            let built = discover::build(&lists, &open, &recent_keys, Timestamp((self.clock)()));
            drop(lists);
            #[cfg(test)]
            {
                self.stats
                    .rebuilds
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Stats::fire(&self.stats.during_rebuild);
            }
            {
                let mut state = lock(&self.state);
                if state.discovery.is_none() {
                    state.rebuild.queued = false;
                    return;
                }
                for entry in &mut state.entries {
                    let (sessions, running) =
                        built.counts.get(&entry.key).copied().unwrap_or((0, 0));
                    entry.sessions = sessions;
                    entry.running = running;
                }
                let folders = built.folders.len();
                state.discovered = DiscoveredState::Ready(built.folders.into());
                let recent = state.entries.len();
                self.publish(state);
                debug!(
                    "snapshot rebuilt";
                    recent = recent,
                    discovered = folders,
                    sessions = built.total,
                    skipped = built.skipped,
                    took_us = started.elapsed()
                );
            }
            let mut state = lock(&self.state);
            if state.rebuild.dirty && state.discovery.is_some() {
                continue;
            }
            state.rebuild.queued = false;
            return;
        }
    }
}
