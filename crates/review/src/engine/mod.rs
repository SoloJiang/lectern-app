//! The review engine (docs/design/review.md「数据流与线程」): open reviews,
//! their immutable snapshots, the commands, `Prepare` with its rows and
//! texts, and the annotation log of each repository. Step 4 wraps it in the
//! "review" plugin; the tests and benches drive it through
//! `testing::Driven`.
//!
//! - `open`, `snapshot`, `command`, `rows`, `text` and `close` return at
//!   once and do no IO on the calling thread: everything that reads git,
//!   the cache, session logs or the annotation log runs on executor tasks.
//!   A seen command replaces the snapshot's seen array in memory before it
//!   returns; the log is appended on a `Background` task.
//! - One computation round per review at a time (`round`): the user waits
//!   for its first part (`Interactive`: the change set, the target's turns,
//!   cached claims and cutoffs, the first file's attribution); what it left
//!   pending (claims not cached, cutoffs not computed in time) and the
//!   other files' attribution follow on `Background`, each batch published
//!   as a new snapshot. Requests during a round wait for its end.
//! - Locks: `state`, then a repository log's `mem`; the other mutexes are
//!   leaves. No lock is held across IO, and events are emitted after the
//!   state lock is released.

pub(crate) mod attribute;
mod narrative;
pub(crate) mod round;
mod rows;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use lectern_cache::Cache;
use lectern_executor::{Executor, Priority};
use lectern_git::{Discovered, GitError};
use lectern_services::changes::{ChangePath, RefreshHint};
use lectern_services::project::{ProjectKey, Projects, WorktreeKey};
use lectern_services::review::{
    AnalysisState, Brief, Degraded, FileEntry, FileKey, FileRows, LoadState, Need, Progress,
    ReviewCommand, ReviewError, ReviewEvent, ReviewId, ReviewSnapshot, ReviewTarget, RowsKey,
    SeenState, TextKey, TurnEntry,
};
use lectern_services::session::{
    Content, SessionKey, SessionSource, SourceId, Timestamp, TurnIndex,
};

use crate::claims::Claims;
use crate::cutoff::{self, Record, Table};
use crate::log::{BaseRecord, Index, LogFile, Record as LogRecord, SessionId};
use crate::lru::Versions;
use crate::seen;

use self::attribute::FileResult;
use self::narrative::Own;
use self::round::{Git, Kind, StepIds, Work};

/// Finds a repository from a worktree root (`git::Repo::discover`; the
/// tests read only the repository's own configuration).
pub(crate) type Discover =
    Arc<dyn Fn(&Path, &[PathBuf]) -> Result<Option<Discovered>, GitError> + Send + Sync>;

/// Publish the background attribution after this many files…
const BATCH_FILES: usize = 8;
/// …or this long, whichever comes first.
const BATCH_TIME: Duration = Duration::from_millis(50);
/// Prepared rows kept per review, and prepared texts.
const MAX_ROWS_BYTES: usize = 8 << 20;
const MAX_TEXT_BYTES: usize = 2 << 20;

pub(crate) struct Config {
    pub executor: Executor,
    pub cache: Cache,
    /// The annotation logs' directory (ADR 0044).
    pub data_dir: PathBuf,
    pub git_ceilings: Arc<[PathBuf]>,
    pub projects: Arc<dyn Projects>,
    pub sources: Arc<[Arc<dyn SessionSource>]>,
    /// Called on an executor thread (or the calling thread of a seen
    /// command) after a snapshot was replaced, with no lock held.
    pub emit: Arc<dyn Fn(ReviewEvent) + Send + Sync>,
    pub clock: Arc<dyn Fn() -> Timestamp + Send + Sync>,
    pub discover: Discover,
    /// How long an interactive round computes cutoff misses
    /// (`cutoff::INTERACTIVE_BUDGET`; the tests set 0).
    pub interactive_budget: Duration,
}

pub(crate) struct Engine {
    pub(crate) cfg: Config,
    me: Weak<Engine>,
    state: Mutex<State>,
    gits: Mutex<HashMap<WorktreeKey, Arc<Git>>>,
    tables: Mutex<HashMap<WorktreeKey, Table>>,
    claims: Mutex<HashMap<SessionKey, Arc<Claims>>>,
    adapters: Mutex<HashMap<SourceId, u32>>,
    versions: Mutex<Versions>,
}

#[derive(Default)]
struct State {
    next: u64,
    reviews: HashMap<ReviewId, Open>,
    targets: HashMap<ReviewTarget, ReviewId>,
    logs: HashMap<PathBuf, Arc<RepoLog>>,
}

/// One repository's annotation log.
struct RepoLog {
    file: Mutex<LogFile>,
    mem: Mutex<LogMem>,
}

struct LogMem {
    index: Index,
    loaded: bool,
    loading: bool,
    flushing: bool,
}

struct Open {
    target: ReviewTarget,
    refs: u32,
    snapshot: Arc<ReviewSnapshot>,
    work: Option<Arc<Work>>,
    round: u64,
    running: bool,
    again: Option<Kind>,
    cancel: Arc<AtomicBool>,
    step_ids: Arc<Mutex<StepIds>>,
    needs: Arc<[Need]>,
    prepare: u64,
    rows: HashMap<RowsKey, Arc<FileRows>>,
    texts: HashMap<TextKey, Arc<str>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn empty_brief() -> Arc<Brief> {
    Arc::new(Brief {
        items: Arc::from(Vec::new()),
        omitted: 0,
        pending_providers: 0,
        // Analyze comes with #66.
        analysis: AnalysisState::Unavailable(Arc::from("unavailable")),
    })
}

fn loading(id: ReviewId, target: ReviewTarget) -> ReviewSnapshot {
    ReviewSnapshot {
        id,
        target,
        generation: 0,
        state: LoadState::Loading,
        base: None,
        sessions: Arc::from(Vec::new()),
        files: Arc::from(Vec::new()),
        unclaimed: 0,
        seen: Arc::from(Vec::new()),
        progress: Progress::default(),
        turns: None,
        groups: Arc::from(Vec::new()),
        brief: empty_brief(),
        focus_review: Arc::from(Vec::new()),
        focus_touched: Arc::from(Vec::new()),
        totals: Default::default(),
        degraded: None,
        notes: Arc::from(Vec::new()),
    }
}

impl Engine {
    /// No IO, no task: the tables are empty until the first `open`.
    pub(crate) fn new(cfg: Config) -> Arc<Engine> {
        Arc::new_cyclic(|me| Engine {
            cfg,
            me: me.clone(),
            state: Mutex::new(State::default()),
            gits: Mutex::new(HashMap::new()),
            tables: Mutex::new(HashMap::new()),
            claims: Mutex::new(HashMap::new()),
            adapters: Mutex::new(HashMap::new()),
            versions: Mutex::new(Versions::default()),
        })
    }

    fn submit(&self, priority: Priority, job: impl FnOnce(&Arc<Engine>) + Send + 'static) {
        let me = self.me.clone();
        self.cfg
            .executor
            .run(priority, move || {
                if let Some(engine) = me.upgrade() {
                    job(&engine);
                }
            })
            .detach();
    }

    fn emit(&self, events: Vec<ReviewEvent>) {
        for e in events {
            (self.cfg.emit)(e);
        }
    }

    // ---- the service's calls -------------------------------------------

    pub(crate) fn open(&self, target: ReviewTarget) -> ReviewId {
        let mut state = lock(&self.state);
        if let Some(&id) = state.targets.get(&target) {
            if let Some(open) = state.reviews.get_mut(&id) {
                open.refs += 1;
            }
            return id;
        }
        state.next += 1;
        let id = ReviewId(state.next);
        state.targets.insert(target.clone(), id);
        state.reviews.insert(
            id,
            Open {
                target: target.clone(),
                refs: 1,
                snapshot: Arc::new(loading(id, target)),
                work: None,
                round: 0,
                running: false,
                again: None,
                cancel: Arc::new(AtomicBool::new(false)),
                step_ids: Arc::new(Mutex::new(StepIds::default())),
                needs: Arc::from(Vec::new()),
                prepare: 0,
                rows: HashMap::new(),
                texts: HashMap::new(),
            },
        );
        self.start_round(&mut state, id, Kind::Fetch, Priority::Interactive);
        id
    }

    pub(crate) fn snapshot(&self, id: ReviewId) -> Option<Arc<ReviewSnapshot>> {
        lock(&self.state)
            .reviews
            .get(&id)
            .map(|o| Arc::clone(&o.snapshot))
    }

    pub(crate) fn rows(&self, id: ReviewId, key: &RowsKey) -> Option<Arc<FileRows>> {
        lock(&self.state).reviews.get(&id)?.rows.get(key).cloned()
    }

    pub(crate) fn text(&self, id: ReviewId, key: &TextKey) -> Option<Arc<str>> {
        lock(&self.state).reviews.get(&id)?.texts.get(key).cloned()
    }

    /// Drops the review when its last reference goes (the 30-second grace
    /// comes with the plugin, step 4), and the worktree state no open
    /// review uses.
    pub(crate) fn close(&self, id: ReviewId) {
        let mut state = lock(&self.state);
        let Some(open) = state.reviews.get_mut(&id) else {
            return;
        };
        open.refs = open.refs.saturating_sub(1);
        if open.refs > 0 {
            return;
        }
        open.cancel.store(true, Ordering::Release);
        let target = open.target.clone();
        state.reviews.remove(&id);
        state.targets.remove(&target);
        let used: HashSet<WorktreeKey> = state
            .reviews
            .values()
            .filter_map(|o| o.work.as_ref().map(|w| w.git.key.clone()))
            .collect();
        let empty = state.reviews.is_empty();
        drop(state);
        lock(&self.gits).retain(|k, _| used.contains(k));
        lock(&self.tables).retain(|k, _| used.contains(k));
        if empty {
            lock(&self.versions).clear();
        }
    }

    pub(crate) fn command(&self, id: ReviewId, command: ReviewCommand) {
        let mut events = Vec::new();
        {
            let mut state = lock(&self.state);
            if !state.reviews.contains_key(&id) {
                return;
            }
            match command {
                ReviewCommand::MarkSeen(_)
                | ReviewCommand::MarkUnseen(_)
                | ReviewCommand::LooksGood => {
                    self.seen_command(&mut state, id, command, &mut events);
                }
                ReviewCommand::Prepare(needs) => self.prepare(&mut state, id, needs),
                ReviewCommand::Refresh => self.start_round(
                    &mut state,
                    id,
                    Kind::Refresh(RefreshHint::Full),
                    Priority::Interactive,
                ),
                // Until #66.
                ReviewCommand::Analyze | ReviewCommand::CancelAnalyze => {}
            }
        }
        self.emit(events);
    }

    // ---- rounds ----------------------------------------------------------

    fn start_round(&self, state: &mut State, id: ReviewId, kind: Kind, priority: Priority) {
        let Some(open) = state.reviews.get_mut(&id) else {
            return;
        };
        if open.running {
            // After the current round; a full refresh wins over the rest.
            open.again = Some(match (open.again.take(), kind) {
                (Some(Kind::Refresh(RefreshHint::Full)), _)
                | (_, Kind::Refresh(RefreshHint::Full)) => Kind::Refresh(RefreshHint::Full),
                (Some(Kind::Fetch), _) | (_, Kind::Fetch) => Kind::Fetch,
                (_, kind) => kind,
            });
            return;
        }
        open.running = true;
        open.round += 1;
        open.cancel.store(true, Ordering::Release);
        open.cancel = Arc::new(AtomicBool::new(false));
        let seq = open.round;
        let cancel = Arc::clone(&open.cancel);
        let prev = open.work.clone();
        let step_ids = Arc::clone(&open.step_ids);
        let target = open.target.clone();
        let interactive = priority == Priority::Interactive;
        self.submit(priority, move |engine| {
            engine.run_round(id, seq, target, kind, prev, step_ids, cancel, interactive);
        });
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "what `start_round` took under the lock, moved into the task"
    )]
    fn run_round(
        &self,
        id: ReviewId,
        seq: u64,
        target: ReviewTarget,
        kind: Kind,
        prev: Option<Arc<Work>>,
        step_ids: Arc<Mutex<StepIds>>,
        cancel: Arc<AtomicBool>,
        interactive: bool,
    ) {
        if let ReviewTarget::Session(key) = &target
            && let Some(summary) = round::folder_session(self, key)
        {
            self.folder_round(id, seq, summary);
            return;
        }
        let budget = interactive.then_some(self.cfg.interactive_budget);
        let prepared = round::prepare(
            self,
            &target,
            &kind,
            prev.as_ref(),
            &step_ids,
            budget,
            &cancel,
        );
        let work = match prepared {
            Ok(work) => Arc::new(work),
            Err(error) => {
                let mut events = Vec::new();
                {
                    let mut state = lock(&self.state);
                    if let Some(open) = current(&mut state, id, seq) {
                        let snapshot = ReviewSnapshot {
                            generation: open.snapshot.generation + 1,
                            state: LoadState::Failed(error),
                            ..loading(id, target)
                        };
                        events.push(ReviewEvent::Changed {
                            id,
                            generation: snapshot.generation,
                        });
                        open.snapshot = Arc::new(snapshot);
                        open.running = false;
                        if let Some(kind) = open.again.take() {
                            self.start_round(&mut state, id, kind, Priority::Background);
                        }
                    }
                }
                self.emit(events);
                return;
            }
        };
        // The first file in the narrative's order is attributed before the
        // first snapshot.
        if matches!(target, ReviewTarget::Session(_)) && !work.files.is_empty() {
            let result = attribute::file(self, &work, 0, &cancel);
            lock(&work.results)[0] = Some(Arc::new(result));
        }
        let mut events = Vec::new();
        {
            let mut state = lock(&self.state);
            let log = self.log_for(&mut state, work.git.common_dir());
            let Some(open) = current(&mut state, id, seq) else {
                return;
            };
            open.work = Some(Arc::clone(&work));
            let snapshot = self.assemble(open, &work, Some(&log));
            events.push(changed(open, snapshot));
        }
        self.emit(events);
        self.submit(Priority::Background, move |engine| {
            engine.settle(id, seq, work, cancel)
        });
    }

    /// The background part of a round: what `prepare` left pending, then
    /// the other files' attribution in batches.
    fn settle(&self, id: ReviewId, seq: u64, work: Arc<Work>, cancel: Arc<AtomicBool>) {
        if !work.pending.is_empty() && round::settle(self, &work, &cancel) {
            let mut state = lock(&self.state);
            if let Some(open) = current(&mut state, id, seq) {
                open.running = false;
                let kind = match open.again.take() {
                    Some(kind) => kind,
                    None => Kind::Reuse,
                };
                self.start_round(&mut state, id, kind, Priority::Background);
            }
            return;
        }
        let mut done = 0;
        let mut since = Instant::now();
        for n in 0..work.files.len() {
            if cancel.load(Ordering::Acquire) || self.wants_again(id, seq) {
                break;
            }
            if lock(&work.results)[n].is_some() {
                continue;
            }
            let result = attribute::file(self, &work, n, &cancel);
            lock(&work.results)[n] = Some(Arc::new(result));
            done += 1;
            if done >= BATCH_FILES || since.elapsed() >= BATCH_TIME {
                self.republish(id, seq);
                done = 0;
                since = Instant::now();
            }
        }
        if done > 0 {
            self.republish(id, seq);
        }
        let mut state = lock(&self.state);
        if let Some(open) = current(&mut state, id, seq) {
            open.running = false;
            if let Some(kind) = open.again.take() {
                self.start_round(&mut state, id, kind, Priority::Interactive);
            }
        }
    }

    fn wants_again(&self, id: ReviewId, seq: u64) -> bool {
        lock(&self.state)
            .reviews
            .get(&id)
            .is_some_and(|o| o.round == seq && o.again.is_some())
    }

    /// A new snapshot of the current work: seen states and attribution as
    /// they are now.
    fn republish(&self, id: ReviewId, seq: u64) {
        let mut events = Vec::new();
        {
            let mut state = lock(&self.state);
            let Some(open) = state.reviews.get(&id) else {
                return;
            };
            if open.round != seq {
                return;
            }
            let Some(work) = open.work.clone() else {
                return;
            };
            let log = self.log_for(&mut state, work.git.common_dir());
            let Some(open) = state.reviews.get_mut(&id) else {
                return;
            };
            let snapshot = self.assemble(open, &work, Some(&log));
            events.push(changed(open, snapshot));
        }
        self.emit(events);
    }

    /// A folder project's session (「未决问题」2, option a): its turns and
    /// calls, no change set, no progress.
    fn folder_round(
        &self,
        id: ReviewId,
        seq: u64,
        summary: lectern_services::session::SessionSummary,
    ) {
        let index: Option<Arc<TurnIndex>> =
            round::source_of(self, &summary.key).and_then(|s| s.turns(&summary.key).ok());
        let own = vec![Own { summary, index }];
        let placed = narrative::place(&own);
        let order = narrative::order(&own, &placed);
        let empty = HashMap::new();
        let turns = narrative::build(
            &narrative::Inputs {
                own: &own,
                placed: &placed,
                order: &order,
                step_files: &empty,
                command_files: &empty,
                last_seen: None,
            },
            &|_| FileKey {
                old: None,
                new: None,
            },
            &|_, _, _| None,
        );
        let mut events = Vec::new();
        {
            let mut state = lock(&self.state);
            let Some(open) = current(&mut state, id, seq) else {
                return;
            };
            let snapshot = ReviewSnapshot {
                state: LoadState::Ready,
                sessions: vec![lectern_services::review::SessionRef {
                    key: own[0].summary.key.clone(),
                    agent: own[0].summary.agent.clone(),
                    spawned_by: None,
                }]
                .into(),
                turns: Some(turns),
                degraded: Some(Degraded::FolderProject),
                ..loading(id, open.target.clone())
            };
            events.push(changed(open, snapshot));
            open.running = false;
        }
        self.emit(events);
    }

    /// The snapshot of `work` with the attribution computed so far and the
    /// seen states of the log's index.
    fn assemble(&self, open: &Open, work: &Work, log: Option<&Arc<RepoLog>>) -> ReviewSnapshot {
        let mem = log.map(|l| lock(&l.mem));
        let index = mem.as_ref().map(|m| &m.index);
        let results = lock(&work.results).clone();
        let files: Arc<[FileEntry]> = work
            .files
            .iter()
            .enumerate()
            .map(|(n, f)| FileEntry {
                key: f.key.clone(),
                change: work.list.files[f.list as usize].clone(),
                noise: f.noise,
                group: f.group,
                package: f.package.clone(),
                attribution: match &results[n] {
                    Some(r) => r.attribution.clone(),
                    None => FileResult::pending(work, f),
                },
            })
            .collect();
        let seen: Arc<[SeenState]> = work
            .files
            .iter()
            .map(|f| index.map_or(SeenState::Unseen, |i| seen::state(i, &f.seen)))
            .collect();
        let last_seen = index.and_then(|i| i.last_seen(&work.target_key));
        let turns = self.turns(work, &results, last_seen);
        ReviewSnapshot {
            id: open.snapshot.id,
            target: work.target.clone(),
            generation: open.snapshot.generation,
            state: LoadState::Ready,
            base: Some(work.base),
            sessions: Arc::clone(&work.sessions),
            files,
            unclaimed: work.unclaimed,
            progress: seen::progress(&seen),
            seen,
            turns,
            groups: Arc::clone(&work.groups),
            brief: empty_brief(),
            focus_review: Arc::clone(&work.focus_review),
            focus_touched: Arc::clone(&work.focus_touched),
            totals: work.list.totals.clone(),
            degraded: work.degraded,
            notes: Arc::from(Vec::new()),
        }
    }

    fn turns(
        &self,
        work: &Work,
        results: &[Option<Arc<FileResult>>],
        last_seen: Option<Timestamp>,
    ) -> Option<Arc<[TurnEntry]>> {
        if !matches!(work.target, ReviewTarget::Session(_)) || work.degraded.is_some() {
            return None;
        }
        Some(narrative::build(
            &narrative::Inputs {
                own: &work.own,
                placed: &work.placed,
                order: &work.order,
                step_files: &work.step_files,
                command_files: &work.command_files,
                last_seen,
            },
            &|f| work.files[f as usize].key.clone(),
            &|s, ordinal, f| attribute::step_entry(work, results, s, ordinal, f),
        ))
    }

    // ---- seen commands and the annotation log ---------------------------

    fn seen_command(
        &self,
        state: &mut State,
        id: ReviewId,
        command: ReviewCommand,
        events: &mut Vec<ReviewEvent>,
    ) {
        let Some(work) = state.reviews.get(&id).and_then(|o| o.work.clone()) else {
            // Still loading: nothing to mark yet.
            return;
        };
        let log = self.log_for(state, work.git.common_dir());
        let Some(open) = state.reviews.get_mut(&id) else {
            return;
        };
        let now = (self.cfg.clock)();
        let mut mem = lock(&log.mem);
        let position = |key: &FileKey| work.files.iter().position(|f| f.key == *key);
        let affected: Vec<usize> = match &command {
            ReviewCommand::MarkSeen(key) => {
                let Some(n) = position(key) else { return };
                self.record_base(&mut mem.index, &work, now);
                mem.index
                    .record(seen::mark_seen(work.files[n].seen.clone(), now));
                self.sharing(&work, n)
            }
            ReviewCommand::MarkUnseen(key) => {
                let Some(n) = position(key) else { return };
                mem.index
                    .record(seen::mark_unseen(work.target_key.clone(), key.clone(), now));
                self.sharing(&work, n)
            }
            _ => {
                let keys: Vec<_> = work.files.iter().map(|f| f.seen.clone()).collect();
                let records = seen::looks_good(&mem.index, &keys, now);
                if !records.is_empty() {
                    self.record_base(&mut mem.index, &work, now);
                }
                for r in records {
                    mem.index.record(r);
                }
                (0..work.files.len()).collect()
            }
        };
        let mut seen: Vec<SeenState> = open.snapshot.seen.to_vec();
        for n in affected {
            if let Some(slot) = seen.get_mut(n) {
                *slot = seen::state(&mem.index, &work.files[n].seen);
            }
        }
        let last_seen = mem.index.last_seen(&work.target_key);
        let flush = !mem.flushing;
        mem.flushing = true;
        drop(mem);
        let turns = open.snapshot.turns.as_ref().map(|turns| {
            turns
                .iter()
                .map(|t| TurnEntry {
                    after_last_seen: last_seen.is_some_and(|s| t.started_at > s),
                    ..t.clone()
                })
                .collect()
        });
        let snapshot = ReviewSnapshot {
            progress: seen::progress(&seen),
            seen: seen.into(),
            turns,
            ..(*open.snapshot).clone()
        };
        events.push(changed(open, snapshot));
        if flush {
            self.submit(Priority::Background, move |engine| engine.flush(&log));
        }
    }

    /// The files whose state a mark of file `n` can change: those sharing
    /// one of its paths.
    fn sharing(&self, work: &Work, n: usize) -> Vec<usize> {
        let f = &work.files[n];
        let mut out = vec![n];
        for p in [&f.key.old, &f.key.new].into_iter().flatten() {
            for &m in work.by_path.get(p).into_iter().flatten() {
                if !out.contains(&(m as usize)) {
                    out.push(m as usize);
                }
            }
        }
        out
    }

    /// The first seen mark of a session fixes its base in the log.
    fn record_base(&self, index: &mut Index, work: &Work, now: Timestamp) {
        let Some((session, entry_at)) = &work.session else {
            return;
        };
        if index.base(session).is_none() {
            index.record(LogRecord::Base(BaseRecord {
                at: now,
                session: session.clone(),
                commit: work.base.commit,
                entry_at: *entry_at,
            }));
        }
    }

    fn log_for(&self, state: &mut State, common_dir: &Path) -> Arc<RepoLog> {
        if let Some(log) = state.logs.get(common_dir) {
            return Arc::clone(log);
        }
        let log = Arc::new(RepoLog {
            file: Mutex::new(LogFile::new(&self.cfg.data_dir, common_dir)),
            mem: Mutex::new(LogMem {
                index: Index::default(),
                loaded: false,
                loading: true,
                flushing: false,
            }),
        });
        state
            .logs
            .insert(common_dir.to_path_buf(), Arc::clone(&log));
        let path = common_dir.to_path_buf();
        let task = Arc::clone(&log);
        self.submit(Priority::Background, move |engine| {
            engine.load_log(&path, &task)
        });
        log
    }

    /// Reads a repository's log whole (`Background`), keeps what was
    /// marked meanwhile, then refreshes the seen states of its reviews; a
    /// session whose recorded base differs from the one its round used is
    /// computed again.
    fn load_log(&self, common_dir: &Path, log: &Arc<RepoLog>) {
        let now = (self.cfg.clock)();
        let (mut index, _report) = lock(&log.file).load(now);
        let pending = {
            let mut mem = lock(&log.mem);
            let pending = mem.index.take_pending();
            for r in &pending {
                index.record(r.clone());
            }
            mem.index = index;
            mem.loaded = true;
            mem.loading = false;
            !pending.is_empty() && !std::mem::replace(&mut mem.flushing, true)
        };
        let mut again: Vec<ReviewId> = Vec::new();
        let mut events = Vec::new();
        {
            let mut state = lock(&self.state);
            let ids: Vec<ReviewId> = state
                .reviews
                .iter()
                .filter(|(_, o)| {
                    o.work
                        .as_ref()
                        .is_some_and(|w| w.git.common_dir() == common_dir)
                })
                .map(|(id, _)| *id)
                .collect();
            for id in ids {
                let Some(open) = state.reviews.get_mut(&id) else {
                    continue;
                };
                let Some(work) = open.work.clone() else {
                    continue;
                };
                let recorded = work
                    .session
                    .as_ref()
                    .and_then(|(s, _)| lock(&log.mem).index.base(s).cloned());
                if let Some(r) = recorded
                    && (r.commit != work.base.commit
                        || work.base.how != lectern_services::review::BaseHow::Recorded)
                {
                    again.push(id);
                }
                let snapshot = self.assemble(open, &work, Some(log));
                events.push(changed(open, snapshot));
            }
            for id in again {
                self.start_round(&mut state, id, Kind::Fetch, Priority::Background);
            }
        }
        self.emit(events);
        if pending {
            self.flush(log);
        }
    }

    /// Appends the queued records (`Background`, holding no other lock
    /// while writing) and compacts when due. A failed append keeps the
    /// records queued for the next command's flush.
    fn flush(&self, log: &Arc<RepoLog>) {
        loop {
            let batch = {
                let mut mem = lock(&log.mem);
                if !mem.loaded {
                    // The load appends what is queued when it is done.
                    mem.flushing = false;
                    return;
                }
                let batch = mem.index.take_pending();
                if batch.is_empty() {
                    mem.flushing = false;
                    return;
                }
                batch
            };
            let now = (self.cfg.clock)();
            let mut file = lock(&log.file);
            if file.append(&batch).is_err() {
                drop(file);
                let mut mem = lock(&log.mem);
                mem.index.restore_pending(batch);
                mem.flushing = false;
                return;
            }
            if file.should_compact() {
                // The index as it is now: records queued since are written
                // again by the next append, which replays the same.
                let mut index = lock(&log.mem).index.clone();
                let _ = file.compact(&mut index, now);
            }
        }
    }

    /// The recorded base of a session, when the log is loaded.
    pub(crate) fn recorded_base(&self, git: &Git, session: &SessionId) -> Option<BaseRecord> {
        let log = lock(&self.state).logs.get(git.common_dir()).cloned()?;
        let mem = lock(&log.mem);
        mem.loaded
            .then(|| mem.index.base(session).cloned())
            .flatten()
    }

    // ---- Prepare ----------------------------------------------------------

    fn prepare(&self, state: &mut State, id: ReviewId, needs: Arc<[Need]>) {
        let Some(open) = state.reviews.get_mut(&id) else {
            return;
        };
        let wanted: HashSet<&Need> = needs.iter().collect();
        open.rows
            .retain(|k, _| wanted.contains(&Need::Rows(k.clone())));
        open.texts.retain(|k, _| wanted.contains(&Need::Text(*k)));
        open.needs = Arc::clone(&needs);
        open.prepare += 1;
        let seq = open.prepare;
        let Some(work) = open.work.clone() else {
            // Prepared when the first snapshot is there: the view sends
            // `Prepare` again on `Changed`.
            return;
        };
        let have_rows: HashSet<RowsKey> = open.rows.keys().cloned().collect();
        let have_texts: HashSet<TextKey> = open.texts.keys().copied().collect();
        self.submit(Priority::Interactive, move |engine| {
            engine.run_prepare(id, seq, work, needs, have_rows, have_texts);
        });
    }

    fn run_prepare(
        &self,
        id: ReviewId,
        seq: u64,
        work: Arc<Work>,
        needs: Arc<[Need]>,
        have_rows: HashSet<RowsKey>,
        have_texts: HashSet<TextKey>,
    ) {
        let cancel = AtomicBool::new(false);
        let result = |n: usize| -> Arc<FileResult> {
            if let Some(r) = lock(&work.results)[n].clone() {
                return r;
            }
            let r = Arc::new(attribute::file(self, &work, n, &cancel));
            lock(&work.results)[n] = Some(Arc::clone(&r));
            r
        };
        let mut rows: Vec<(RowsKey, Arc<FileRows>)> = Vec::new();
        let mut texts: Vec<(TextKey, Arc<str>)> = Vec::new();
        let (mut row_bytes, mut text_bytes) = (0usize, 0usize);
        for need in needs.iter() {
            match need {
                Need::Rows(key) if !have_rows.contains(key) => {
                    if row_bytes >= MAX_ROWS_BYTES {
                        continue;
                    }
                    if let Some(r) = rows::rows_for(self, &work, &result, key) {
                        row_bytes += rows::bytes(&r);
                        rows.push((key.clone(), Arc::new(r)));
                    }
                }
                Need::Text(key) if !have_texts.contains(key) => {
                    if text_bytes >= MAX_TEXT_BYTES {
                        continue;
                    }
                    let Some(session) = work.sessions.get(key.session as usize) else {
                        continue;
                    };
                    let Some(source) = round::source_of(self, &session.key) else {
                        continue;
                    };
                    if let Ok(Content::Text(text)) = source.load(&session.key, &key.at) {
                        text_bytes += text.len();
                        texts.push((*key, text));
                    }
                }
                _ => {}
            }
        }
        let mut events = Vec::new();
        {
            let mut state = lock(&self.state);
            let Some(open) = state.reviews.get_mut(&id) else {
                return;
            };
            if open.prepare != seq {
                return;
            }
            open.rows.extend(rows);
            open.texts.extend(texts);
            events.push(ReviewEvent::Prepared {
                id,
                generation: open.snapshot.generation,
            });
        }
        self.emit(events);
    }

    // ---- shared per-worktree state ----------------------------------------

    pub(crate) fn git(
        &self,
        project: &ProjectKey,
        key: &WorktreeKey,
    ) -> Result<Arc<Git>, ReviewError> {
        if let Some(git) = lock(&self.gits).get(key) {
            return Ok(Arc::clone(git));
        }
        let snapshot = self
            .cfg
            .projects
            .snapshot(project)
            .ok_or(ReviewError::NotInOpenProject)?;
        let worktree = snapshot
            .worktrees
            .iter()
            .find(|w| w.key == *key)
            .filter(|w| !w.prunable)
            .ok_or(ReviewError::Missing)?;
        let found = (self.cfg.discover)(&worktree.root, &self.cfg.git_ceilings)
            .map_err(|e| {
                ReviewError::Changes(lectern_services::changes::ChangeError::Io(
                    e.to_string().into(),
                ))
            })?
            .ok_or(ReviewError::Missing)?;
        let info = found.worktree.ok_or(ReviewError::Missing)?;
        let handle = found
            .repo
            .worktree(&info.id)
            .map_err(|_| ReviewError::Missing)?;
        let common = found.repo.location().common_dir.clone();
        let log_name = info
            .git_dir
            .strip_prefix(&common)
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let git = Arc::new(Git {
            project: project.clone(),
            key: key.clone(),
            root: info.root.clone(),
            hash: found.repo.location().hash,
            repo: found.repo,
            worktree: handle,
            log_name: Arc::from(log_name.as_str()),
        });
        lock(&self.gits).insert(key.clone(), Arc::clone(&git));
        Ok(git)
    }

    /// The worktree's cutoff table for these records and paths: the one in
    /// memory when it answers for them, otherwise built from
    /// `review.tree_changes` and computed misses within `budget`.
    pub(crate) fn table(
        &self,
        git: &Git,
        records: &[Record],
        paths: &HashSet<ChangePath>,
        budget: Option<Duration>,
        cancel: &AtomicBool,
    ) -> Table {
        if let Some(t) = lock(&self.tables).get(&git.key)
            && round::table_covers(t, records, paths)
        {
            return t.clone();
        }
        if records.is_empty() {
            return Table::default();
        }
        let partition = self.cfg.cache.repo(git.common_dir());
        let table = cutoff::build(
            &git.repo,
            &partition,
            records,
            paths.clone(),
            budget,
            cancel,
        );
        lock(&self.tables).insert(git.key.clone(), table.clone());
        table
    }

    /// Completes the unknown records of the worktree's table. True when
    /// any became known.
    pub(crate) fn complete_table(
        &self,
        git: &Git,
        records: &[Record],
        paths: &HashSet<ChangePath>,
        cancel: &AtomicBool,
    ) -> bool {
        let mut table = match lock(&self.tables).get(&git.key) {
            Some(t) if round::table_covers(t, records, paths) => t.clone(),
            _ => Table {
                covered: paths.clone(),
                changes: HashMap::new(),
            },
        };
        let before = table.unknown(records).len();
        let partition = self.cfg.cache.repo(git.common_dir());
        cutoff::complete(&git.repo, &partition, &mut table, records, cancel);
        let after = table.unknown(records).len();
        lock(&self.tables).insert(git.key.clone(), table);
        after < before
    }

    pub(crate) fn adapter(&self, source: &SourceId) -> Option<u32> {
        lock(&self.adapters).get(source).copied()
    }

    pub(crate) fn adapter_seen(&self, source: &SourceId, version: u32) {
        let mut adapters = lock(&self.adapters);
        let old = adapters.insert(source.clone(), version);
        drop(adapters);
        if old.is_some_and(|v| v != version) {
            // Claims of the old version no longer stand.
            lock(&self.claims).retain(|k, c| k.source != *source || c.adapter_version == version);
        }
    }

    pub(crate) fn memory_claims(
        &self,
        key: &SessionKey,
        last_activity: Timestamp,
    ) -> Option<Arc<Claims>> {
        lock(&self.claims)
            .get(key)
            .filter(|c| c.last_activity == last_activity)
            .cloned()
    }

    pub(crate) fn remember_claims(&self, key: &SessionKey, claims: &Arc<Claims>) {
        lock(&self.claims).insert(key.clone(), Arc::clone(claims));
    }

    pub(crate) fn version(&self, oid: &lectern_services::project::BlobId) -> Option<Arc<[u8]>> {
        lock(&self.versions).get(oid)
    }

    pub(crate) fn keep_version(&self, oid: lectern_services::project::BlobId, content: Arc<[u8]>) {
        lock(&self.versions).put(oid, content);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn version_bytes(&self) -> usize {
        lock(&self.versions).bytes()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn work(&self, id: ReviewId) -> Option<Arc<Work>> {
        lock(&self.state).reviews.get(&id)?.work.clone()
    }
}

/// The open review when round `seq` is still its current one.
fn current(state: &mut State, id: ReviewId, seq: u64) -> Option<&mut Open> {
    state.reviews.get_mut(&id).filter(|o| o.round == seq)
}

/// Replaces the snapshot with `snapshot` at the next generation.
fn changed(open: &mut Open, snapshot: ReviewSnapshot) -> ReviewEvent {
    let generation = open.snapshot.generation + 1;
    open.snapshot = Arc::new(ReviewSnapshot {
        generation,
        ..snapshot
    });
    ReviewEvent::Changed {
        id: open.snapshot.id,
        generation,
    }
}
