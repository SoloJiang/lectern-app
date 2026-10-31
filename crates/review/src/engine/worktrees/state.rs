//! The worktrees overview's per-project state, its interests and the
//! marks that say what is stale. Every entry point here only takes the
//! `worktrees` lock and submits tasks.

use super::*;

/// Which counts of a worktree are to be made again.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) struct Need {
    pub uncommitted: bool,
    pub committed: bool,
}

impl Need {
    pub(super) const BOTH: Need = Need {
        uncommitted: true,
        committed: true,
    };
    pub(super) const UNCOMMITTED: Need = Need {
        uncommitted: true,
        committed: false,
    };
    pub(super) const COMMITTED: Need = Need {
        uncommitted: false,
        committed: true,
    };

    pub(super) fn any(self) -> bool {
        self.uncommitted || self.committed
    }

    pub(super) fn merge(&mut self, other: Need) {
        self.uncommitted |= other.uncommitted;
        self.committed |= other.committed;
    }
}

/// What happened since the last round that makes counts stale. Only
/// marks: the listeners that fill it read nothing and do no IO.
#[derive(Default)]
pub(super) struct Marks {
    /// Everything again (a refresh, a rescan, a failed watch).
    pub all: bool,
    /// HEAD or the index of a worktree changed, by its watch name.
    pub names: HashMap<WorktreeName, Need>,
    /// Worktrees by key (the overview published a count set).
    pub keys: HashMap<WorktreeKey, Need>,
    /// A reference or the configuration changed: the default branch may
    /// have moved, and HEAD may have moved without its file changing.
    pub refs: bool,
    /// The current worktree changed.
    pub active: bool,
    /// Sessions that changed: the round finds their worktrees.
    pub sessions: HashSet<SessionKey>,
    /// More sessions than `MAX_MARKED_SESSIONS`.
    pub sessions_all: bool,
}

impl Marks {
    pub(super) fn is_empty(&self) -> bool {
        !self.all
            && !self.refs
            && !self.active
            && !self.sessions_all
            && self.names.is_empty()
            && self.keys.is_empty()
            && self.sessions.is_empty()
    }

    /// Puts marks a round took but did not use back.
    pub(super) fn merge(&mut self, other: Marks) {
        self.all |= other.all;
        self.refs |= other.refs;
        self.active |= other.active;
        for (name, need) in other.names {
            self.names.entry(name).or_default().merge(need);
        }
        for (key, need) in other.keys {
            self.keys.entry(key).or_default().merge(need);
        }
        self.sessions_all |= other.sessions_all;
        self.sessions.extend(other.sessions);
        if self.sessions.len() > MAX_MARKED_SESSIONS {
            self.sessions_all = true;
            self.sessions.clear();
        }
    }
}

/// What two worktrees' overlap holds: the common paths' count and the
/// first of them, shared by both directions.
#[derive(Clone)]
pub(super) struct Pair {
    pub files: u32,
    pub paths: Arc<[RelPath]>,
}

/// The default branch as the last round resolved it.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct BaseRef {
    pub name: Arc<str>,
    pub oid: Oid,
}

/// One worktree's counts and what the next comparison reuses.
pub(super) struct Entry {
    /// The HEAD commit the counts are of; None when unborn or unreadable.
    pub head: Option<Oid>,
    /// The merge base of the default branch and `head`.
    pub merge_base: Option<Oid>,
    pub uncommitted: ChangeCount,
    pub committed: ChangeCount,
    pub vs_base: ChangeCount,
    pub counted_at: Option<Timestamp>,
    /// When the last comparison ended and how long it took (the throttle).
    pub finished: Option<Timestamp>,
    pub took: Duration,
    /// The 64-bit hashes of the paths changed against the base (against
    /// HEAD without a base), sorted; None once released or when unknown.
    pub hashes: Option<Arc<[u64]>>,
    /// The path set is known (the overlaps are meaningful), whether or not
    /// the hashes are still held.
    pub paths_known: bool,
    pub need: Need,
}

/// One project's worktrees overview.
pub(super) struct WtState {
    pub interests: u64,
    pub generation: u64,
    pub snapshot: Option<Arc<WorktreesOverview>>,
    /// The non-prunable worktrees as the last round listed them, current
    /// first.
    pub order: Vec<WorktreeKey>,
    pub entries: HashMap<WorktreeKey, Entry>,
    /// Overlaps of two worktrees, by the pair in key order.
    pub pairs: HashMap<(WorktreeKey, WorktreeKey), Pair>,
    /// The default branch; `base_known` says whether a round resolved it.
    pub base: Option<BaseRef>,
    pub base_known: bool,
    /// The default branch is to be resolved again (a reference changed).
    pub base_stale: bool,
    pub merge_bases: HashMap<(Oid, Oid), Option<Oid>>,
    pub marks: Marks,
    /// The repository's watch, kept until the project closes: the events
    /// keep marking while nobody watches.
    pub watch: Option<RepoWatch>,
    pub running: bool,
    pub round: u64,
    pub cancel: Arc<AtomicBool>,
    /// The instant (clock milliseconds) a wake for the throttle was asked
    /// for, so that one wake per instant is.
    pub wake: Option<i64>,
    /// Counts the releases: the timer that lets the hashes go finds
    /// another number when the project was watched again.
    pub released: u64,
    pub ctx: Ctx,
}

impl WtState {
    fn new(ctx: Ctx) -> WtState {
        WtState {
            interests: 0,
            generation: 0,
            snapshot: None,
            order: Vec::new(),
            entries: HashMap::new(),
            pairs: HashMap::new(),
            base: None,
            base_known: false,
            base_stale: false,
            merge_bases: HashMap::new(),
            marks: Marks::default(),
            watch: None,
            running: false,
            round: 0,
            cancel: Arc::new(AtomicBool::new(false)),
            wake: None,
            released: 0,
            ctx,
        }
    }

    pub(super) fn hash_count(&self) -> usize {
        self.entries
            .values()
            .filter_map(|e| e.hashes.as_ref())
            .map(|h| h.len())
            .sum()
    }
}

/// The worktrees overviews, by project, and the round numbers across them.
pub(crate) struct WtStates {
    pub(super) projects: HashMap<ProjectKey, WtState>,
    rounds: u64,
    hash_limit: usize,
}

impl Default for WtStates {
    fn default() -> WtStates {
        WtStates {
            projects: HashMap::new(),
            rounds: 0,
            hash_limit: HASH_LIMIT,
        }
    }
}

impl WtStates {
    /// Numbers a new round of `project`'s state and cancels the running
    /// one; the state then runs it.
    fn begin(&mut self, project: &ProjectKey) -> Option<(u64, Ctx)> {
        self.rounds += 1;
        let round = self.rounds;
        let state = self.projects.get_mut(project)?;
        state.cancel.store(true, Ordering::Release);
        state.cancel = Arc::new(AtomicBool::new(false));
        state.running = true;
        state.round = round;
        Some((round, state.ctx))
    }
}

impl Engine {
    /// `Review::worktrees`: the last result, kept after the interests went
    /// until the project closes.
    pub(crate) fn worktrees(&self, project: &ProjectKey) -> Option<Arc<WorktreesOverview>> {
        lock(&self.worktrees)
            .projects
            .get(project)
            .and_then(|s| s.snapshot.clone())
    }

    /// `Review::watch_worktrees`: the first interest asks for the stale and
    /// missing counts at once; the returned value's drop releases one.
    pub(crate) fn watch_worktrees(&self, project: &ProjectKey) -> OverviewInterest {
        let ProjectKey::Git { common_dir } = project else {
            // A folder has no worktrees.
            return OverviewInterest::new(|| {});
        };
        let now = (self.cfg.clock)();
        let start = {
            let mut states = lock(&self.worktrees);
            let state = states
                .projects
                .entry(project.clone())
                .or_insert_with(|| WtState::new(diag::worktrees_ctx(common_dir)));
            state.interests += 1;
            let first = state.interests == 1;
            if first {
                // Counts that went stale while nobody watched: the ones
                // older than five minutes (no file watch on the others)
                // and those whose hashes were let go. The ones whose
                // events marked them are in `marks`.
                for entry in state.entries.values_mut() {
                    let old = entry
                        .counted_at
                        .is_none_or(|at| now.0.saturating_sub(at.0) >= STALE_MS);
                    if old || (entry.paths_known && entry.hashes.is_none()) {
                        entry.need = Need::BOTH;
                    }
                }
                state.released += 1;
            }
            if first { states.begin(project) } else { None }
        };
        if let Some((round, ctx)) = start {
            self.spawn_worktrees_round(project, round, ctx);
        }
        let me = self.me.clone();
        let project = project.clone();
        OverviewInterest::new(move || {
            if let Some(engine) = me.upgrade() {
                engine.worktrees_release(&project);
            }
        })
    }

    /// `Review::refresh_worktrees`: every worktree stale, recomputed under
    /// the throttle. With no interest nothing is asked for.
    pub(crate) fn refresh_worktrees(&self, project: &ProjectKey) {
        let held = lock(&self.worktrees)
            .projects
            .get(project)
            .is_some_and(|s| s.interests > 0);
        if held {
            self.worktrees_mark(project, |m| {
                m.all = true;
                m.refs = true;
            });
        }
    }

    /// Records what is stale and, with an interest and no round running,
    /// starts one. Without a state (no interest ever) there is nothing to
    /// keep: the first interest computes everything.
    pub(super) fn worktrees_mark(&self, project: &ProjectKey, mark: impl FnOnce(&mut Marks)) {
        let start = {
            let mut states = lock(&self.worktrees);
            let Some(state) = states.projects.get_mut(project) else {
                return;
            };
            mark(&mut state.marks);
            if state.interests == 0 || state.running {
                None
            } else {
                states.begin(project)
            }
        };
        if let Some((round, ctx)) = start {
            self.spawn_worktrees_round(project, round, ctx);
        }
    }

    /// The overview of the current worktree published its count set: that
    /// worktree's uncommitted count comes from it.
    pub(in crate::engine) fn worktrees_uncommitted_changed(
        &self,
        project: &ProjectKey,
        key: WorktreeKey,
    ) {
        self.worktrees_mark(project, |m| {
            m.keys.entry(key).or_default().merge(Need::UNCOMMITTED);
        });
    }

    /// The overview's last interest went: the current worktree's
    /// uncommitted count is made again here, whatever it waited for.
    pub(in crate::engine) fn worktrees_current_unsettled(&self, project: &ProjectKey) {
        self.worktrees_mark(project, |m| m.active = true);
    }

    /// `SessionsChanged`: an agent working in a worktree that nobody
    /// watches grows its session file, the only signal there.
    pub(crate) fn worktrees_sessions_changed(
        &self,
        event: &lectern_services::session::SessionsChanged,
    ) {
        if event.changed.is_empty() {
            return;
        }
        let projects: Vec<ProjectKey> = lock(&self.worktrees).projects.keys().cloned().collect();
        for project in projects {
            self.worktrees_mark(&project, |m| {
                for key in event.changed.iter() {
                    if m.sessions.len() >= MAX_MARKED_SESSIONS {
                        m.sessions_all = true;
                        m.sessions.clear();
                        break;
                    }
                    m.sessions.insert(key.clone());
                }
            });
        }
    }

    /// `ProjectEvent::Changed`: worktrees added or removed need a round
    /// (the missing ones are computed, the gone ones dropped); another
    /// current worktree has its count from the overview.
    pub(crate) fn worktrees_project_changed(&self, project: &ProjectKey, parts: ChangedParts) {
        if parts.worktrees || parts.active {
            self.worktrees_mark(project, |m| m.active |= parts.active);
        }
    }

    /// One interest dropped. The last stops the computation and keeps what
    /// it has; a large set of hashes goes after `RETAIN`.
    fn worktrees_release(&self, project: &ProjectKey) {
        let release = {
            let mut states = lock(&self.worktrees);
            let limit = states.hash_limit;
            states.rounds += 1;
            let round = states.rounds;
            let Some(state) = states.projects.get_mut(project) else {
                return;
            };
            state.interests = state.interests.saturating_sub(1);
            if state.interests > 0 {
                return;
            }
            // The running round finds another number and ends; what it
            // had not stored stays marked.
            state.cancel.store(true, Ordering::Release);
            state.round = round;
            state.running = false;
            state.released += 1;
            (state.hash_count() > limit).then_some((state.released, state.ctx))
        };
        let Some((released, ctx)) = release else {
            return;
        };
        let me = self.me.clone();
        let project = project.clone();
        let let_go = move || {
            if let Some(engine) = me.upgrade() {
                engine.submit(Priority::Background, ctx, move |engine| {
                    engine.worktrees_let_go(&project, released)
                });
            }
        };
        match self.cfg.host.clone() {
            Some(host) => host.later(RETAIN, Box::new(let_go)),
            None => let_go(),
        }
    }

    /// The path hashes go, the counts and the overlaps stay: the entries
    /// are compared again when the project is watched again.
    fn worktrees_let_go(&self, project: &ProjectKey, released: u64) {
        let mut states = lock(&self.worktrees);
        let Some(state) = states.projects.get_mut(project) else {
            return;
        };
        if state.interests > 0 || state.released != released {
            return;
        }
        for entry in state.entries.values_mut() {
            if entry.hashes.take().is_some() {
                entry.need = Need::BOTH;
            }
        }
        debug!("worktrees hashes released");
    }

    /// `ProjectEvent::Closed`: the state goes with its watch.
    pub(crate) fn worktrees_closed(&self, project: &ProjectKey) {
        let state = lock(&self.worktrees).projects.remove(project);
        if let Some(state) = state {
            state.cancel.store(true, Ordering::Release);
            let ctx = state.ctx;
            self.submit(Priority::Background, ctx, move |_| drop(state));
        }
    }

    /// The plugin is deactivated.
    pub(in crate::engine) fn worktrees_clear(&self) {
        let states: Vec<WtState> = lock(&self.worktrees)
            .projects
            .drain()
            .map(|(_, s)| s)
            .collect();
        for state in &states {
            state.cancel.store(true, Ordering::Release);
        }
        drop(states);
    }

    pub(super) fn spawn_worktrees_round(&self, project: &ProjectKey, round: u64, ctx: Ctx) {
        let project = project.clone();
        self.submit(Priority::Background, ctx, move |engine| {
            engine.worktrees_round(&project, round);
        });
    }

    /// Asks for a round at `at` (clock milliseconds) when the throttle
    /// lets the worktree be compared again. Without a host there is no
    /// later: the next trigger finds it due.
    pub(super) fn worktrees_wake_at(&self, project: &ProjectKey, at: i64) {
        let Some(host) = self.cfg.host.clone() else {
            return;
        };
        let now = (self.cfg.clock)().0;
        let me = self.me.clone();
        let project = project.clone();
        host.later(
            Duration::from_millis(at.saturating_sub(now).max(0) as u64),
            Box::new(move || {
                if let Some(engine) = me.upgrade() {
                    engine.worktrees_woken(&project, at);
                }
            }),
        );
    }

    fn worktrees_woken(&self, project: &ProjectKey, at: i64) {
        {
            let mut states = lock(&self.worktrees);
            if let Some(state) = states.projects.get_mut(project)
                && state.wake == Some(at)
            {
                state.wake = None;
            }
        }
        self.worktrees_mark(project, |_| {});
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Engine {
    /// Whether any worktrees state exists for the project.
    pub(crate) fn has_worktrees_state(&self, project: &ProjectKey) -> bool {
        lock(&self.worktrees).projects.contains_key(project)
    }

    /// The path hashes held in all (the release after `RETAIN`).
    pub(crate) fn worktrees_hash_count(&self, project: &ProjectKey) -> usize {
        lock(&self.worktrees)
            .projects
            .get(project)
            .map_or(0, WtState::hash_count)
    }

    /// Lets a project's hashes go above this many (tests).
    pub(crate) fn set_worktrees_hash_limit(&self, limit: usize) {
        lock(&self.worktrees).hash_limit = limit;
    }

    /// The marks a repository batch with these changes makes (tests: the
    /// engine has no watcher).
    pub(crate) fn worktrees_batch(&self, project: &ProjectKey, batch: &RepoBatch) {
        sink::mark_batch(self, project, batch);
    }
}
