//! The plugin's engine: opening the database, the reading cadence, the
//! watched snapshots and the `dyn Usage` service (docs/design/usage.md
//! 「数据流与线程」).
//!
//! - Nothing happens before `started`: the database opens in the first task.
//! - Reading: one batch at a time. Active sessions (`open_turn`, or activity
//!   in the last 10 minutes) are read batch after batch to the end; the
//!   others are backfilled newest first, one batch per stretch of 10 seconds
//!   of kernel quiescence. Every batch holds a `BusyGuard` until it commits.
//! - Snapshots: one per watched query, recomputed in the background after
//!   every commit, after a project opens or closes, and at the next local
//!   midnight; `snapshot` only copies an `Arc`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use lectern_executor::{Executor, Priority};
use lectern_kernel::{ActivationToken, PluginHandle, ServiceRef, ServiceSet};
use lectern_logging::{Ctx, Value, debug, error, info, warn};
use lectern_services::project::{ProjectEvent, ProjectKey, Projects};
use lectern_services::session::{
    Batch, Place as SessionPlace, Record, SessionError, SessionKey, SessionSource, SessionSummary,
    SessionsChanged, SourceId,
};
use lectern_services::usage::{
    AnalysisId, AnalysisStart, Limits, LocalDate, MeterEvent, MrSource, MrSourceRegistration,
    ProjectInfo, ProjectRef, ProjectStatus, SnapshotHeader, Usage, UsageCommand, UsageDegraded,
    UsageError, UsageEvent, UsageInterest, UsageProjectId, UsageQuery, UsageSnapshot, UsageState,
};

use crate::LocalZone;
use crate::snapshot::{self, Applied, Owners, Watched};
use crate::store::cold::{self, Month, Stepped};
use crate::store::{
    Checked, FileStamp, Ingest, Ingested, Mode, Place, ProjectRow, SessionInfo, Store, StoreError,
    Totals,
};
use crate::time::Time;
use crate::window;

/// `max_bytes` of each `SessionSource::read` (agents design「接口」).
pub(crate) const READ_BYTES: u64 = 8 << 20;

/// A session with activity this recent is read without pacing.
pub(crate) const ACTIVE_MS: i64 = 10 * 60 * 1000;

/// Quiescence the kernel must keep before the next backfill batch.
pub(crate) const PACE: Duration = Duration::from_secs(10);

/// Most cwd paths whose place is remembered.
const PLACE_CACHE: usize = 4096;

pub(crate) struct Deps {
    pub(crate) executor: Executor,
    pub(crate) data_dir: PathBuf,
    pub(crate) zone: Arc<dyn LocalZone>,
    pub(crate) time: Time,
}

enum Db {
    Closed,
    Open(Arc<Store>),
    Failed(Arc<str>),
}

struct Interest {
    count: usize,
    snapshot: Option<Arc<UsageSnapshot>>,
    /// Computed again from the database by the next snapshot task.
    dirty: bool,
    /// A snapshot task is computing it: deltas that arrive meanwhile are not
    /// applied, the query is computed again instead.
    computing: bool,
    /// The accumulators of a window query, which later commits' fact
    /// deltas update in place.
    watched: Option<Watched>,
}

#[derive(Default)]
struct Pace {
    /// One backfill batch may start.
    allowed: bool,
    /// The kernel has been quiescent since this `Quiescent` (cleared by
    /// `Busy`).
    quiet: Option<u64>,
    max_seq: u64,
    /// A pacing timer runs for this quiescent period.
    timer: Option<u64>,
    /// `quiet` once its `Quiescent` has been handled, timer armed.
    handled: Option<u64>,
}

/// Backfill progress over the inactive sessions found changed since the
/// last run (or since the last backfill finished).
#[derive(Default)]
struct Backfill {
    /// Per session: file size when found changed, bytes read so far.
    sessions: HashMap<SessionKey, (u64, u64)>,
    started: Option<Instant>,
}

impl Backfill {
    fn progress(&self) -> (u64, u64) {
        self.sessions.values().fold((0, 0), |(d, t), (size, read)| {
            (d + read.min(size), t + size)
        })
    }
}

/// The archive and the space reclaim (docs/design/usage.md「冷热分层」):
/// background work with the backfill's cadence, one step per quiet stretch.
#[derive(Default)]
struct Maintenance {
    /// Ask the store which months to archive (after opening, and at every
    /// local midnight after that).
    check: bool,
    months: VecDeque<Month>,
    /// Free pages may be over the thresholds.
    reclaim: bool,
    /// The next local midnight a check is armed for.
    midnight: Option<i64>,
}

impl Maintenance {
    fn pending(&self) -> bool {
        self.check || !self.months.is_empty() || self.reclaim
    }
}

#[derive(Default)]
struct State {
    handle: Option<PluginHandle>,
    stopped: bool,
    /// Sessions with records not yet read.
    dirty: HashSet<SessionKey>,
    /// Sessions that may have changed: their log file is compared with the
    /// one the last read to the end saw before they become dirty.
    unverified: HashSet<SessionKey>,
    verifying: bool,
    /// Unchanged sessions read once anyway, unpaced, one per source and
    /// run: a read is the only way to learn that the source's adapter
    /// version changed (it then replays from the start).
    probe: HashSet<SessionKey>,
    probed: HashSet<SourceId>,
    /// Sources whose sessions were all queued again after an adapter change.
    replayed: HashSet<SourceId>,
    reading: bool,
    /// A write failed: no more reading until the next `SessionsChanged`.
    blocked: bool,
    pace: Pace,
    backfill: Backfill,
    maintenance: Maintenance,
    places: HashMap<PathBuf, Place>,
    interests: HashMap<UsageQuery, Interest>,
    computing: bool,
    generation: u64,
    open: Vec<ProjectKey>,
    /// The next local midnight a timer is armed for.
    midnight: Option<i64>,
    needs_check: bool,
    checking: bool,
    recovered: Option<i64>,
}

pub(crate) struct Engine {
    me: Weak<Engine>,
    deps: Deps,
    sources: ServiceSet<dyn SessionSource>,
    projects: ServiceRef<dyn Projects>,
    db: Mutex<Db>,
    state: Mutex<State>,
    limits: Arc<Limits>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The 128-bit tag of a session ID for the log scope: the UUID itself when
/// the ID is one (Claude Code and Codex IDs are), a hash otherwise.
fn session_tag(id: &str) -> [u8; 16] {
    let hex: Vec<u8> = id.bytes().filter(|b| *b != b'-').collect();
    if hex.len() == 32 && hex.iter().all(u8::is_ascii_hexdigit) {
        let mut out = [0u8; 16];
        for (i, pair) in hex.chunks(2).enumerate() {
            let digit = |b: u8| (b as char).to_digit(16).unwrap_or(0) as u8;
            out[i] = digit(pair[0]) << 4 | digit(pair[1]);
        }
        return out;
    }
    use std::hash::{Hash, Hasher};
    let mut out = [0u8; 16];
    for (i, half) in out.chunks_mut(8).enumerate() {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (i, id).hash(&mut h);
        half.copy_from_slice(&h.finish().to_le_bytes());
    }
    out
}

pub(crate) fn place_of_key(key: &ProjectKey, rel: &str) -> Place {
    match key {
        ProjectKey::Git { common_dir } => Place::Git(common_dir.to_path_buf()),
        ProjectKey::Folder { root } if rel.is_empty() => Place::Dir(root.to_path_buf()),
        ProjectKey::Folder { root } => Place::Dir(root.join(rel)),
    }
}

impl Engine {
    pub(crate) fn new(
        deps: Deps,
        sources: ServiceSet<dyn SessionSource>,
        projects: ServiceRef<dyn Projects>,
    ) -> Arc<Engine> {
        Arc::new_cyclic(|me| Engine {
            me: me.clone(),
            deps,
            sources,
            projects,
            db: Mutex::new(Db::Closed),
            state: Mutex::new(State::default()),
            limits: Arc::new(Limits::default()),
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    pub(crate) fn store(&self) -> Option<Arc<Store>> {
        match &*lock(&self.db) {
            Db::Open(store) => Some(store.clone()),
            _ => None,
        }
    }

    /// `started`: the first task opens the database.
    pub(crate) fn start(&self, handle: PluginHandle) {
        self.state().handle = Some(handle);
        self.spawn(true, |engine| engine.open());
    }

    /// Deactivation: no more tasks; the database is marked closed normally.
    pub(crate) fn stop(&self) {
        {
            let mut state = self.state();
            state.stopped = true;
            state.handle = None;
        }
        if let Some(store) = self.store() {
            store.close();
        }
    }

    /// Runs `work` on the executor, with a `BusyGuard` when `busy`. Returns
    /// false once the plugin has stopped.
    fn spawn(&self, busy: bool, work: impl FnOnce(&Engine) + Send + 'static) -> bool {
        let handle = {
            let state = self.state();
            if state.stopped {
                return false;
            }
            match &state.handle {
                Some(h) => h.clone(),
                None => return false,
            }
        };
        let guard = if busy {
            match handle.busy() {
                Some(g) => Some(g),
                None => return false,
            }
        } else {
            None
        };
        let executor = self.deps.executor.clone();
        let me = self.me.clone();
        handle.own_with(move |gate| {
            executor.run_gated(&gate, Priority::Background, move || {
                let _guard = guard;
                if let Some(engine) = me.upgrade() {
                    work(&engine);
                }
            })
        })
    }

    /// Wraps a timer callback: it runs only while the engine lives and the
    /// plugin has not stopped. It must not block (it runs on the clock's
    /// timer thread); work goes to `spawn`.
    fn timer(&self, then: impl FnOnce(&Engine) + Send + 'static) -> impl FnOnce() + Send + 'static {
        let me = self.me.clone();
        move || {
            let Some(engine) = me.upgrade() else {
                return;
            };
            let stopped = engine.state().stopped;
            if !stopped {
                then(&engine);
            }
        }
    }

    fn open(&self) {
        let now = self.deps.time.now_ms();
        match Store::open(&self.deps.data_dir, now) {
            Ok(opened) => {
                info!(
                    "usage database ready";
                    migrated_from = opened.migrated_from,
                    needs_check = opened.needs_check,
                    recovered = opened.recovered.is_some()
                );
                let store = Arc::new(opened.store);
                *lock(&self.db) = Db::Open(store.clone());
                let mut state = self.state();
                state.needs_check = opened.needs_check;
                state.recovered = opened.recovered.map(|r| r.at);
                state.maintenance.check = true;
                drop(state);
                self.register_open_projects(&store);
                let keys: Vec<SessionKey> = self.summaries().into_keys().collect();
                self.state().unverified.extend(keys);
                self.verify_now();
            }
            Err(e) => {
                let reason = e.reason();
                error!("database could not be opened"; path = Value::path(&self.deps.data_dir));
                *lock(&self.db) = Db::Failed(reason);
            }
        }
        self.mark_all_dirty();
        self.compute();
        self.pump();
    }

    fn register_open_projects(&self, store: &Store) {
        let Some(projects) = self.projects.get() else {
            return;
        };
        let keys = projects.open_projects();
        for key in &keys {
            let name = projects.snapshot(key).map(|s| s.name.to_string());
            let _ = store.project_opened(key, name.as_deref().unwrap_or(""));
        }
        self.state().open = keys;
    }

    /// Starts the task that checks the unverified sessions, unless it runs.
    fn verify(&self) {
        if self.store().is_none() {
            return;
        }
        {
            let mut state = self.state();
            if state.verifying || state.unverified.is_empty() {
                return;
            }
            state.verifying = true;
        }
        let started = self.spawn(true, |engine| {
            engine.verify_now();
            engine.mark_all_dirty();
            engine.compute();
            engine.pump();
            engine.verify();
        });
        if !started {
            self.state().verifying = false;
        }
    }

    /// Moves the unverified sessions whose log file differs from the one
    /// their last read to the end saw (or that never reached it) to the
    /// sessions to read; an unchanged file is not read again. Inactive ones
    /// join the backfill. Blocking (one query, one `stat` per session).
    fn verify_now(&self) {
        let keys: Vec<SessionKey> = std::mem::take(&mut self.state().unverified)
            .into_iter()
            .collect();
        let Some(store) = self.store() else {
            self.state().verifying = false;
            return;
        };
        let saved = store.saved(&keys).unwrap_or_else(|e| {
            warn!("saved positions could not be read"; sqlite = e.code());
            HashMap::new()
        });
        let summaries = self.summaries();
        let now = self.deps.time.now_ms();
        let mut changed = Vec::new();
        let mut unchanged: HashMap<SourceId, &SessionSummary> = HashMap::new();
        for key in &keys {
            let Some(summary) = summaries.get(key) else {
                continue;
            };
            let stamp = FileStamp::of(&summary.file);
            let saved = saved.get(key);
            if stamp.is_some() && saved.and_then(|s| s.stamp) == stamp {
                let newest = unchanged.entry(key.source.clone()).or_insert(summary);
                if (summary.last_activity, &summary.key) > (newest.last_activity, &newest.key) {
                    *newest = summary;
                }
                continue;
            }
            let size = stamp.map_or(0, |s| s.size);
            let read = saved.map_or(0, |s| s.offset);
            changed.push((key.clone(), size, read, is_active(summary, now)));
        }
        let mut state = self.state();
        state.verifying = false;
        let (mut added, mut bytes) = (0usize, 0u64);
        for (key, size, read, active) in changed {
            if !active && !state.backfill.sessions.contains_key(&key) {
                state.backfill.sessions.insert(key.clone(), (size, read));
                added += 1;
                bytes += size.saturating_sub(read);
            }
            state.dirty.insert(key);
        }
        for (source, summary) in unchanged {
            if state.probed.insert(source) {
                state.probe.insert(summary.key.clone());
                state.dirty.insert(summary.key.clone());
            }
        }
        if added > 0 && state.backfill.started.is_none() {
            state.backfill.started = Some(Instant::now());
            drop(state);
            info!("backfill started"; sessions = added, bytes = bytes);
        }
    }

    fn summaries(&self) -> HashMap<SessionKey, SessionSummary> {
        let mut out = HashMap::new();
        for source in self.sources.current() {
            for s in source.sessions().sessions.iter() {
                out.insert(s.key.clone(), s.clone());
            }
        }
        out
    }

    fn writable(&self) -> bool {
        self.store().is_some_and(|s| s.mode() != Mode::Newer)
    }

    /// Starts the next batch, if any may start now.
    fn pump(&self) {
        if !self.writable() {
            return;
        }
        {
            let mut state = self.state();
            if state.stopped || state.reading || state.blocked {
                return;
            }
            if state.dirty.is_empty() {
                // Nothing to read: the archive and the reclaim share the
                // backfill's queue and cadence.
                if !state.maintenance.pending() {
                    return;
                }
                if !state.pace.allowed {
                    drop(state);
                    self.arm_pace();
                    return;
                }
                state.pace.allowed = false;
                state.reading = true;
                drop(state);
                if !self.spawn(true, |engine| engine.maintain()) {
                    self.state().reading = false;
                }
                return;
            }
        }
        let summaries = self.summaries();
        let now = self.deps.time.now_ms();
        let pick = {
            let mut state = self.state();
            if state.reading {
                return;
            }
            state.dirty.retain(|k| summaries.contains_key(k));
            state.probe.retain(|k| summaries.contains_key(k));
            let newest = |active: bool, state: &State| {
                state
                    .dirty
                    .iter()
                    .filter_map(|k| summaries.get(k))
                    .filter(|s| (is_active(s, now) || state.probe.contains(&s.key)) == active)
                    .max_by_key(|s| (s.last_activity, s.key.clone()))
                    .map(|s| s.key.clone())
            };
            let pick = match newest(true, &state) {
                Some(k) => Some(k),
                None if state.pace.allowed => {
                    let k = newest(false, &state);
                    if k.is_some() {
                        state.pace.allowed = false;
                    }
                    k
                }
                None => None,
            };
            if pick.is_some() {
                state.reading = true;
            }
            pick
        };
        match pick {
            Some(key) => {
                if !self.spawn(true, move |engine| engine.read_one(key)) {
                    self.state().reading = false;
                }
            }
            None => self.arm_pace(),
        }
    }

    fn read_one(&self, key: SessionKey) {
        let tag = session_tag(&key.id);
        let _scope = lectern_logging::scope(Ctx {
            session: Some(tag),
            ..Ctx::default()
        });
        let outcome = self.read_batch(&key);
        // The source's adapter changed: its unchanged sessions replay too.
        // Once per source and run: every session still on an old cursor
        // replays when read and reports the change again.
        let mut replays: Vec<SessionKey> = Vec::new();
        if let Read::Done {
            adapter_changed: true,
            ..
        } = &outcome
            && self.state().replayed.insert(key.source.clone())
        {
            info!("session adapter changed; its sessions are read again");
            replays = self
                .summaries()
                .into_keys()
                .filter(|k| k.source == key.source && *k != key)
                .collect();
        }
        let mut state = self.state();
        state.probe.remove(&key);
        if !replays.is_empty() {
            if state.backfill.started.is_none() {
                state.backfill.started = Some(Instant::now());
            }
            for k in replays {
                state.backfill.sessions.entry(k.clone()).or_insert((0, 0));
                state.dirty.insert(k);
            }
        }
        let mut stored = None;
        match outcome {
            Read::Done { out, .. } => {
                let (at_end, to) = (out.at_end, out.to);
                if let Some(entry) = state.backfill.sessions.get_mut(&key) {
                    entry.1 = if at_end { entry.0.max(to) } else { to };
                }
                if at_end {
                    state.dirty.remove(&key);
                    if let Some(entry) = state.backfill.sessions.get_mut(&key) {
                        entry.1 = entry.0;
                    }
                }
                stored = Some(out);
            }
            Read::Gone => {
                state.dirty.remove(&key);
                state.backfill.sessions.remove(&key);
            }
            Read::Later => {
                state.dirty.remove(&key);
            }
            // Still dirty: the next pump reads it from the saved cursor.
            Read::Stale => {}
            Read::Blocked => state.blocked = true,
        }
        self.log_backfill(&mut state);
        drop(state);
        match stored {
            Some(out) => self.apply_facts(&out),
            None => self.mark_all_dirty(),
        }
        // Only now may the next read start: its deltas come after these.
        self.state().reading = false;
        self.compute();
        self.pump();
    }

    /// After a stored batch: the watched windows take its fact deltas and
    /// are replaced at once; the other queries (and windows that cannot
    /// take them) are computed again in the background.
    fn apply_facts(&self, out: &Ingested) {
        let missing: Vec<i64> = {
            let state = self.state();
            let mut ids: Vec<i64> = state
                .interests
                .values()
                .filter_map(|i| i.watched.as_ref())
                .flat_map(|w| w.missing(&out.facts))
                .collect();
            ids.sort_unstable();
            ids.dedup();
            ids
        };
        let texts = match (missing.is_empty(), self.store()) {
            (false, Some(store)) => store.texts(&missing).ok(),
            _ => Some(HashMap::new()),
        };
        let usage_state = self.usage_state();
        let mut changed = Vec::new();
        {
            let mut state = self.state();
            let mut generation = state.generation;
            for (query, interest) in state.interests.iter_mut() {
                let window = matches!(
                    query,
                    UsageQuery::Project { .. } | UsageQuery::AllProjects { .. }
                );
                let usable = window && !interest.dirty && !interest.computing;
                let (Some(watched), true, Some(texts)) =
                    (interest.watched.as_mut(), usable, texts.as_ref())
                else {
                    interest.dirty = true;
                    continue;
                };
                if out.facts_unknown {
                    interest.dirty = true;
                    continue;
                }
                watched.add_texts(texts);
                let applied = watched.apply(out.commit, &out.facts, out.inheritance_unknown);
                let header_moved = interest
                    .snapshot
                    .as_deref()
                    .is_some_and(|s| state_of(s) != Some(&usage_state));
                match applied {
                    Applied::Recompute => interest.dirty = true,
                    Applied::Changed | Applied::Unchanged
                        if applied == Applied::Changed || header_moved =>
                    {
                        generation += 1;
                        let s = watched.snapshot(usage_state.clone());
                        interest.snapshot = Some(Arc::new(with_generation(s, generation)));
                        changed.push((query.clone(), generation));
                    }
                    Applied::Changed | Applied::Unchanged => {}
                }
            }
            state.generation = generation;
        }
        let emitter = self.state().handle.as_ref().map(PluginHandle::emitter);
        if let Some(emitter) = emitter {
            for (query, generation) in changed {
                emitter.emit(UsageEvent::Changed { query, generation });
            }
        }
    }

    /// One step of background maintenance, in a quiet stretch with nothing
    /// to read: the archive plan when due, else one archive step (one
    /// segment), else one reclaim batch.
    fn maintain(&self) {
        let Some(store) = self.store() else {
            self.state().reading = false;
            return;
        };
        let now = self.deps.time.now_ms();
        let zone = self.deps.zone.as_ref();
        let (check, month) = {
            let state = self.state();
            (
                state.maintenance.check,
                state.maintenance.months.front().copied(),
            )
        };
        let mut archived = false;
        if check {
            match store.archive_plan(now, zone) {
                Ok(plan) => {
                    if !plan.is_empty() {
                        info!("archive due"; months = plan.len());
                    }
                    let mut state = self.state();
                    state.maintenance.months = plan.into();
                    state.maintenance.check = false;
                    state.maintenance.reclaim = true;
                }
                Err(e) => {
                    warn!("archive plan failed"; sqlite = e.code());
                    self.state().maintenance.check = false;
                }
            }
            self.arm_archive_check(now);
        } else if let Some(m) = month {
            let done = match store.archive_step(m, now, zone) {
                Ok(Stepped::Segment {
                    table,
                    rows,
                    raw_bytes,
                    packed_bytes,
                    finished,
                }) => {
                    debug!(
                        "cold segment written";
                        month = m,
                        table = table,
                        rows = rows,
                        raw_bytes = raw_bytes,
                        packed_bytes = packed_bytes
                    );
                    archived = true;
                    finished
                }
                Ok(Stepped::Started) => false,
                Ok(Stepped::Finished) => {
                    archived = true;
                    true
                }
                Ok(Stepped::Mismatch) => true,
                Err(e) => {
                    if e.is_corrupt() {
                        self.spawn(true, |engine| engine.repair());
                    } else {
                        warn!("archive step failed"; month = m, sqlite = e.code());
                    }
                    true
                }
            };
            if done {
                if archived {
                    info!("month archived"; month = m);
                }
                self.state().maintenance.months.pop_front();
            }
        } else {
            match store.reclaim(&cold::RECLAIM) {
                Ok(Some(pages)) => debug!("free pages given back"; pages = pages),
                Ok(None) => self.state().maintenance.reclaim = false,
                Err(e) => {
                    warn!("reclaim failed"; sqlite = e.code());
                    self.state().maintenance.reclaim = false;
                }
            }
        }
        {
            let mut state = self.state();
            state.reading = false;
            if archived {
                for (query, interest) in state.interests.iter_mut() {
                    if matches!(query, UsageQuery::History { .. }) {
                        interest.dirty = true;
                    }
                }
            }
        }
        self.compute();
        self.pump();
    }

    /// The archive plan is asked again at the next local midnight.
    fn arm_archive_check(&self, now: i64) {
        let next = window::next_midnight(now, self.deps.zone.as_ref());
        {
            let mut state = self.state();
            if state.maintenance.midnight.is_some_and(|m| m <= next) {
                return;
            }
            state.maintenance.midnight = Some(next);
        }
        let fire = self.timer(move |engine| {
            {
                let mut state = engine.state();
                if state.maintenance.midnight == Some(next) {
                    state.maintenance.midnight = None;
                }
                state.maintenance.check = true;
            }
            engine.arm_pace();
        });
        self.deps.time.at_wall(next, fire);
    }

    /// Ends the backfill once none of its sessions waits.
    fn log_backfill(&self, state: &mut State) {
        let Some(started) = state.backfill.started else {
            return;
        };
        let pending = state
            .backfill
            .sessions
            .keys()
            .any(|k| state.dirty.contains(k));
        if !pending {
            let (done, _) = state.backfill.progress();
            info!(
                "backfill finished";
                sessions = state.backfill.sessions.len(),
                bytes = done,
                took_us = started.elapsed()
            );
            state.backfill = Backfill::default();
        }
    }

    fn read_batch(&self, key: &SessionKey) -> Read {
        let Some(store) = self.store() else {
            return Read::Blocked;
        };
        let Some(source) = self
            .sources
            .current()
            .into_iter()
            .find(|s| s.id() == key.source)
        else {
            return Read::Gone;
        };
        let Some(summary) = source.sessions().get(key).cloned() else {
            return Read::Gone;
        };
        let cursor = match store.cursor(key) {
            Ok(c) => c,
            Err(e) => return self.store_failed(&store, &e),
        };
        // Before the read: a later append makes the stamp differ.
        let stamp = FileStamp::of(&summary.file);
        let batch = match source.read(key, &cursor, READ_BYTES) {
            Ok(b) => b,
            Err(SessionError::NotFound) => return Read::Gone,
            Err(SessionError::Unrecognized) => return Read::Later,
            Err(SessionError::Io(_) | SessionError::TooLarge(_)) => {
                warn!("session read failed"; path = Value::path(&summary.file), offset = cursor.offset());
                return Read::Later;
            }
        };
        drop(source);
        let start = self.place(&summary, &summary.cwd);
        let places = self.places(&summary, &batch);
        let input = Ingest {
            session: SessionInfo {
                key,
                agent: &summary.agent,
                parent: summary.parent.as_ref().map(|p| &p.session),
                inheritance_unknown: summary.inheritance_unknown,
                start: &start,
            },
            from: &cursor,
            batch: &batch,
            places: &places,
            stamp,
        };
        let adapter_changed = batch.reset
            && cursor
                .adapter_version()
                .is_some_and(|v| batch.cursor.adapter_version() != Some(v));
        match store.ingest(&input) {
            // A salvage reset the cursor while this batch was read: read
            // again from the saved one.
            Ok(out) if out.stale => Read::Stale,
            Ok(out) => Read::Done {
                adapter_changed,
                out: Box::new(out),
            },
            Err(e) => self.store_failed(&store, &e),
        }
    }

    /// A failed store operation: corruption is repaired in the background,
    /// any other failure makes the database read only until a later write
    /// succeeds.
    fn store_failed(&self, store: &Store, e: &StoreError) -> Read {
        if e.is_corrupt() {
            error!("database corrupt"; sqlite = e.code());
            self.spawn(true, |engine| engine.repair());
            return Read::Blocked;
        }
        if matches!(e, StoreError::ReadOnly) {
            return Read::Blocked;
        }
        error!("database write failed"; sqlite = e.code(), path = Value::path(store.dir()));
        store.set_unwritable(e.reason());
        Read::Blocked
    }

    fn repair(&self) {
        let Some(store) = self.store() else {
            return;
        };
        let now = self.deps.time.now_ms();
        match store.repair(now) {
            Ok(Checked::Recovered(r)) => self.recovered(r.at),
            Ok(_) => {}
            // A newer version's file: left alone, read only.
            Err(StoreError::ReadOnly) => {}
            Err(e) => {
                error!("database repair failed"; sqlite = e.code());
                store.set_unwritable(e.reason());
            }
        }
        self.state().blocked = false;
        self.mark_all_dirty();
        self.compute();
        self.pump();
    }

    /// After a salvage every session is read again (their cursors may be
    /// back at the start).
    fn recovered(&self, at: i64) {
        let keys: Vec<SessionKey> = self.summaries().into_keys().collect();
        let mut state = self.state();
        state.recovered = Some(at);
        state.dirty.extend(keys);
    }

    /// The place of a cwd: from the session's own placement when it is the
    /// starting cwd, else `Projects::resolve` (remembered per path).
    fn place(&self, summary: &SessionSummary, cwd: &Path) -> Place {
        if cwd == &*summary.cwd {
            match &summary.place {
                SessionPlace::Located(l) => return place_of_key(&l.project, &l.rel),
                SessionPlace::Resolved(r) => return place_of_key(&r.project, &r.rel),
                SessionPlace::Pending | SessionPlace::Missing | SessionPlace::Unresolvable(_) => {}
            }
        }
        if cwd.as_os_str().is_empty() {
            return Place::Unknown;
        }
        if let Some(p) = self.state().places.get(cwd) {
            return p.clone();
        }
        let place = match self.projects.get().map(|p| p.resolve(cwd)) {
            Some(Ok(r)) => place_of_key(&r.project, &r.rel),
            _ => Place::Dir(cwd.to_path_buf()),
        };
        let mut state = self.state();
        if state.places.len() >= PLACE_CACHE {
            state.places.clear();
        }
        state.places.insert(cwd.to_path_buf(), place.clone());
        place
    }

    fn places(&self, summary: &SessionSummary, batch: &Batch) -> HashMap<PathBuf, Place> {
        let mut out = HashMap::new();
        for rec in &batch.records {
            if let Record::Context { cwd, .. } = &rec.value
                && !out.contains_key(cwd.as_ref())
            {
                out.insert(cwd.to_path_buf(), self.place(summary, cwd));
            }
        }
        out
    }

    // ---- pacing ----

    pub(crate) fn on_quiescent(&self, seq: u64) {
        let check = {
            let mut state = self.state();
            if seq <= state.pace.max_seq {
                return;
            }
            state.pace.max_seq = seq;
            state.pace.quiet = Some(seq);
            let check = state.needs_check && !state.checking;
            if check {
                state.checking = true;
            }
            check
        };
        if check {
            self.spawn(true, |engine| engine.check());
        }
        self.arm_pace();
        let mut state = self.state();
        if state.pace.quiet == Some(seq) {
            state.pace.handled = Some(seq);
        }
    }

    pub(crate) fn on_busy(&self, seq: u64) {
        let mut state = self.state();
        if seq <= state.pace.max_seq {
            return;
        }
        state.pace.max_seq = seq;
        state.pace.quiet = None;
        state.pace.handled = None;
    }

    /// Arms the pacing timer when the kernel is quiescent and sessions wait
    /// to be backfilled.
    fn arm_pace(&self) {
        let quiet = {
            let mut state = self.state();
            let Some(quiet) = state.pace.quiet else {
                return;
            };
            if state.stopped
                || state.pace.allowed
                || state.pace.timer == Some(quiet)
                || (state.dirty.is_empty() && !state.maintenance.pending())
            {
                return;
            }
            state.pace.timer = Some(quiet);
            quiet
        };
        let fire = self.timer(move |engine| engine.pace_fired(quiet));
        self.deps.time.after(PACE, fire);
    }

    fn pace_fired(&self, quiet: u64) {
        {
            let mut state = self.state();
            if state.pace.timer == Some(quiet) {
                state.pace.timer = None;
            }
            if state.pace.quiet != Some(quiet) {
                return;
            }
            state.pace.allowed = true;
        }
        self.pump();
        // A grant the pump could not use now (another batch was running) is
        // dropped: it must not start a batch later, after busy moments; the
        // next quiet stretch grants again.
        self.state().pace.allowed = false;
    }

    fn check(&self) {
        let Some(store) = self.store() else {
            return;
        };
        let now = self.deps.time.now_ms();
        match store.check(now) {
            Ok(Checked::Recovered(r)) => self.recovered(r.at),
            Ok(_) => {}
            Err(e) => error!("database check failed"; sqlite = e.code()),
        }
        let mut state = self.state();
        state.needs_check = false;
        state.checking = false;
        drop(state);
        self.mark_all_dirty();
        self.compute();
        self.pump();
    }

    // ---- events ----

    pub(crate) fn on_sessions(&self, event: &SessionsChanged) {
        {
            let mut state = self.state();
            state.blocked = false;
            state.unverified.extend(event.changed.iter().cloned());
            for key in event.removed.iter() {
                state.dirty.remove(key);
                state.unverified.remove(key);
                state.probe.remove(key);
            }
        }
        self.verify();
        self.pump();
    }

    pub(crate) fn on_project(&self, event: &ProjectEvent) {
        match event {
            ProjectEvent::Opened {
                result: Ok(key), ..
            } => {
                let key = key.clone();
                self.spawn(true, move |engine| engine.project_opened(key));
            }
            ProjectEvent::Closed { project } => {
                self.state().open.retain(|k| k != project);
                self.mark_all_dirty();
                self.compute();
            }
            _ => {}
        }
    }

    fn project_opened(&self, key: ProjectKey) {
        if let Some(store) = self.store() {
            let name = self
                .projects
                .get()
                .and_then(|p| p.snapshot(&key))
                .map(|s| s.name.to_string())
                .unwrap_or_default();
            match store.project_opened(&key, &name) {
                Ok(row) => {
                    info!("project opened"; project = Value::uuid(row.pid.0), folder = row.folder)
                }
                Err(e) => warn!("project could not be recorded"; sqlite = e.code()),
            }
        }
        {
            let mut state = self.state();
            if !state.open.contains(&key) {
                state.open.push(key);
            }
        }
        self.mark_all_dirty();
        self.compute();
    }

    // ---- snapshots ----

    fn mark_all_dirty(&self) {
        for interest in self.state().interests.values_mut() {
            interest.dirty = true;
        }
    }

    /// Starts the snapshot task unless one runs; it recomputes every dirty
    /// query.
    fn compute(&self) {
        if self.store().is_none() && !matches!(&*lock(&self.db), Db::Failed(_)) {
            return;
        }
        {
            let mut state = self.state();
            if state.computing || !state.interests.values().any(|i| i.dirty) {
                return;
            }
            state.computing = true;
        }
        if !self.spawn(true, |engine| engine.compute_all()) {
            self.state().computing = false;
        }
    }

    fn compute_all(&self) {
        loop {
            let query = {
                let mut state = self.state();
                let next = state
                    .interests
                    .iter_mut()
                    .find(|(_, i)| i.dirty)
                    .map(|(q, i)| {
                        i.dirty = false;
                        i.computing = true;
                        q.clone()
                    });
                match next {
                    Some(q) => q,
                    None => {
                        state.computing = false;
                        return;
                    }
                }
            };
            let computed = self.compute_one(&query);
            let usage_state = self.usage_state();
            let generation = {
                let mut state = self.state();
                state.generation += 1;
                let generation = state.generation;
                let Some(interest) = state.interests.get_mut(&query) else {
                    continue;
                };
                interest.computing = false;
                let snapshot = match computed {
                    Some(Computed::Watched(w)) => {
                        let s = w.snapshot(usage_state);
                        interest.watched = Some(w);
                        Some(s)
                    }
                    Some(Computed::Snapshot(s)) => {
                        interest.watched = None;
                        Some(s)
                    }
                    None => {
                        interest.watched = None;
                        None
                    }
                };
                interest.snapshot = snapshot.map(|s| Arc::new(with_generation(s, generation)));
                generation
            };
            let emitter = self.state().handle.as_ref().map(PluginHandle::emitter);
            if let Some(emitter) = emitter {
                emitter.emit(UsageEvent::Changed { query, generation });
            }
        }
    }

    fn usage_state(&self) -> UsageState {
        let state = {
            let db = lock(&self.db);
            match &*db {
                Db::Closed => UsageState::Loading,
                Db::Failed(reason) => {
                    UsageState::Degraded(UsageDegraded::Unwritable(reason.clone()))
                }
                Db::Open(store) => match store.mode() {
                    Mode::Newer => UsageState::Degraded(UsageDegraded::NewerDatabase),
                    Mode::Unwritable(reason) => {
                        UsageState::Degraded(UsageDegraded::Unwritable(reason))
                    }
                    Mode::Writable => UsageState::Ready,
                },
            }
        };
        match state {
            UsageState::Ready => {
                let s = self.state();
                if let Some(at) = s.recovered {
                    UsageState::Degraded(UsageDegraded::Recovered {
                        at: lectern_services::session::Timestamp(at),
                    })
                } else if s.backfill.sessions.keys().any(|k| s.dirty.contains(k)) {
                    let (done, total) = s.backfill.progress();
                    UsageState::Backfilling { done, total }
                } else {
                    UsageState::Ready
                }
            }
            other => other,
        }
    }

    fn compute_one(&self, query: &UsageQuery) -> Option<Computed> {
        let now = self.deps.time.now_ms();
        let open = self.state().open.clone();
        if matches!(
            query,
            UsageQuery::Project { .. } | UsageQuery::AllProjects { .. }
        ) {
            self.arm_midnight(now);
        }
        let projects = self.projects.get();
        // `compute` runs while closed only after opening failed.
        let Some(store) = self.store() else {
            return unavailable(
                query,
                self.usage_state(),
                now,
                self.deps.zone.as_ref(),
                &open,
                projects.as_deref(),
            )
            .map(Computed::Snapshot);
        };
        let context = Context {
            store: &store,
            projects: projects.as_deref(),
            zone: self.deps.zone.as_ref(),
            now_ms: now,
            open: &open,
            state: self.usage_state(),
        };
        match compute_full(&context, query) {
            Ok(s) => s,
            Err(e) => {
                self.query_failed(&e);
                None
            }
        }
    }

    fn query_failed(&self, e: &StoreError) {
        if e.is_corrupt() {
            error!("database corrupt"; sqlite = e.code());
            self.spawn(true, |engine| engine.repair());
        } else {
            warn!("snapshot query failed"; sqlite = e.code());
        }
    }

    /// Window snapshots move to the next day at local midnight.
    fn arm_midnight(&self, now: i64) {
        let next = window::next_midnight(now, self.deps.zone.as_ref());
        {
            let mut state = self.state();
            if state.midnight.is_some_and(|m| m <= next) {
                return;
            }
            state.midnight = Some(next);
        }
        let fire = self.timer(move |engine| {
            {
                let mut state = engine.state();
                if state.midnight == Some(next) {
                    state.midnight = None;
                }
                for (query, interest) in state.interests.iter_mut() {
                    if matches!(
                        query,
                        UsageQuery::Project { .. } | UsageQuery::AllProjects { .. }
                    ) {
                        interest.dirty = true;
                    }
                }
            }
            debug!("local midnight: window snapshots recomputed"; midnight_ms = next);
            engine.compute();
        });
        self.deps.time.at_wall(next, fire);
    }

    fn release(&self, query: &UsageQuery) {
        let mut state = self.state();
        if let Some(interest) = state.interests.get_mut(query) {
            interest.count = interest.count.saturating_sub(1);
            if interest.count == 0 {
                state.interests.remove(query);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn dirty_sessions(&self) -> usize {
        self.state().dirty.len()
    }

    /// The `Quiescent` handled last (its pacing timer armed), if no `Busy`
    /// came after it.
    #[cfg(test)]
    pub(crate) fn handled_quiet(&self) -> Option<u64> {
        self.state().pace.handled
    }
}

fn range_of(days: &[(LocalDate, Range<i64>)]) -> Range<i64> {
    match (days.first(), days.last()) {
        (Some(first), Some(last)) => first.1.start..last.1.end,
        _ => 0..0,
    }
}

/// What computing a snapshot needs; the engine and the tests build it.
pub(crate) struct Context<'a> {
    pub(crate) store: &'a Store,
    pub(crate) projects: Option<&'a dyn Projects>,
    pub(crate) zone: &'a dyn LocalZone,
    pub(crate) now_ms: i64,
    /// Projects open in this run.
    pub(crate) open: &'a [ProjectKey],
    pub(crate) state: UsageState,
}

impl Context<'_> {
    fn header(&self, may_include_inherited: bool) -> SnapshotHeader {
        SnapshotHeader {
            generation: 0,
            state: self.state.clone(),
            may_include_inherited,
        }
    }
}

/// The snapshot of a query; None for a project the database does not know
/// and for the queries of later steps.
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn compute(
    cx: &Context<'_>,
    query: &UsageQuery,
) -> Result<Option<UsageSnapshot>, StoreError> {
    Ok(compute_full(cx, query)?.map(|c| match c {
        Computed::Watched(w) => w.snapshot(cx.state.clone()),
        Computed::Snapshot(s) => s,
    }))
}

/// A query computed from the database: a window's accumulators (kept to
/// apply later commits' deltas), or a snapshot computed whole every time.
pub(crate) enum Computed {
    Watched(Watched),
    Snapshot(UsageSnapshot),
}

pub(crate) fn compute_full(
    cx: &Context<'_>,
    query: &UsageQuery,
) -> Result<Option<Computed>, StoreError> {
    match query {
        UsageQuery::Project { project, window } => {
            let days = window::days(*window, cx.now_ms, cx.zone);
            let Some(row) = project_row(cx, project)? else {
                return Ok(None);
            };
            // A project never opened has its one location; an opened one
            // claims directories, and a newer location may be one of them.
            let (locations, max_location) = if row.opened {
                let owners = Owners::new(&cx.store.projects()?, &cx.store.dir_locations()?);
                (owners.locations_of(row.id), owners.max_location)
            } else {
                (vec![row.location], i64::MAX)
            };
            let totals = cx.store.sums_at(&locations, range_of(&days))?;
            Ok(Some(Computed::Watched(Watched::project(
                snapshot::info(&row, cx.open),
                *window,
                &days,
                locations,
                max_location,
                &totals,
            ))))
        }
        UsageQuery::AllProjects { window } => {
            let days = window::days(*window, cx.now_ms, cx.zone);
            let owners = Owners::new(&cx.store.projects()?, &cx.store.dir_locations()?);
            let totals = cx.store.sums_all(range_of(&days))?;
            Ok(Some(Computed::Watched(Watched::all(
                *window, &days, owners, cx.open, &totals,
            ))))
        }
        UsageQuery::History {
            project,
            from,
            to,
            days,
        } => {
            let Some(row) = project_row(cx, project)? else {
                return Ok(None);
            };
            let locations = if row.opened {
                Owners::new(&cx.store.projects()?, &cx.store.dir_locations()?).locations_of(row.id)
            } else {
                vec![row.location]
            };
            let totals = cx.store.history(&locations, *from, *to, *days, cx.zone)?;
            let range = window::start(*from, cx.zone)
                ..window::start(window::date_of(window::day_number(*to) + 1), cx.zone);
            let inherited = cx.store.inherited_in(&locations, range)?;
            Ok(Some(Computed::Snapshot(UsageSnapshot::History(
                snapshot::history(
                    cx.header(inherited),
                    snapshot::info(&row, cx.open),
                    *from,
                    &totals,
                ),
            ))))
        }
        // Later steps of docs/design/usage.md「验收」.
        UsageQuery::Worktree { .. } | UsageQuery::Session { .. } | UsageQuery::Mr { .. } => {
            Ok(None)
        }
    }
}

/// The snapshot of a window query while the database could not be opened:
/// no usage, and the header says why (`Degraded(Unwritable)`). The project
/// of an open reference has no ID yet (all zero).
fn unavailable(
    query: &UsageQuery,
    state: UsageState,
    now_ms: i64,
    zone: &dyn LocalZone,
    open: &[ProjectKey],
    projects: Option<&dyn Projects>,
) -> Option<UsageSnapshot> {
    let header = SnapshotHeader {
        generation: 0,
        state,
        may_include_inherited: false,
    };
    let totals = Totals::default();
    match query {
        UsageQuery::Project { project, window } => {
            let days = window::days(*window, now_ms, zone);
            let info = match project {
                ProjectRef::Open(key) => ProjectInfo {
                    id: UsageProjectId([0; 16]),
                    name: projects
                        .and_then(|p| p.snapshot(key))
                        .map(|s| s.name.to_string())
                        .unwrap_or_default()
                        .into(),
                    folder: matches!(key, ProjectKey::Folder { .. }),
                    key: open.contains(key).then(|| key.clone()),
                    status: ProjectStatus::Active,
                },
                ProjectRef::Id(pid) => ProjectInfo {
                    id: *pid,
                    name: "".into(),
                    folder: false,
                    key: None,
                    status: ProjectStatus::Active,
                },
            };
            let p = snapshot::project(header, info, *window, &days, &totals);
            Some(UsageSnapshot::Project(p))
        }
        UsageQuery::AllProjects { window } => {
            let days = window::days(*window, now_ms, zone);
            let owners = Owners::new(&[], &[]);
            let a = snapshot::all(header, *window, &days, &totals, &owners, open);
            Some(UsageSnapshot::All(a))
        }
        UsageQuery::Worktree { .. }
        | UsageQuery::Session { .. }
        | UsageQuery::Mr { .. }
        | UsageQuery::History { .. } => None,
    }
}

/// The project row of a reference; an open project the database does not
/// know yet is recorded first.
fn project_row(cx: &Context<'_>, project: &ProjectRef) -> Result<Option<ProjectRow>, StoreError> {
    match project {
        ProjectRef::Id(pid) => cx.store.project_by_id(pid),
        ProjectRef::Open(key) => {
            let open = cx.open.contains(key);
            match cx.store.project_by_key(key)? {
                Some(row) if row.opened || !open => Ok(Some(row)),
                _ if open => {
                    let name = cx
                        .projects
                        .and_then(|p| p.snapshot(key))
                        .map(|s| s.name.to_string())
                        .unwrap_or_default();
                    cx.store.project_opened(key, &name).map(Some)
                }
                other => Ok(other),
            }
        }
    }
}

fn state_of(snapshot: &UsageSnapshot) -> Option<&UsageState> {
    match snapshot {
        UsageSnapshot::Project(p) => Some(&p.header.state),
        UsageSnapshot::All(a) => Some(&a.header.state),
        _ => None,
    }
}

fn with_generation(snapshot: UsageSnapshot, generation: u64) -> UsageSnapshot {
    match snapshot {
        UsageSnapshot::Project(mut p) => {
            p.header.generation = generation;
            UsageSnapshot::Project(p)
        }
        UsageSnapshot::All(mut a) => {
            a.header.generation = generation;
            UsageSnapshot::All(a)
        }
        UsageSnapshot::History(mut h) => {
            h.header.generation = generation;
            UsageSnapshot::History(h)
        }
        other => other,
    }
}

fn is_active(s: &SessionSummary, now: i64) -> bool {
    s.open_turn || now.saturating_sub(s.last_activity.0) < ACTIVE_MS
}

enum Read {
    Done {
        /// The batch replays from the start because the cursor came from
        /// another adapter version.
        adapter_changed: bool,
        out: Box<Ingested>,
    },
    /// The session or its source no longer exists.
    Gone,
    /// Not readable now; read again after the next `SessionsChanged`.
    Later,
    /// Discarded: the saved cursor moved meanwhile (a salvage).
    Stale,
    /// The database refused the write.
    Blocked,
}

impl Usage for Engine {
    fn watch(&self, query: UsageQuery) -> UsageInterest {
        {
            let mut state = self.state();
            let interest = state.interests.entry(query.clone()).or_insert(Interest {
                count: 0,
                snapshot: None,
                dirty: true,
                computing: false,
                watched: None,
            });
            interest.count += 1;
        }
        self.compute();
        let me = self.me.clone();
        UsageInterest::new(move || {
            if let Some(engine) = me.upgrade() {
                engine.release(&query);
            }
        })
    }

    fn snapshot(&self, query: &UsageQuery) -> Option<Arc<UsageSnapshot>> {
        self.state()
            .interests
            .get(query)
            .and_then(|i| i.snapshot.clone())
    }

    fn limits(&self) -> Arc<Limits> {
        self.limits.clone()
    }

    fn command(&self, command: UsageCommand) {
        // Merging and confirming projects come with step 3 of the design.
        let _ = command;
        debug!("usage command ignored until project identity is implemented");
    }

    fn begin_analysis(&self, start: AnalysisStart) -> Result<AnalysisId, UsageError> {
        // Metering comes with step 8 of the design.
        let _ = start;
        Err(UsageError::Unavailable)
    }

    fn meter(&self, event: MeterEvent) -> Result<(), UsageError> {
        let _ = event;
        Err(UsageError::Unavailable)
    }

    fn register_mr_source(
        &self,
        provider: Arc<dyn MrSource>,
        token: ActivationToken,
    ) -> MrSourceRegistration {
        // MR intervals come with step 7: nothing calls the provider yet.
        drop((provider, token));
        MrSourceRegistration::new(|| {})
    }
}
