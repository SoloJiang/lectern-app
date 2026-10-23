//! The "review" plugin (docs/design/review.md「依赖」「数据流与线程」):
//! provides `dyn Review`, requires `dyn Projects` and `dyn SessionSource`,
//! activates after the first frame.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Wake, Waker};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lectern_cache::Cache;
use lectern_executor::{Executor, Priority};
use lectern_fs_watch::Watcher;
use lectern_kernel::{
    Activation, ActivationToken, Active, BusyGuard, Clock, Context, Manifest, Name, Plugin,
    PluginError, PluginHandle, SystemClock, service,
};
use lectern_services::project::{ProjectKey, Projects};
use lectern_services::review::{
    BriefRules, BriefRulesRegistration, FileRows, Overview, OverviewInterest, Review,
    ReviewCommand, ReviewEvent, ReviewId, ReviewSnapshot, ReviewTarget, RowsKey, TextKey,
};
use lectern_services::session::{SessionSource, SessionsChanged, Timestamp};

use crate::cutoff::INTERACTIVE_BUDGET;
use crate::engine::{Config, Discover, Engine, Held, Host};

#[cfg(test)]
mod tests;

/// The plugin ID.
pub const PLUGIN_ID: &str = "review";

/// What `app` hands the plugin. The process-wide resources are created once
/// by `app` (ADR 0025); building this does no IO.
pub struct ReviewDeps {
    pub executor: Executor,
    pub cache: Cache,
    pub watcher: Watcher,
    /// The annotation logs' directory: `app` takes `review/` under the
    /// application data directory `platform` gives (ADR 0044). Created by
    /// the first write.
    pub data_dir: PathBuf,
    /// `GIT_CEILING_DIRECTORIES`, read once by `app` (as `project` does).
    pub git_ceilings: Arc<[PathBuf]>,
}

/// The plugin and its ID, for `PluginSet::with`.
pub fn plugin(deps: ReviewDeps) -> (Name, ReviewPlugin) {
    (
        Name::Borrowed(PLUGIN_ID),
        ReviewPlugin {
            deps,
            clock: Arc::new(SystemClock),
            wall: Arc::new(wall_clock),
            discover: Arc::new(lectern_git::Repo::discover),
            #[cfg(test)]
            provided: Arc::default(),
        },
    )
}

pub struct ReviewPlugin {
    deps: ReviewDeps,
    /// The timer of the 30-second retention.
    clock: Arc<dyn Clock>,
    /// The time of day the 14-day window of the sessions counts from.
    wall: Arc<dyn Fn() -> Timestamp + Send + Sync>,
    discover: Discover,
    /// The engine and the service of the latest activation, for the tests.
    #[cfg(test)]
    pub(crate) provided: Provided,
}

/// The engine and the service an activation provided (tests).
#[cfg(test)]
pub(crate) type Provided = Arc<Mutex<Option<(Arc<Engine>, Arc<dyn Review>)>>>;

#[cfg(any(test, feature = "test-support"))]
impl ReviewPlugin {
    /// Replaces the retention's timer (tests: `kernel::testing::ManualClock`).
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> ReviewPlugin {
        self.clock = clock;
        self
    }

    /// Replaces the time of day (tests and benchmarks with sessions of a
    /// fixed date).
    pub fn with_wall_clock(
        mut self,
        wall: impl Fn() -> Timestamp + Send + Sync + 'static,
    ) -> ReviewPlugin {
        self.wall = Arc::new(wall);
        self
    }

    /// Discovers repositories reading only their own configuration, so that
    /// the machine's git configuration can't change a test or a benchmark.
    pub fn with_repository_config_only(mut self) -> ReviewPlugin {
        self.discover = Arc::new(lectern_git::testing::discover);
        self
    }
}

impl Plugin for ReviewPlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: vec![service::<dyn Review>()],
            requires: vec![service::<dyn Projects>(), service::<dyn SessionSource>()],
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    /// Builds the empty tables and takes the service references: no IO, no
    /// task, no watch.
    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        let projects = ctx.service::<dyn Projects>();
        let sources = ctx.service_set::<dyn SessionSource>();
        let host = Arc::new(KernelHost::new(
            self.deps.executor.clone(),
            self.clock.clone(),
        ));
        let emitter = host.clone();
        let engine = Engine::new(Config {
            executor: self.deps.executor.clone(),
            cache: self.deps.cache.clone(),
            data_dir: self.deps.data_dir.clone(),
            git_ceilings: self.deps.git_ceilings.clone(),
            projects: Arc::new(move || projects.get().map(Held::new)),
            sources: Arc::new(move || sources.current().into_iter().map(Held::new).collect()),
            host: Some(host.clone()),
            watcher: Some(self.deps.watcher.clone()),
            emit: Arc::new(move |event| emitter.emit(event)),
            clock: self.wall.clone(),
            discover: self.discover.clone(),
            interactive_budget: INTERACTIVE_BUDGET,
        });
        let review: Arc<dyn Review> = Arc::new(Service {
            engine: engine.clone(),
        });
        #[cfg(test)]
        {
            *self.provided.lock().unwrap() = Some((engine.clone(), review.clone()));
        }
        ctx.provide::<dyn Review>(review)?;
        let listener = engine.clone();
        ctx.on::<SessionsChanged>(move |event, _| listener.sessions_changed(event));
        Ok(Box::new(ReviewActive { engine, host }))
    }
}

fn wall_clock() -> Timestamp {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    Timestamp(i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
}

struct ReviewActive {
    engine: Arc<Engine>,
    host: Arc<KernelHost>,
}

impl Active for ReviewActive {
    /// Lets the tasks asked for since `activate` start; submits nothing of
    /// its own.
    fn started(&mut self, handle: PluginHandle) {
        self.host.attach(handle);
    }
}

impl Drop for ReviewActive {
    fn drop(&mut self) {
        self.host.stop();
        self.engine.shutdown();
    }
}

/// The `dyn Review` of the plugin.
struct Service {
    engine: Arc<Engine>,
}

impl Review for Service {
    fn open(&self, target: ReviewTarget) -> ReviewId {
        self.engine.open(target)
    }

    fn snapshot(&self, id: ReviewId) -> Option<Arc<ReviewSnapshot>> {
        self.engine.snapshot(id)
    }

    fn command(&self, id: ReviewId, command: ReviewCommand) {
        self.engine.command(id, command)
    }

    fn rows(&self, id: ReviewId, key: &RowsKey) -> Option<Arc<FileRows>> {
        self.engine.rows(id, key)
    }

    fn text(&self, id: ReviewId, key: &TextKey) -> Option<Arc<str>> {
        self.engine.text(id, key)
    }

    fn close(&self, id: ReviewId) {
        self.engine.close(id)
    }

    /// The overview comes with step 5: no project has one.
    fn overview(&self, _: &ProjectKey) -> Option<Arc<Overview>> {
        None
    }

    /// Step 5: nothing is computed for the interest, so nothing to release.
    fn watch_overview(&self, _: &ProjectKey) -> OverviewInterest {
        OverviewInterest::new(|| {})
    }

    fn register_brief_rules(
        &self,
        provider: Arc<dyn BriefRules>,
        token: ActivationToken,
    ) -> BriefRulesRegistration {
        self.engine.register_brief_rules(provider, token)
    }
}

/// The engine's link to the kernel: tasks through `own_with` holding a
/// `BusyGuard` (kernel「静止」), events through the plugin's emitter, and the
/// retention's timer. The `PluginHandle` only exists from `started` on; what
/// is asked for before is queued and starts then.
struct KernelHost {
    executor: Executor,
    clock: Arc<dyn Clock>,
    state: Mutex<HostState>,
}

type Work = Box<dyn FnOnce() + Send>;

enum HostState {
    Waiting(Vec<(Priority, Work)>),
    Attached(PluginHandle),
    Stopped,
}

impl KernelHost {
    fn new(executor: Executor, clock: Arc<dyn Clock>) -> KernelHost {
        KernelHost {
            executor,
            clock,
            state: Mutex::new(HostState::Waiting(Vec::new())),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, HostState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn attach(&self, handle: PluginHandle) {
        // One statement, so the guard is gone before the match below.
        let previous = std::mem::replace(&mut *self.state(), HostState::Attached(handle.clone()));
        let queued = match previous {
            HostState::Waiting(queued) => queued,
            // Never attached twice; keep what was.
            other => {
                *self.state() = other;
                return;
            }
        };
        for (priority, work) in queued {
            self.run(&handle, priority, work);
        }
    }

    fn stop(&self) {
        *self.state() = HostState::Stopped;
    }

    fn handle(&self) -> Option<PluginHandle> {
        match &*self.state() {
            HostState::Attached(handle) => Some(handle.clone()),
            _ => None,
        }
    }

    /// The guard is taken before the task exists and dropped when `work`
    /// returns, after it published its results.
    fn run(&self, handle: &PluginHandle, priority: Priority, work: Work) {
        let Some(busy) = handle.busy() else { return };
        let executor = self.executor.clone();
        handle.own_with(move |gate| {
            executor.run_gated(&gate, priority, move || {
                let _busy = busy;
                work();
            })
        });
    }

    fn emit(&self, event: ReviewEvent) {
        if let Some(handle) = self.handle() {
            handle.emitter().emit(event);
        }
    }
}

impl Host for KernelHost {
    fn spawn(&self, priority: Priority, work: Work) {
        let handle = {
            let mut state = self.state();
            match &mut *state {
                HostState::Waiting(queued) => {
                    queued.push((priority, work));
                    return;
                }
                HostState::Attached(handle) => handle.clone(),
                HostState::Stopped => return,
            }
        };
        self.run(&handle, priority, work);
    }

    /// Not busy while it waits: the kernel can be quiescent during the 30
    /// seconds. The wake only calls `work`, which submits a task.
    fn later(&self, delay: Duration, work: Work) {
        let waker = Waker::from(Arc::new(Later {
            work: Mutex::new(Some(work)),
        }));
        self.clock.wake_at(self.clock.now() + delay, waker);
    }

    fn busy(&self) -> Option<BusyGuard> {
        self.handle()?.busy()
    }
}

struct Later {
    work: Mutex<Option<Work>>,
}

impl Wake for Later {
    fn wake(self: Arc<Self>) {
        let work = self
            .work
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(work) = work {
            work();
        }
    }
}
