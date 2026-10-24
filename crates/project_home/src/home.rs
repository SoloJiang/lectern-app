//! `Home`: the state of the "project-home" plugin and the composing task
//! (docs/design/project_home.md「数据结构」「数据流与线程」).
//!
//! The listeners (executor threads) only set flags and schedule one composing
//! task; the task reads the services' snapshots (each a copy of an `Arc`),
//! runs [`snapshot::build`] and publishes the result. At most one task runs;
//! events that arrive while it runs set `dirty` and it composes again before
//! it ends.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use lectern_executor::{Executor, Priority};
use lectern_kernel::{PluginHandle, ServiceRef, ServiceSet};
use lectern_logging::{debug, info, trace, warn};
use lectern_services::project::{ProjectEvent, ProjectKey, Projects};
use lectern_services::review::{OverviewInterest, Review, ReviewEvent};
use lectern_services::session::{
    SessionKey, SessionList, SessionSource, SessionsChanged, SourceId, Timestamp,
};
use lectern_services::usage::{ProjectRef, UsageQuery, Window};

use crate::diag::{count, session};
use crate::snapshot::{self, HomeSnapshot, Inputs, UsageInputs};
use crate::usage_slot::UsageSlot;

/// The time of day, injected so that the tests fix it.
pub(crate) type Wall = Arc<dyn Fn() -> Timestamp + Send + Sync>;

/// What `activate` hands the state.
pub(crate) struct Parts {
    pub executor: Executor,
    pub projects: ServiceRef<dyn Projects>,
    pub sessions: ServiceSet<dyn SessionSource>,
    pub review: ServiceRef<dyn Review>,
    pub usage: UsageSlot,
    pub wall: Wall,
}

/// The state of one activation of "project-home". The views of step 2 hold an
/// `Arc` of it and read [`snapshot`](Home::snapshot).
pub struct Home {
    executor: Executor,
    projects: ServiceRef<dyn Projects>,
    sessions: ServiceSet<dyn SessionSource>,
    review: ServiceRef<dyn Review>,
    usage: UsageSlot,
    wall: Wall,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// The base project: from `Opened` to `Closed`.
    project: Option<ProjectKey>,
    /// Counts the projects and closings; a result composed for an older value
    /// is dropped.
    epoch: u64,
    /// Held from `Opened` to `Closed`, not for as long as the screen shows:
    /// dropping it on every screen change would redo the overview's first
    /// round (docs/design/project_home.md「数据流与线程」).
    interest: Option<OverviewInterest>,
    snapshot: Option<Arc<HomeSnapshot>>,
    /// Something changed since the task last read the services.
    dirty: bool,
    /// A composing task is queued or running.
    building: bool,
    /// The first composition of a project is logged at info, later ones at debug.
    built_once: bool,
    /// Sessions `complete` was asked for; cleared when the project closes.
    completed: HashSet<SessionKey>,
    /// Overview sessions a warning was written for.
    warned: HashSet<SessionKey>,
    /// From `started`; the callback of the usage slot schedules through it.
    handle: Option<PluginHandle>,
}

impl Home {
    pub(crate) fn new(parts: Parts) -> Arc<Home> {
        Arc::new(Home {
            executor: parts.executor,
            projects: parts.projects,
            sessions: parts.sessions,
            review: parts.review,
            usage: parts.usage,
            wall: parts.wall,
            state: Mutex::new(State::default()),
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The latest snapshot of the base project; None before the first one is
    /// composed and after the project closed. Copies an `Arc` under a lock.
    pub fn snapshot(&self) -> Option<Arc<HomeSnapshot>> {
        self.lock().snapshot.clone()
    }

    /// `started`: keeps the handle, lets the usage slot schedule, and takes up
    /// a project that is open already. The listeners hear nothing before the
    /// activation is live, so an `Opened` before this is only seen here.
    pub(crate) fn attach(self: &Arc<Self>, handle: PluginHandle) {
        self.lock().handle = Some(handle.clone());
        let weak = Arc::downgrade(self);
        self.usage.set_on_change(Arc::new(move || {
            if let Some(home) = weak.upgrade() {
                home.request_rebuild();
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

    /// The activation is gone: lets go of the interests.
    pub(crate) fn detach(&self) {
        let interest = {
            let mut state = self.lock();
            state.handle = None;
            state.project = None;
            state.epoch += 1;
            state.snapshot = None;
            state.interest.take()
        };
        drop(interest);
        self.usage.set_project(None);
    }

    pub(crate) fn on_project(self: &Arc<Self>, event: &ProjectEvent, handle: &PluginHandle) {
        match event {
            ProjectEvent::Opened {
                result: Ok(key), ..
            } => {
                trace!("home event"; event = "project-opened");
                self.open(key, handle);
            }
            ProjectEvent::Opened { .. } => {}
            ProjectEvent::Changed {
                project,
                generation,
                ..
            } => {
                trace!("home event"; event = "project-changed", generation = *generation);
                let known = self.lock().project.clone();
                match known {
                    Some(current) if current == *project => self.schedule(handle),
                    // The `Opened` fell between `started` and the activation
                    // going live (project design: a successful `Opened` is
                    // followed by at least one `Changed`). A `Changed` that
                    // outlives its `Closed` finds no snapshot and is ignored.
                    None if self.is_open(project) => self.open(project, handle),
                    None => {}
                    Some(_) => {}
                }
            }
            ProjectEvent::Closed { project } => {
                trace!("home event"; event = "project-closed");
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
        trace!("home event"; event = "sessions-changed", generation = event.generation);
        self.schedule(handle);
    }

    pub(crate) fn on_review(self: &Arc<Self>, event: &ReviewEvent, handle: &PluginHandle) {
        let ReviewEvent::Overview {
            project,
            generation,
        } = event
        else {
            return;
        };
        trace!("home event"; event = "overview", generation = *generation);
        if self.lock().project.as_ref() == Some(project) {
            self.schedule(handle);
        }
    }

    /// A project became the base: takes the overview interest (dropping the
    /// old project's first) and the usage interest, and composes.
    fn open(self: &Arc<Self>, key: &ProjectKey, handle: &PluginHandle) {
        let (previous, epoch) = {
            let mut state = self.lock();
            if state.project.as_ref() == Some(key) {
                drop(state);
                self.schedule(handle);
                return;
            }
            let previous = state.interest.take();
            state.project = Some(key.clone());
            state.epoch += 1;
            state.snapshot = None;
            state.built_once = false;
            state.completed.clear();
            state.warned.clear();
            (previous, state.epoch)
        };
        drop(previous);
        let interest = self.review.get().map(|review| review.watch_overview(key));
        let surplus = {
            let mut state = self.lock();
            if state.epoch == epoch {
                state.interest = interest;
                None
            } else {
                interest
            }
        };
        drop(surplus);
        self.usage.set_project(Some(key.clone()));
        self.schedule(handle);
    }

    /// The base project closed: lets go of both interests and the snapshot.
    fn close(&self, key: &ProjectKey) {
        let interest = {
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
            state.warned.clear();
            state.interest.take()
        };
        drop(interest);
        self.usage.set_project(None);
    }

    /// The usage slot's callback: through the handle `started` kept.
    fn request_rebuild(self: &Arc<Self>) {
        let handle = self.lock().handle.clone();
        if let Some(handle) = handle {
            self.schedule(&handle);
        }
    }

    /// Marks the snapshot dirty and, when no task is queued or running, submits
    /// one.
    fn schedule(self: &Arc<Self>, handle: &PluginHandle) {
        {
            let mut state = self.lock();
            if state.project.is_none() {
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
        let home = self.clone();
        let executor = self.executor.clone();
        let accepted = handle.own_with(move |gate| {
            executor.run_gated(&gate, Priority::Background, move || {
                let _busy = busy;
                home.run();
            })
        });
        if !accepted {
            self.lock().building = false;
        }
    }

    /// The task: composes while something is dirty.
    fn run(&self) {
        loop {
            let (project, epoch) = {
                let mut state = self.lock();
                match state.project.clone() {
                    Some(project) if state.dirty => {
                        state.dirty = false;
                        (project, state.epoch)
                    }
                    _ => {
                        state.building = false;
                        return;
                    }
                }
            };
            let started = Instant::now();
            match self.compose(&project) {
                Some(built) => self.publish(&project, epoch, built, started),
                None => warn!("home build dropped"; reason = "project-closed", dropped = 1u32),
            }
        }
    }

    /// Reads the services and builds. None when the project is gone.
    fn compose(&self, project: &ProjectKey) -> Option<snapshot::Built> {
        let snapshot = self.projects.get()?.snapshot(project)?;
        let lists: Vec<(SourceId, Arc<SessionList>)> = self
            .sessions
            .current()
            .iter()
            .map(|source| (source.id(), source.sessions()))
            .collect();
        let overview = self
            .review
            .get()
            .and_then(|review| review.overview(project));
        let usage = self.usage.usage().and_then(|(service, _admission)| {
            let usage = service.get()?;
            Some(UsageInputs {
                snapshot: usage.snapshot(&UsageQuery::Project {
                    project: ProjectRef::Open(project.clone()),
                    window: Window::D7,
                }),
                limits: usage.limits(),
            })
        });
        Some(snapshot::build(&Inputs {
            project: &snapshot,
            sessions: &lists,
            overview: overview.as_deref(),
            // `Overview.packages` is not there yet (project_home.md 前置条件 A).
            changed_today: None,
            usage: usage.as_ref(),
            now: (self.wall)(),
        }))
    }

    /// Stores the snapshot unless the project changed meanwhile, then asks the
    /// sources to complete the sessions shown with pending tails.
    fn publish(&self, project: &ProjectKey, epoch: u64, built: snapshot::Built, started: Instant) {
        let snapshot::Built {
            snapshot,
            missing,
            pending,
        } = built;
        let snapshot = Arc::new(snapshot);
        let (first, to_complete, to_warn) = {
            let mut state = self.lock();
            if state.epoch != epoch || state.project.as_ref() != Some(project) {
                drop(state);
                warn!("home build dropped"; reason = "generation-stale", dropped = 1u32);
                return;
            }
            state.snapshot = Some(snapshot.clone());
            let first = !std::mem::replace(&mut state.built_once, true);
            let to_complete: Vec<SessionKey> = pending
                .into_iter()
                .filter(|key| state.completed.insert(key.clone()))
                .collect();
            let to_warn: Vec<SessionKey> = missing
                .into_iter()
                .filter(|key| state.warned.insert(key.clone()))
                .collect();
            (first, to_complete, to_warn)
        };

        for key in &to_warn {
            warn!("overview session not in list"; sid = session(key));
        }
        let took = started.elapsed();
        let sessions = snapshot.sidebar.session_rows;
        let worktrees = count(snapshot.sidebar.groups.len());
        let cards = count(snapshot.main.needs_review.len());
        let done = count(snapshot.main.done.len());
        let packages = count(snapshot.sidebar.packages.len());
        let overview = snapshot.main.overview_ready;
        if first {
            info!(
                "home snapshot built";
                sessions = sessions, worktrees = worktrees, cards = cards, done = done,
                packages = packages, overview = overview, took_us = took,
            );
        } else {
            debug!(
                "home snapshot built";
                sessions = sessions, worktrees = worktrees, cards = cards, done = done,
                packages = packages, overview = overview, took_us = took,
                project_gen = snapshot.sources.project, overview_gen = snapshot.sources.overview,
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
        debug!("home complete requested"; sessions = count(keys.len()));
    }
}
