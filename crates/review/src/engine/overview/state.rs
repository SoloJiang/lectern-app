//! The overview states, their interests, the triggers and the two-second
//! merge window. Every entry point here only takes the `overviews` lock (a
//! leaf: nothing else is taken while it is held) and submits tasks.

use super::*;

/// What a round is asked to do; triggers merge into one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Trigger {
    /// Compare everything again (HEAD, the index, a rescan).
    pub full: bool,
    /// Compare these paths again.
    pub paths: Vec<ChangePath>,
    /// The project's worktrees changed: find the current one again.
    pub resolve: bool,
}

impl Trigger {
    pub(crate) fn full() -> Trigger {
        Trigger {
            full: true,
            ..Trigger::default()
        }
    }

    pub(crate) fn resolve() -> Trigger {
        Trigger {
            full: true,
            resolve: true,
            paths: Vec::new(),
        }
    }

    pub(super) fn merge(&mut self, other: Trigger) {
        self.full |= other.full;
        self.resolve |= other.resolve;
        if self.full {
            self.paths.clear();
            return;
        }
        for p in other.paths {
            if !self.paths.contains(&p) {
                self.paths.push(p);
            }
        }
    }

    pub(super) fn how(&self) -> &'static str {
        if self.resolve {
            "resolve"
        } else if self.full {
            "full"
        } else if !self.paths.is_empty() {
            "paths"
        } else {
            "reuse"
        }
    }
}

/// The overviews with an interest, by project.
#[derive(Default)]
pub(crate) struct Overviews {
    pub(super) projects: HashMap<ProjectKey, OverviewState>,
    /// Round numbers across every state: a round queued for a state that
    /// was released and watched again does not match the new state.
    rounds: u64,
}

/// One project's overview and what the next round reuses.
pub(super) struct OverviewState {
    pub interests: u64,
    pub generation: u64,
    pub snapshot: Option<Arc<Overview>>,
    /// The current worktree's handle, once a round resolved it.
    pub git: Option<Arc<Git>>,
    /// The count change set; None before the first round and when capped.
    pub count: Option<Arc<ChangeSet>>,
    /// The window's sessions with their derived claims (「概览」 step 2).
    pub sessions: Vec<Known>,
    pub watch: Option<Watching>,
    pub running: bool,
    /// The merge window's timer is set.
    pub armed: bool,
    pub again: Option<Trigger>,
    pub round: u64,
    pub cancel: Arc<AtomicBool>,
    pub ctx: Ctx,
}

impl OverviewState {
    fn new() -> OverviewState {
        OverviewState {
            interests: 0,
            generation: 0,
            snapshot: None,
            git: None,
            count: None,
            sessions: Vec::new(),
            watch: None,
            running: false,
            armed: false,
            again: None,
            round: 0,
            cancel: Arc::new(AtomicBool::new(false)),
            ctx: Ctx::default(),
        }
    }
}

/// A session of the window: its key, when it started and its claims. Not
/// the summary: the budget is under 1 MB for a hundred of them.
pub(super) struct Known {
    pub key: SessionKey,
    pub started_at: Timestamp,
    pub claims: Arc<Claims>,
}

/// The overview's two subscriptions. Dropping it unsubscribes.
pub(super) struct Watching {
    pub _repo: RepoWatch,
    pub _tree: WorktreeWatch,
}

impl Engine {
    /// `Review::overview`: the snapshot while an interest is held.
    pub(crate) fn overview(&self, project: &ProjectKey) -> Option<Arc<Overview>> {
        lock(&self.overviews)
            .projects
            .get(project)
            .filter(|s| s.interests > 0)
            .and_then(|s| s.snapshot.clone())
    }

    /// `Review::watch_overview`: the first interest starts a round at
    /// once; the returned value's drop releases one interest.
    pub(crate) fn watch_overview(&self, project: &ProjectKey) -> OverviewInterest {
        let first = {
            let mut overviews = lock(&self.overviews);
            let state = overviews
                .projects
                .entry(project.clone())
                .or_insert_with(OverviewState::new);
            state.interests += 1;
            state.interests == 1
        };
        if first {
            self.start_overview_round(project, Trigger::resolve());
        }
        let me = self.me.clone();
        let project = project.clone();
        OverviewInterest::new(move || {
            if let Some(engine) = me.upgrade() {
                engine.overview_release(&project);
            }
        })
    }

    /// One interest dropped. The last one releases the state on a
    /// `Background` task: the count set, the claims and the watches are
    /// not dropped on the caller's thread.
    fn overview_release(&self, project: &ProjectKey) {
        let released = {
            let mut overviews = lock(&self.overviews);
            let Some(state) = overviews.projects.get_mut(project) else {
                return;
            };
            state.interests = state.interests.saturating_sub(1);
            if state.interests > 0 {
                return;
            }
            overviews.projects.remove(project)
        };
        if let Some(state) = released {
            self.drop_overview_state(state);
        }
    }

    /// `ProjectEvent::Closed`: the state goes whatever the interests; a
    /// later release finds nothing.
    pub(crate) fn project_closed(&self, project: &ProjectKey) {
        let released = lock(&self.overviews).projects.remove(project);
        if let Some(state) = released {
            self.drop_overview_state(state);
        }
    }

    fn drop_overview_state(&self, state: OverviewState) {
        state.cancel.store(true, Ordering::Release);
        let ctx = state.ctx;
        debug!("overview released");
        self.submit(Priority::Background, ctx, move |_| drop(state));
    }

    /// `ProjectEvent::Changed`: with another active worktree the current
    /// one is found again; with new sub-projects the counts per
    /// sub-project are made again from the count set; other changes ask
    /// for nothing (the watches see the files).
    pub(crate) fn project_changed(&self, project: &ProjectKey, parts: ChangedParts) {
        if parts.active {
            self.overview_request(project, Trigger::resolve());
        } else if parts.packages {
            self.overview_request(project, Trigger::default());
        }
    }

    /// Asks for a round. Runs after the merge window, or after the
    /// running round and then the window; without a host (`Driven`), as
    /// soon as the executor runs.
    pub(crate) fn overview_request(&self, project: &ProjectKey, trigger: Trigger) {
        let arm = {
            let mut overviews = lock(&self.overviews);
            let Some(state) = overviews.projects.get_mut(project) else {
                return;
            };
            if state.interests == 0 {
                return;
            }
            debug!("overview requested"; how = trigger.how());
            match &mut state.again {
                Some(again) => again.merge(trigger),
                None => state.again = Some(trigger),
            }
            if state.running || state.armed {
                false
            } else {
                state.armed = true;
                true
            }
        };
        if arm {
            self.arm_overview(project);
        }
    }

    /// Every overview: a `SessionsChanged`.
    pub(crate) fn overviews_request_all(&self, trigger: Trigger) {
        let projects: Vec<ProjectKey> = lock(&self.overviews).projects.keys().cloned().collect();
        for project in projects {
            self.overview_request(&project, trigger.clone());
        }
    }

    /// The overviews of a repository: its annotation log was read or
    /// written, so the progress may have changed.
    pub(crate) fn overviews_for_repo(&self, common_dir: &Path, trigger: Trigger) {
        let projects: Vec<ProjectKey> = lock(&self.overviews)
            .projects
            .iter()
            .filter(|(_, s)| s.git.as_ref().is_some_and(|g| g.common_dir() == common_dir))
            .map(|(p, _)| p.clone())
            .collect();
        for project in projects {
            self.overview_request(&project, trigger.clone());
        }
    }

    /// Starts the merge window: `overview_fire` when it ends.
    fn arm_overview(&self, project: &ProjectKey) {
        let me = self.me.clone();
        let project = project.clone();
        let fire = move || {
            if let Some(engine) = me.upgrade() {
                engine.overview_fire(&project);
            }
        };
        match self.cfg.host.clone() {
            Some(host) => host.later(MERGE_WINDOW, Box::new(fire)),
            None => fire(),
        }
    }

    /// The merge window ended: the merged trigger starts a round unless
    /// one is running (then the round's end arms the window again).
    fn overview_fire(&self, project: &ProjectKey) {
        let trigger = {
            let mut overviews = lock(&self.overviews);
            let Some(state) = overviews.projects.get_mut(project) else {
                return;
            };
            state.armed = false;
            if state.running {
                return;
            }
            state.again.take()
        };
        if let Some(trigger) = trigger {
            self.start_overview_round(project, trigger);
        }
    }

    /// Submits a round (`Background`); cancels the one running.
    pub(super) fn start_overview_round(&self, project: &ProjectKey, trigger: Trigger) {
        let (seq, ctx) = {
            let mut overviews = lock(&self.overviews);
            if !overviews.projects.contains_key(project) {
                return;
            }
            overviews.rounds += 1;
            let round = overviews.rounds;
            let Some(state) = overviews.projects.get_mut(project) else {
                return;
            };
            state.running = true;
            state.round = round;
            state.cancel.store(true, Ordering::Release);
            state.cancel = Arc::new(AtomicBool::new(false));
            (state.round, state.ctx)
        };
        let project = project.clone();
        self.submit(Priority::Background, ctx, move |engine| {
            engine.overview_round(&project, seq, trigger);
        });
    }

    /// A round ended: the next one after the window when a trigger waits.
    pub(super) fn overview_round_ended(&self, project: &ProjectKey) {
        let arm = {
            let mut overviews = lock(&self.overviews);
            let Some(state) = overviews.projects.get_mut(project) else {
                return;
            };
            state.running = false;
            if state.again.is_some() && !state.armed {
                state.armed = true;
                true
            } else {
                false
            }
        };
        if arm {
            self.arm_overview(project);
        }
    }

    /// The worktrees and sessions the overviews hold, which `release`
    /// keeps in the engine's tables.
    pub(in crate::engine) fn overview_retain(&self) -> (HashSet<WorktreeKey>, HashSet<SessionKey>) {
        let overviews = lock(&self.overviews);
        let worktrees = overviews
            .projects
            .values()
            .filter_map(|s| s.git.as_ref().map(|g| g.key.clone()))
            .collect();
        let sessions = overviews
            .projects
            .values()
            .flat_map(|s| s.sessions.iter().map(|k| k.key.clone()))
            .collect();
        (worktrees, sessions)
    }

    /// The plugin is deactivated.
    pub(in crate::engine) fn overviews_clear(&self) {
        let states: Vec<OverviewState> = lock(&self.overviews)
            .projects
            .drain()
            .map(|(_, s)| s)
            .collect();
        for state in &states {
            state.cancel.store(true, Ordering::Release);
        }
        drop(states);
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Engine {
    /// The count change set an overview holds (tests: the limits, the
    /// release).
    pub(crate) fn overview_count(&self, project: &ProjectKey) -> Option<Arc<ChangeSet>> {
        lock(&self.overviews)
            .projects
            .get(project)
            .and_then(|s| s.count.clone())
    }

    /// Whether any overview state exists for the project.
    pub(crate) fn has_overview_state(&self, project: &ProjectKey) -> bool {
        lock(&self.overviews).projects.contains_key(project)
    }
}
