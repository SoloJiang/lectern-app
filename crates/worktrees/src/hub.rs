//! `Hub`: the state of the "worktrees" plugin and the composing task
//! (docs/design/worktrees.md「数据结构」「数据流与线程」).
//!
//! The listeners (executor threads) only set flags and schedule one composing
//! task; the task reads the services' snapshots (each a copy of an `Arc`),
//! runs [`snapshot::build`] and publishes the result. At most one task runs;
//! events that arrive while it runs set `dirty` and it composes again before it
//! ends. Nothing is composed or watched while the screen is not the current
//! one (`shown`), except that the changes of the current worktree are counted.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use futures::channel::mpsc;
use lectern_executor::{Executor, Priority};
use lectern_kernel::{Name, PluginHandle, ServiceRef, ServiceSet};
use lectern_logging::{debug, info, trace, warn};
use lectern_services::project::{ProjectEvent, ProjectKey, Projects, WorktreeKey};
use lectern_services::review::{OverviewInterest, Review, ReviewEvent};
use lectern_services::session::{
    SessionKey, SessionList, SessionSource, SessionsChanged, SourceId, Timestamp,
};
use lectern_services::usage::UsageQuery;
use lectern_services::workspace::{
    Contribution, ContributionKind, Contributions, Milestone, Registration, Route, SidebarSection,
    Target, WorkspaceCommand, WorkspaceEvent, screens,
};
use lectern_ui::Icon;
use lectern_ui::contrib::SidebarItem;

use crate::diag::count;
use crate::snapshot::{self, Inputs, RowUsage, UsageInputs, WorktreesSnapshot};
use crate::usage_link::UsageLink;

/// The ID of the sidebar entry.
pub(crate) const ENTRY_ID: &str = "worktrees";

/// The time of day, injected so that the tests fix it.
pub(crate) type Wall = Arc<dyn Fn() -> Timestamp + Send + Sync>;

/// What `activate` hands the state.
pub(crate) struct Parts {
    pub executor: Executor,
    pub projects: ServiceRef<dyn Projects>,
    pub sessions: ServiceSet<dyn SessionSource>,
    pub review: ServiceRef<dyn Review>,
    pub contributions: ServiceRef<dyn Contributions>,
    pub usage: UsageLink,
    pub wall: Wall,
}

/// The state of one activation of "worktrees". The views of step 2 hold an
/// `Arc` of it and read [`snapshot`](Hub::snapshot).
pub struct Hub {
    executor: Executor,
    projects: ServiceRef<dyn Projects>,
    sessions: ServiceSet<dyn SessionSource>,
    review: ServiceRef<dyn Review>,
    contributions: ServiceRef<dyn Contributions>,
    usage: UsageLink,
    wall: Wall,
    state: Mutex<State>,
    /// The wake-up channels of the views that are built (「数据结构」). Each
    /// message is the new snapshot.
    views: Mutex<Vec<mpsc::UnboundedSender<Arc<WorktreesSnapshot>>>>,
}

#[derive(Default)]
struct State {
    /// The base project: from `Opened` to `Closed`. A folder project is
    /// recorded too (the snapshot's `folder`).
    project: Option<ProjectKey>,
    /// Counts the projects and closings; a result composed for an older value
    /// is dropped.
    epoch: u64,
    /// The registration of the sidebar entry; only for a git project.
    entry: Option<Registration>,
    /// The Worktrees screen is the current screen.
    shown: bool,
    /// Held while `shown`.
    interest: Option<OverviewInterest>,
    snapshot: Option<Arc<WorktreesSnapshot>>,
    /// Something changed since the task last read the services.
    dirty: bool,
    /// A composing task is queued or running.
    building: bool,
    /// The first composition after each time the screen is shown is logged at
    /// info, later ones at debug.
    built_once: bool,
    /// How many times the current worktree changed, shown or not.
    switches: u64,
    /// `switches` when the screen last became the current one: what the
    /// milestone counts from.
    shown_at: u64,
    /// Sessions `complete` was asked for; cleared when the project closes.
    completed: HashSet<SessionKey>,
    /// From `started`; the callback of the usage link schedules through it.
    handle: Option<PluginHandle>,
}

impl Hub {
    pub(crate) fn new(parts: Parts) -> Arc<Hub> {
        Arc::new(Hub {
            executor: parts.executor,
            projects: parts.projects,
            sessions: parts.sessions,
            review: parts.review,
            contributions: parts.contributions,
            usage: parts.usage,
            wall: parts.wall,
            state: Mutex::new(State::default()),
            views: Mutex::new(Vec::new()),
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The latest snapshot of the base project; None before the first one is
    /// composed, while the screen is not the current one and after the
    /// project closed. Copies an `Arc` under a lock.
    pub fn snapshot(&self) -> Option<Arc<WorktreesSnapshot>> {
        self.lock().snapshot.clone()
    }

    /// How many times the current worktree changed (`ProjectEvent::Changed`
    /// with `parts.active`), also while the screen is not the current one.
    pub fn switches(&self) -> u64 {
        self.lock().switches
    }

    /// The value of [`switches`](Hub::switches) when the screen last became
    /// the current one. A model takes it as what it has seen: the switch that
    /// follows the push counts for the milestone even when it arrives before
    /// `workspace` has built the model (the view is built in the frame after
    /// the push), and a switch from before the push does not
    /// (「数据流与线程」).
    pub fn switches_when_shown(&self) -> u64 {
        self.lock().shown_at
    }

    /// A view that was just built (`view` is `screen` or `detail`) registers
    /// for snapshots. Main thread.
    ///
    /// The sender is registered here and the caller reads `snapshot()`
    /// afterwards, so what is published in between reaches the view as well
    /// and nothing is missed. The channel ends when the activation is gone
    /// (`detach`).
    pub fn attach_view(
        &self,
        view: &'static str,
    ) -> mpsc::UnboundedReceiver<Arc<WorktreesSnapshot>> {
        let (sender, receiver) = mpsc::unbounded();
        self.views
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(sender);
        debug!("worktrees view attached"; view = view);
        receiver
    }

    /// Sends the snapshot to every view; the ones whose receiver is gone (the
    /// view was dropped) are forgotten. Any thread, does not block.
    fn wake(&self, snapshot: &Arc<WorktreesSnapshot>) {
        self.views
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|sender| sender.unbounded_send(snapshot.clone()).is_ok());
    }

    /// How many views are registered (tests).
    #[cfg(test)]
    pub(crate) fn view_count(&self) -> usize {
        self.views
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// A timer of a model asks for a new composition (an Active session ran
    /// out). Ignored while the screen is not the current one. Any thread.
    pub fn request_build(self: &Arc<Self>) {
        let handle = self.lock().handle.clone();
        if let Some(handle) = handle {
            self.schedule(&handle);
        }
    }

    /// The time of day, in the unit of the sessions' timestamps: the views
    /// arm their expiry timer from the same clock the snapshot was composed
    /// with (tests move it).
    pub fn now(&self) -> Timestamp {
        (self.wall)()
    }

    /// `Contributions::dispatch`, logged. The command goes to the window's
    /// queue; nothing waits for it. `target` is the kind of the destination
    /// (`range-base`, `range-head`, `session`, `switch`), never a name or a
    /// path. Main thread.
    pub(crate) fn dispatch(&self, command: WorkspaceCommand, target: &'static str) {
        debug!("worktrees command"; command = command_name(&command), target = target);
        if let Some(contributions) = self.contributions.get() {
            contributions.dispatch(command);
        }
    }

    /// ⌘R and the toolbar's Refresh: `Review::refresh_worktrees` for the
    /// project the snapshot is of. It returns at once. Main thread.
    pub(crate) fn refresh(&self, project: &ProjectKey) {
        debug!("worktrees command"; command = "refresh", target = "refresh");
        if let Some(review) = self.review.get() {
            review.refresh_worktrees(project);
        }
    }

    /// The target's place in `ProjectSnapshot::worktrees`, for the log of a
    /// switch (a copy of an `Arc` under a lock). Main thread.
    pub(crate) fn log_switch(&self, project: &ProjectKey, worktree: &WorktreeKey) {
        let snapshot = self
            .projects
            .get()
            .and_then(|projects| projects.snapshot(project));
        let found = snapshot.as_ref().and_then(|snapshot| {
            snapshot
                .worktrees
                .iter()
                .position(|candidate| candidate.key == *worktree)
                .map(|index| (index, snapshot.worktrees[index].is_main))
        });
        if let Some((index, main)) = found {
            info!("worktree switch requested"; index = count(index), main = main);
        }
    }

    /// The screen drew the first frame of the settled snapshot after a
    /// switch (「性能」): tells `workspace`, which hands it to `app`. Any
    /// thread.
    pub(crate) fn report_milestone(&self, switches: u64) {
        if let Some(contributions) = self.contributions.get() {
            contributions.report_milestone(Milestone::WorktreeSwitched);
        }
        info!("worktrees milestone reported"; switches = switches);
    }

    /// `started`: keeps the handle, lets the usage link schedule, and takes up
    /// a project that is open already. The listeners hear nothing before the
    /// activation is live, so an `Opened` before this is only seen here.
    pub(crate) fn attach(self: &Arc<Self>, handle: PluginHandle) {
        self.lock().handle = Some(handle.clone());
        let weak = Arc::downgrade(self);
        self.usage.set_on_change(Arc::new(move || {
            if let Some(hub) = weak.upgrade() {
                hub.request_build();
            }
        }));
        let open = self
            .projects
            .get()
            .and_then(|projects| projects.open_projects().pop());
        if let Some(key) = open {
            self.open(&key, &handle);
        }
    }

    /// The activation is gone: lets go of the entry and the interests.
    pub(crate) fn detach(&self) {
        let (entry, interest) = {
            let mut state = self.lock();
            state.handle = None;
            state.project = None;
            state.epoch += 1;
            state.dirty = false;
            state.snapshot = None;
            state.completed.clear();
            (state.entry.take(), state.interest.take())
        };
        drop(entry);
        drop(interest);
        self.usage.set_wanted(Arc::from(Vec::new()));
        // The views' channels end; the views keep what they show.
        self.views
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    pub(crate) fn on_project(self: &Arc<Self>, event: &ProjectEvent, handle: &PluginHandle) {
        match event {
            ProjectEvent::Opened {
                result: Ok(key), ..
            } => {
                trace!("worktrees event"; event = "project-opened");
                self.open(key, handle);
            }
            ProjectEvent::Opened { .. } => {}
            ProjectEvent::Changed {
                project,
                generation,
                parts,
            } => {
                trace!("worktrees event"; event = "project-changed", generation = *generation);
                let known = self.lock().project.clone();
                match known {
                    Some(current) if current == *project => {
                        if parts.active {
                            self.lock().switches += 1;
                        }
                        self.schedule(handle);
                    }
                    // The `Opened` fell between `started` and the activation
                    // going live (project design: a successful `Opened` is
                    // followed by at least one `Changed`). A `Changed` that
                    // outlives its `Closed` finds no snapshot and is ignored.
                    None if self.is_open(project) => self.open(project, handle),
                    None | Some(_) => {}
                }
            }
            ProjectEvent::Closed { project } => {
                trace!("worktrees event"; event = "project-closed");
                self.close(project);
            }
        }
    }

    fn is_open(&self, project: &ProjectKey) -> bool {
        self.projects
            .get()
            .is_some_and(|projects| projects.snapshot(project).is_some())
    }

    pub(crate) fn on_sessions(self: &Arc<Self>, event: &SessionsChanged, handle: &PluginHandle) {
        trace!("worktrees event"; event = "sessions-changed", generation = event.generation);
        self.schedule(handle);
    }

    pub(crate) fn on_review(self: &Arc<Self>, event: &ReviewEvent, handle: &PluginHandle) {
        let ReviewEvent::Worktrees {
            project,
            generation,
        } = event
        else {
            return;
        };
        trace!("worktrees event"; event = "worktrees", generation = *generation);
        if self.lock().project.as_ref() == Some(project) {
            self.schedule(handle);
        }
    }

    /// `WorkspaceEvent` (executor thread): the screen became the current one
    /// or stopped being it.
    pub(crate) fn on_workspace(self: &Arc<Self>, event: &WorkspaceEvent, handle: &PluginHandle) {
        let WorkspaceEvent::ScreenChanged { to, .. } = event else {
            return;
        };
        trace!("worktrees event"; event = "screen-changed");
        if to.screen == screens::WORKTREES {
            self.show(handle);
        } else {
            self.hide();
        }
    }

    /// The screen became the current one: takes the interest in the
    /// worktrees' changes and composes. The interests in the usage follow the
    /// first snapshot.
    fn show(self: &Arc<Self>, handle: &PluginHandle) {
        let (project, epoch) = {
            let mut state = self.lock();
            if std::mem::replace(&mut state.shown, true) {
                return;
            }
            state.shown_at = state.switches;
            state.built_once = false;
            (state.project.clone(), state.epoch)
        };
        debug!("worktrees shown"; queries = count(self.usage.watching()));
        if let Some(project) = project {
            self.watch(&project, epoch);
            self.schedule(handle);
        }
    }

    /// The screen is not the current one any more: lets go of both interests
    /// and the snapshot.
    fn hide(&self) {
        let interest = {
            let mut state = self.lock();
            if !std::mem::replace(&mut state.shown, false) {
                return;
            }
            state.dirty = false;
            state.snapshot = None;
            state.interest.take()
        };
        drop(interest);
        let queries = self.usage.watching();
        self.usage.set_wanted(Arc::from(Vec::new()));
        debug!("worktrees hidden"; queries = count(queries));
    }

    /// Takes the interest in the worktrees' changes of a git project, unless
    /// the project or the screen changed meanwhile.
    fn watch(&self, project: &ProjectKey, epoch: u64) {
        if matches!(project, ProjectKey::Folder { .. }) {
            return;
        }
        let interest = self
            .review
            .get()
            .map(|review| review.watch_worktrees(project));
        let surplus = {
            let mut state = self.lock();
            if state.epoch == epoch && state.shown && state.interest.is_none() {
                state.interest = interest;
                None
            } else {
                interest
            }
        };
        drop(surplus);
    }

    /// A project became the base: registers the sidebar entry of a git
    /// project (dropping the old project's first), and composes if the
    /// screen is the current one.
    fn open(self: &Arc<Self>, key: &ProjectKey, handle: &PluginHandle) {
        let (previous, interest, epoch, shown) = {
            let mut state = self.lock();
            if state.project.as_ref() == Some(key) {
                drop(state);
                self.schedule(handle);
                return;
            }
            let previous = state.entry.take();
            let interest = state.interest.take();
            state.project = Some(key.clone());
            state.epoch += 1;
            state.snapshot = None;
            state.built_once = false;
            state.dirty = false;
            state.completed.clear();
            (previous, interest, state.epoch, state.shown)
        };
        drop(previous);
        drop(interest);
        if matches!(key, ProjectKey::Git { .. }) {
            let entry = self.register_entry(handle);
            let mut state = self.lock();
            if state.epoch == epoch {
                state.entry = entry;
            }
        }
        if shown {
            self.watch(key, epoch);
        }
        self.schedule(handle);
    }

    /// The sidebar entry of the `Views` section (「命令与事件」): it only
    /// exists while a git project is open.
    fn register_entry(&self, handle: &PluginHandle) -> Option<Registration> {
        let contributions = self.contributions.get()?;
        let item = SidebarItem {
            label: lectern_i18n::keys::worktrees::ENTRY,
            icon: Some(Icon::Worktrees),
            on_select: WorkspaceCommand::Push(Route {
                screen: Name::Borrowed(screens::WORKTREES),
                target: Target::None,
            }),
        };
        Some(contributions.register(
            Contribution {
                kind: ContributionKind::SidebarEntry {
                    section: SidebarSection::Views,
                },
                id: Name::Borrowed(ENTRY_ID),
                order: 0,
                payload: item.into_payload(),
            },
            Some(handle.token()),
        ))
    }

    /// The base project closed: lets go of the entry, the interests and the
    /// snapshot, and tells the views.
    fn close(&self, key: &ProjectKey) {
        let (entry, interest, switches) = {
            let mut state = self.lock();
            if state.project.as_ref() != Some(key) {
                return;
            }
            state.project = None;
            state.epoch += 1;
            state.dirty = false;
            state.snapshot = None;
            state.built_once = false;
            state.completed.clear();
            (state.entry.take(), state.interest.take(), state.switches)
        };
        drop(entry);
        drop(interest);
        self.usage.set_wanted(Arc::from(Vec::new()));
        // The views clear: they get an empty snapshot.
        self.wake(&Arc::new(WorktreesSnapshot::empty(
            key.clone(),
            (self.wall)(),
            switches,
        )));
    }

    /// Marks the snapshot dirty and, when no task is queued or running,
    /// submits one. Nothing is composed while the screen is not the current
    /// one.
    fn schedule(self: &Arc<Self>, handle: &PluginHandle) {
        {
            let mut state = self.lock();
            if state.project.is_none() || !state.shown {
                return;
            }
            state.dirty = true;
            if state.building {
                return;
            }
            state.building = true;
        }
        let Some(busy) = handle.busy() else {
            self.lock().building = false;
            return;
        };
        let hub = self.clone();
        let executor = self.executor.clone();
        let accepted = handle.own_with(move |gate| {
            executor.run_gated(&gate, Priority::Background, move || {
                let _busy = busy;
                hub.run();
            })
        });
        if !accepted {
            self.lock().building = false;
        }
    }

    /// The task: composes while something is dirty.
    fn run(&self) {
        loop {
            let (project, epoch, switches) = {
                let mut state = self.lock();
                match state.project.clone() {
                    Some(project) if state.dirty && state.shown => {
                        state.dirty = false;
                        (project, state.epoch, state.switches)
                    }
                    _ => {
                        state.building = false;
                        return;
                    }
                }
            };
            let started = Instant::now();
            match self.compose(&project, switches) {
                Some(built) => self.publish(&project, epoch, built, started),
                None => warn!("worktrees build dropped"; reason = "project-closed"),
            }
        }
    }

    /// Reads the services and builds. None when the project is gone.
    fn compose(&self, project: &ProjectKey, switches: u64) -> Option<snapshot::Built> {
        let snapshot = self.projects.get()?.snapshot(project)?;
        let lists: Vec<(SourceId, Arc<SessionList>)> = self
            .sessions
            .current()
            .iter()
            .map(|source| (source.id(), source.sessions()))
            .collect();
        let changes = self
            .review
            .get()
            .and_then(|review| review.worktrees(project));
        let usage = self.read_usage(&snapshot);
        Some(snapshot::build(&Inputs {
            project: &snapshot,
            sessions: &lists,
            changes: changes.as_deref(),
            usage: usage.as_ref(),
            switches,
            usage_generation: self.usage.generation(),
            now: (self.wall)(),
        }))
    }

    /// The usage snapshots the composition needs: None when there is no
    /// `dyn Usage`. Each of the worktrees that are not prunable, then each MR
    /// of a worktree whose branch is in more than one.
    fn read_usage(
        &self,
        project: &lectern_services::project::ProjectSnapshot,
    ) -> Option<UsageInputs> {
        let (service, _admission) = self.usage.usage()?;
        let usage = service.get()?;
        let mut inputs = UsageInputs::default();
        for worktree in project.worktrees.iter().filter(|w| !w.prunable) {
            let query = UsageQuery::Worktree {
                project: project.key.clone(),
                worktree: worktree.key.clone(),
            };
            if let Some(snapshot) = usage.snapshot(&query) {
                inputs.worktrees.insert(worktree.key.clone(), snapshot);
            }
        }
        let mrs: Vec<_> = inputs
            .worktrees
            .values()
            .filter_map(|snapshot| match &**snapshot {
                lectern_services::usage::UsageSnapshot::Worktree(w) if w.mrs.len() > 1 => {
                    Some(w.mrs.iter().map(|label| label.mr.clone()))
                }
                _ => None,
            })
            .flatten()
            .collect();
        for mr in mrs {
            if let Some(snapshot) = usage.snapshot(&UsageQuery::Mr { mr: mr.clone() }) {
                inputs.mrs.insert(mr, snapshot);
            }
        }
        Some(inputs)
    }

    /// Stores the snapshot unless the project or the screen changed
    /// meanwhile, sends it to the views, hands the usage link the queries it
    /// needs, and asks the sources to complete the sessions shown with
    /// pending tails.
    fn publish(&self, project: &ProjectKey, epoch: u64, built: snapshot::Built, started: Instant) {
        let snapshot::Built {
            snapshot,
            pending,
            queries,
        } = built;
        let snapshot = Arc::new(snapshot);
        let (first, to_complete) = {
            let mut state = self.lock();
            if state.project.as_ref() != Some(project) {
                drop(state);
                warn!("worktrees build dropped"; reason = "project-closed");
                return;
            }
            if state.epoch != epoch {
                drop(state);
                warn!("worktrees build dropped"; reason = "generation-stale");
                return;
            }
            if !state.shown {
                drop(state);
                warn!("worktrees build dropped"; reason = "hidden");
                return;
            }
            state.snapshot = Some(snapshot.clone());
            let first = !std::mem::replace(&mut state.built_once, true);
            let to_complete: Vec<SessionKey> = pending
                .into_iter()
                .filter(|key| state.completed.insert(key.clone()))
                .collect();
            (first, to_complete)
        };

        self.wake(&snapshot);
        self.usage.set_wanted(queries);
        // The screen may have been left between the check and the call: the
        // interests must not outlive it.
        if !self.lock().shown {
            self.usage.set_wanted(Arc::from(Vec::new()));
        }
        let took = started.elapsed();
        let rows = snapshot.active_rows.iter().chain(snapshot.idle_rows.iter());
        let worktrees = count(rows.clone().count());
        let active = count(snapshot.active_rows.len());
        let sessions = count(rows.clone().map(|row| row.sessions as usize).sum());
        let changes = snapshot.sources.changes.is_some();
        let usage = rows
            .clone()
            .any(|row| !matches!(row.usage, RowUsage::Absent));
        if first {
            info!(
                "worktrees snapshot built";
                worktrees = worktrees, active = active, sessions = sessions,
                changes = changes, usage = usage, took_us = took,
            );
        } else {
            debug!(
                "worktrees snapshot built";
                worktrees = worktrees, active = active, sessions = sessions,
                changes = changes, usage = usage, took_us = took,
                project_gen = snapshot.sources.project,
                changes_gen = snapshot.sources.changes,
                usage_gen = snapshot.sources.usage,
            );
        }
        if !to_complete.is_empty() {
            self.complete(&to_complete);
        }
    }

    /// `SessionSource::complete` for the sessions, grouped by source.
    fn complete(&self, keys: &[SessionKey]) {
        for source in self.sessions.current() {
            let id = source.id();
            let mine: Vec<SessionKey> = keys
                .iter()
                .filter(|key| key.source == id)
                .cloned()
                .collect();
            if !mine.is_empty() {
                source.complete(&mine);
            }
        }
        debug!("worktrees complete requested"; sessions = count(keys.len()));
    }
}

/// The kind of a command, for the log (never the route's target).
fn command_name(command: &WorkspaceCommand) -> &'static str {
    match command {
        WorkspaceCommand::Push(_) => "push",
        WorkspaceCommand::SetActiveWorktree(_) => "set-active-worktree",
        _ => "other",
    }
}
