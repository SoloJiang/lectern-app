//! The review engine (docs/design/review.md「数据流与线程」): open reviews,
//! their immutable snapshots, the commands, `Prepare` with its rows and
//! texts, and the annotation log of each repository. `plugin` wraps it in
//! the "review" plugin (step 4); the engine tests and benches drive it
//! without the plugin through `testing::Driven`.
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
//! - Refresh (`watch`): each open review watches its repository and its
//!   worktree through fs_watch; a batch becomes a `RefreshHint`, a
//!   `SessionsChanged` becomes `Kind::Reuse`. A request during a round is
//!   merged into the next one (the path union, `Full` over the rest).
//! - Release: the last `close` keeps the review for `RETAIN` (the host's
//!   timer); `open` in that time revives it, the timer's task releases it.
//!   Without a host (`testing::Driven`) `close` releases at once.
//! - Locks: `state`, then a repository log's `mem`; the other mutexes are
//!   leaves. No lock is held across IO, and events are emitted after the
//!   state lock is released.

pub(crate) mod attribute;
mod brief;
mod narrative;
pub(crate) mod overview;
pub(crate) mod round;
mod rows;
mod watch;

#[cfg(test)]
pub(crate) mod tests;

use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use lectern_cache::Cache;
use lectern_executor::{Executor, Priority};
use lectern_fs_watch::Watcher;
use lectern_git::{Discovered, GitError};
use lectern_kernel::BusyGuard;
use lectern_logging::{Ctx, Value, debug, info, warn};
use lectern_services::changes::{ChangeError, ChangePath, RefreshHint};
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
use crate::diag;
use crate::log::{BaseRecord, Index, LogFile, Record as LogRecord, SessionId};
use crate::lru::Versions;
use crate::seen;

use self::attribute::FileResult;
use self::narrative::Own;
use self::round::{Git, Kind, StepIds, Work};
use self::watch::Watching;
use self::watch::merge_hints;

/// Finds a repository from a worktree root (`git::Repo::discover`; the
/// tests read only the repository's own configuration).
pub(crate) type Discover =
    Arc<dyn Fn(&Path, &[PathBuf]) -> Result<Option<Discovered>, GitError> + Send + Sync>;

/// Publish the background attribution after this many files…
const BATCH_FILES: usize = 8;
/// …or this long, whichever comes first.
const BATCH_TIME: Duration = Duration::from_millis(50);
/// How long a review nobody holds is kept, for a return to the project
/// home (docs/design/review.md「数据流与线程」).
pub(crate) const RETAIN: Duration = Duration::from_secs(30);
/// Prepared rows kept per review, and prepared texts.
const MAX_ROWS_BYTES: usize = 8 << 20;
const MAX_TEXT_BYTES: usize = 2 << 20;

/// A service used for one call: the plugin's `Admitted` (the provider is
/// deactivated only when no call is in it), or a plain `Arc` in the tests.
pub(crate) struct Held<S: ?Sized + 'static>(Box<dyn Deref<Target = S> + Send>);

impl<S: ?Sized + 'static> Held<S> {
    pub(crate) fn new(service: impl Deref<Target = S> + Send + 'static) -> Held<S> {
        Held(Box::new(service))
    }
}

impl<S: ?Sized + 'static> Deref for Held<S> {
    type Target = S;

    fn deref(&self) -> &S {
        &self.0
    }
}

/// The live session sources, each held for the caller's use.
pub(crate) type SourcesFn = Arc<dyn Fn() -> Vec<Held<dyn SessionSource>> + Send + Sync>;
/// The project service, None once its provider is gone.
pub(crate) type ProjectsFn = Arc<dyn Fn() -> Option<Held<dyn Projects>> + Send + Sync>;

/// What the plugin gives the engine in place of the bare executor
/// (docs/design/review.md「数据流与线程」): tasks go through the kernel's
/// `own_with` holding a `BusyGuard` until their results are published, and
/// the 30-second retention has a timer.
pub(crate) trait Host: Send + Sync {
    /// Runs `work` on the executor. Dropped when the plugin is gone.
    fn spawn(&self, priority: Priority, work: Box<dyn FnOnce() + Send>);
    /// Calls `work` once `delay` has passed, on any thread; it only submits.
    fn later(&self, delay: Duration, work: Box<dyn FnOnce() + Send>);
    /// A guard for work that is not yet a task (the sinks of fs_watch).
    fn busy(&self) -> Option<BusyGuard>;
}

pub(crate) struct Config {
    pub executor: Executor,
    pub cache: Cache,
    /// The annotation logs' directory (ADR 0044).
    pub data_dir: PathBuf,
    pub git_ceilings: Arc<[PathBuf]>,
    pub projects: ProjectsFn,
    pub sources: SourcesFn,
    /// Some in the plugin; the tests of the engine have neither.
    pub host: Option<Arc<dyn Host>>,
    /// Some in the plugin: the review's repository and worktree watches.
    pub watcher: Option<Watcher>,
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
    /// (ancestor, commit) → is it its ancestor; `review.ancestry` behind it.
    ancestry: Mutex<HashMap<(lectern_git::Oid, lectern_git::Oid), bool>>,
    /// The `BriefRules` providers registered (「简报规则的扩展点」).
    providers: Mutex<brief::Providers>,
    /// The project overviews with an interest (「概览」). A leaf lock.
    overviews: Mutex<overview::Overviews>,
    /// IO faults and size limits for the logs opened from now on.
    #[cfg(test)]
    log_test: Mutex<LogTest>,
}

#[cfg(test)]
#[derive(Default)]
struct LogTest {
    faults: Option<crate::log::Faults>,
    limits: Option<crate::log::Limits>,
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
    /// Appends that failed in a row, kept for the next retry.
    failures: u32,
}

struct Open {
    target: ReviewTarget,
    /// The log scope of its tasks: the target session's tag, then with the
    /// review's hash once a round resolved the repository.
    ctx: Ctx,
    /// When `open` was called: the `review ready` and `review settled`
    /// records' `took_us`.
    opened: Instant,
    /// Whether `review ready` and `review settled` were written.
    ready: bool,
    settled: bool,
    /// Rounds prepared again since the open because claims or cutoffs
    /// became known.
    reprepared: u32,
    /// Zero while the review is kept for `RETAIN` after its last `close`.
    refs: u32,
    /// Counts the closes to zero and the opens that revive: the release
    /// timer of an earlier close finds another number and does nothing.
    idle: u64,
    /// The repository and worktree watches, once a round resolved them.
    watch: Option<Watching>,
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
    /// The providers' request for the current work and their replies.
    brief: brief::BriefState,
}

impl Open {
    fn held(&self) -> bool {
        self.refs > 0
    }
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
            ancestry: Mutex::new(HashMap::new()),
            providers: Mutex::new(brief::Providers::default()),
            overviews: Mutex::new(overview::Overviews::default()),
            #[cfg(test)]
            log_test: Mutex::new(LogTest::default()),
        })
    }

    /// The annotation logs opened from now on fail where `faults` says and
    /// use `limits` (the `logs_*` tests).
    #[cfg(test)]
    pub(crate) fn set_log_test(
        &self,
        faults: impl Fn(crate::log::Op) -> std::io::Result<()> + Send + Sync + 'static,
        limits: crate::log::Limits,
    ) {
        let mut test = lock(&self.log_test);
        test.faults = Some(Arc::new(faults));
        test.limits = Some(limits);
    }

    /// Runs `job` on an executor task, inside the log scope `ctx` (the
    /// review's `review` and `session`, docs/design/review.md「日志与诊断」).
    /// In the plugin the task holds a `BusyGuard` until `job` returns, so
    /// that the kernel is not quiescent before the results are published;
    /// the guard is taken here, before the task exists, and a job that
    /// submits the next one does so before it returns.
    fn submit(
        &self,
        priority: Priority,
        ctx: Ctx,
        job: impl FnOnce(&Arc<Engine>) + Send + 'static,
    ) {
        let me = self.me.clone();
        let work = move || {
            let _scope = lectern_logging::scope(ctx);
            if let Some(engine) = me.upgrade() {
                job(&engine);
            }
        };
        match &self.cfg.host {
            Some(host) => host.spawn(priority, Box::new(work)),
            None => self.cfg.executor.run(priority, work).detach(),
        }
    }

    /// Emits the events; after a `Changed`, asks the brief providers about
    /// a work they have not seen (`engine::brief`).
    fn emit(&self, events: Vec<ReviewEvent>) {
        for e in events {
            let changed = match &e {
                ReviewEvent::Changed { id, .. } => Some(*id),
                _ => None,
            };
            (self.cfg.emit)(e);
            if let Some(id) = changed {
                self.ask_providers(id);
            }
        }
    }

    // ---- the service's calls -------------------------------------------

    pub(crate) fn open(&self, target: ReviewTarget) -> ReviewId {
        let mut state = lock(&self.state);
        if let Some(&id) = state.targets.get(&target) {
            if let Some(open) = state.reviews.get_mut(&id) {
                // A kept review is revived as it is: its watches kept it
                // current, nothing is computed again.
                open.refs += 1;
                open.idle += 1;
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
                ctx: diag::target_ctx(&target),
                opened: Instant::now(),
                ready: false,
                settled: false,
                reprepared: 0,
                refs: 1,
                idle: 0,
                watch: None,
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
                brief: brief::BriefState::default(),
            },
        );
        self.start_round(&mut state, id, Kind::Fetch, Priority::Interactive);
        id
    }

    pub(crate) fn snapshot(&self, id: ReviewId) -> Option<Arc<ReviewSnapshot>> {
        lock(&self.state)
            .reviews
            .get(&id)
            .filter(|o| o.held())
            .map(|o| Arc::clone(&o.snapshot))
    }

    pub(crate) fn rows(&self, id: ReviewId, key: &RowsKey) -> Option<Arc<FileRows>> {
        let state = lock(&self.state);
        let open = state.reviews.get(&id).filter(|o| o.held())?;
        open.rows.get(key).cloned()
    }

    pub(crate) fn text(&self, id: ReviewId, key: &TextKey) -> Option<Arc<str>> {
        let state = lock(&self.state);
        let open = state.reviews.get(&id).filter(|o| o.held())?;
        open.texts.get(key).cloned()
    }

    /// Drops a reference. With the last one the review is kept for
    /// `RETAIN` (the host's timer), then released; without a host it is
    /// released at once.
    pub(crate) fn close(&self, id: ReviewId) {
        let mut state = lock(&self.state);
        let Some(open) = state.reviews.get_mut(&id) else {
            return;
        };
        if open.refs == 0 {
            return;
        }
        open.refs -= 1;
        if open.refs > 0 {
            return;
        }
        let Some(host) = self.cfg.host.clone() else {
            let released = self.release(state, id);
            drop(released);
            return;
        };
        open.idle += 1;
        let (ctx, idle) = (open.ctx, open.idle);
        drop(state);
        let me = self.me.clone();
        host.later(
            RETAIN,
            Box::new(move || {
                let Some(engine) = me.upgrade() else { return };
                engine.submit(Priority::Background, ctx, move |engine| {
                    engine.expire(id, idle)
                });
            }),
        );
    }

    /// The retention of close number `idle` ended (`Background`): releases
    /// the review unless it was opened again, or closed again, since.
    fn expire(&self, id: ReviewId, idle: u64) {
        let state = lock(&self.state);
        let due = state
            .reviews
            .get(&id)
            .is_some_and(|o| o.refs == 0 && o.idle == idle);
        if due {
            let released = self.release(state, id);
            debug!("review released"; id = id.0);
            drop(released);
        }
    }

    /// Removes the review and the worktree state no remaining review uses.
    /// The review is returned, to be dropped with no lock held: its watches
    /// unsubscribe from fs_watch.
    fn release(&self, mut state: MutexGuard<'_, State>, id: ReviewId) -> Option<Open> {
        let open = state.reviews.remove(&id)?;
        open.cancel.store(true, Ordering::Release);
        open.brief.cancel();
        state.targets.remove(&open.target);
        let mut used: HashSet<WorktreeKey> = state
            .reviews
            .values()
            .filter_map(|o| o.work.as_ref().map(|w| w.git.key.clone()))
            .collect();
        let mut sessions: HashSet<SessionKey> = state
            .reviews
            .values()
            .filter_map(|o| o.work.as_ref())
            .flat_map(|w| w.whos.iter().map(|who| who.key.clone()))
            .collect();
        let empty = state.reviews.is_empty();
        drop(state);
        // The overviews hold worktrees and sessions too.
        let (worktrees, held) = self.overview_retain();
        used.extend(worktrees);
        sessions.extend(held);
        lock(&self.gits).retain(|k, _| used.contains(k));
        lock(&self.tables).retain(|k, _| used.contains(k));
        lock(&self.claims).retain(|k, _| sessions.contains(k));
        if empty {
            lock(&self.versions).clear();
            lock(&self.ancestry).clear();
        }
        Some(open)
    }

    /// The plugin is deactivated: every review goes, with its watches.
    pub(crate) fn shutdown(&self) {
        let reviews = {
            let mut state = lock(&self.state);
            state.targets.clear();
            std::mem::take(&mut state.reviews)
        };
        for open in reviews.values() {
            open.cancel.store(true, Ordering::Release);
            open.brief.cancel();
        }
        drop(reviews);
        lock(&self.providers).clear();
        self.overviews_clear();
        lock(&self.gits).clear();
        lock(&self.tables).clear();
        lock(&self.claims).clear();
        lock(&self.versions).clear();
        lock(&self.ancestry).clear();
    }

    pub(crate) fn command(&self, id: ReviewId, command: ReviewCommand) {
        let mut events = Vec::new();
        {
            let mut state = lock(&self.state);
            if !state.reviews.get(&id).is_some_and(Open::held) {
                return;
            }
            match command {
                ReviewCommand::MarkSeen(_)
                | ReviewCommand::MarkUnseen(_)
                | ReviewCommand::LooksGood => {
                    self.seen_command(&mut state, id, command, &mut events);
                    // The progress of the overview's sessions changed.
                    if let Some(dir) = state
                        .reviews
                        .get(&id)
                        .and_then(|o| o.work.as_ref())
                        .map(|w| w.git.common_dir().to_path_buf())
                    {
                        drop(state);
                        self.overviews_for_repo(&dir, overview::Trigger::default());
                        self.emit(events);
                        return;
                    }
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
            // After the current round, as one: the path sets are joined, a
            // full refresh wins over the rest.
            open.again = Some(merge_hints(open.again.take(), kind));
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
        self.submit(priority, open.ctx, move |engine| {
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
        let started = Instant::now();
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
                    // A round cancelled by `close` or a newer round is not a
                    // failure: no record, no `Failed` snapshot.
                    if let Some(open) = current(&mut state, id, seq) {
                        if !matches!(error, ReviewError::Changes(ChangeError::Cancelled)) {
                            info!("review failed";
                                id = id.0,
                                round = seq,
                                kind = diag::kind(&kind),
                                error = diag::review_error(&error));
                        }
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
        let _scope = lectern_logging::scope(work.ctx);
        let prepared_in = started.elapsed();
        // The first file in the narrative's order is attributed before the
        // first snapshot.
        let first = Instant::now();
        if matches!(target, ReviewTarget::Session(_)) && !work.files.is_empty() {
            let result = attribute::file(self, &work, 0, &cancel);
            lock(&work.results)[0] = Some(Arc::new(result));
        }
        let first = first.elapsed();
        debug!("review round";
            id = id.0,
            round = seq,
            kind = diag::kind(&kind),
            files = work.files.len(),
            unclaimed = work.unclaimed,
            pending = !work.pending.is_empty(),
            took_us = prepared_in);
        let mut events = Vec::new();
        {
            let mut state = lock(&self.state);
            let log = self.log_for(&mut state, work.git.common_dir(), work.ctx);
            let Some(open) = current(&mut state, id, seq) else {
                return;
            };
            open.ctx = work.ctx;
            if !open.ready {
                open.ready = true;
                let t = &work.timing;
                info!("review ready";
                    id = id.0,
                    round = seq,
                    base = diag::base_how(work.base.how),
                    files = work.files.len(),
                    took_us = open.opened.elapsed(),
                    locate_us = t.locate,
                    turns_us = t.turns,
                    fetch_us = t.fetch,
                    claims_us = t.claims,
                    cutoffs_us = t.cutoffs,
                    first_us = first);
            }
            open.work = Some(Arc::clone(&work));
            // Rows of the previous work show its texts and attribution:
            // drop them and prepare what the view asked for again. Texts
            // are at fixed positions of append-only session files: kept.
            open.rows.clear();
            let needs = Arc::clone(&open.needs);
            let snapshot = self.assemble(open, &work, Some(&log));
            events.push(changed(open, snapshot));
            if !needs.is_empty() {
                self.prepare(&mut state, id, needs);
            }
        }
        self.emit(events);
        let ctx = work.ctx;
        // After the first snapshot: arming is queued for fs_watch's thread,
        // and its first batch (a rescan) refreshes once more.
        self.ensure_watch(id, &work.git);
        self.submit(Priority::Background, ctx, move |engine| {
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
                    None => {
                        open.reprepared += 1;
                        Kind::Reuse
                    }
                };
                self.start_round(&mut state, id, kind, Priority::Background);
            }
            return;
        }
        let mut done = 0;
        let mut since = Instant::now();
        let mut finished = true;
        for n in 0..work.files.len() {
            if cancel.load(Ordering::Acquire) || self.wants_again(id, seq) {
                finished = false;
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
            if finished && !open.settled && open.again.is_none() {
                open.settled = true;
                info!("review settled";
                    id = id.0,
                    round = seq,
                    files = work.files.len(),
                    reprepared = open.reprepared,
                    took_us = open.opened.elapsed());
            }
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
            let log = self.log_for(&mut state, work.git.common_dir(), work.ctx);
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
            if let Some(kind) = open.again.take() {
                self.start_round(&mut state, id, kind, Priority::Background);
            }
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
        let brief = self.brief(open, work, &results, &seen);
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
            brief,
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
        let log = self.log_for(state, work.git.common_dir(), work.ctx);
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
            self.submit(Priority::Background, work.ctx, move |engine| {
                engine.flush(&log)
            });
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

    /// The repository's log; the first call reads it on a `Background`
    /// task, in the scope `ctx` of the review that asked (a log is per
    /// repository: its records carry that review's IDs).
    /// A repository's log once it is read (starting the read when it has
    /// not begun); None until then.
    fn loaded_log(&self, common_dir: &Path, ctx: Ctx) -> Option<Arc<RepoLog>> {
        let log = {
            let mut state = lock(&self.state);
            self.log_for(&mut state, common_dir, ctx)
        };
        let loaded = lock(&log.mem).loaded;
        loaded.then_some(log)
    }

    fn log_for(&self, state: &mut State, common_dir: &Path, ctx: Ctx) -> Arc<RepoLog> {
        if let Some(log) = state.logs.get(common_dir) {
            return Arc::clone(log);
        }
        #[allow(unused_mut, reason = "faults and limits are set in tests only")]
        let mut file = LogFile::new(&self.cfg.data_dir, common_dir);
        #[cfg(test)]
        {
            let test = lock(&self.log_test);
            if let Some(faults) = &test.faults {
                file.set_shared_faults(Arc::clone(faults));
            }
            if let Some(limits) = test.limits {
                file.set_limits(limits);
            }
        }
        let log = Arc::new(RepoLog {
            file: Mutex::new(file),
            mem: Mutex::new(LogMem {
                index: Index::default(),
                loaded: false,
                loading: true,
                flushing: false,
                failures: 0,
            }),
        });
        state
            .logs
            .insert(common_dir.to_path_buf(), Arc::clone(&log));
        let path = common_dir.to_path_buf();
        let task = Arc::clone(&log);
        self.submit(Priority::Background, ctx, move |engine| {
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
        // The overviews' progress reads this index.
        self.overviews_for_repo(common_dir, overview::Trigger::default());
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
            if let Err(e) = file.append(&batch) {
                let path = file.path().to_path_buf();
                drop(file);
                let mut mem = lock(&log.mem);
                mem.failures += 1;
                warn!("annotation log append failed; kept for the next mark";
                    path = Value::path(&path),
                    io = Value::io(&e),
                    records = batch.len(),
                    failures = mem.failures);
                mem.index.restore_pending(batch);
                mem.flushing = false;
                return;
            }
            let failures = std::mem::take(&mut lock(&log.mem).failures);
            if failures > 0 {
                warn!("annotation log append retried";
                    path = Value::path(file.path()),
                    records = batch.len(),
                    failures = failures);
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
        self.submit(Priority::Interactive, work.ctx, move |engine| {
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
        let started = Instant::now();
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
        debug!("prepared";
            id = id.0,
            request = seq,
            needs = needs.len(),
            rows = rows.len(),
            texts = texts.len(),
            row_bytes = row_bytes,
            text_bytes = text_bytes,
            versions_bytes = lock(&self.versions).bytes(),
            took_us = started.elapsed());
        let mut events = Vec::new();
        {
            let mut state = lock(&self.state);
            let Some(open) = state.reviews.get_mut(&id) else {
                return;
            };
            // A newer request, or a newer round whose work these rows are
            // not of (its install prepares the request again).
            let same_work = open.work.as_ref().is_some_and(|w| Arc::ptr_eq(w, &work));
            if open.prepare != seq || !same_work {
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
        let projects = (self.cfg.projects)().ok_or(ReviewError::NotInOpenProject)?;
        let snapshot = projects
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

    /// Whether `ancestor` is an ancestor of `of`: from memory, else
    /// `review.ancestry`, else walked. An error counts as not (the record
    /// then cuts the paths it changed: the conservative side). Blocking.
    pub(crate) fn descends(
        &self,
        git: &Git,
        ancestor: lectern_git::Oid,
        of: lectern_git::Oid,
        cancel: &AtomicBool,
    ) -> bool {
        if let Some(&found) = lock(&self.ancestry).get(&(ancestor, of)) {
            return found;
        }
        let partition = self.cfg.cache.repo(git.common_dir());
        let Some(found) = cutoff::descends(&git.repo, &partition, ancestor, of, cancel) else {
            return false;
        };
        lock(&self.ancestry).insert((ancestor, of), found);
        found
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

    /// The reviews kept, open or waiting out their retention.
    #[cfg(test)]
    pub(crate) fn kept(&self) -> usize {
        lock(&self.state).reviews.len()
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
