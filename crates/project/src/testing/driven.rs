//! `Driven`: the project model on the production git access with its work
//! run on the caller's thread, for bench's `project/*` (docs/design/
//! project.md「性能」). Nothing goes through the kernel: what the model
//! emits and the facts it submits are logged with the time, so one run can
//! be split into its phases (file list ready, sub-projects ready).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use lectern_fs_watch::Watcher;
use lectern_kernel::{BusyGuard, WorkspaceFacts};
use lectern_services::project::{ProjectEvent, Projects};

use crate::git_access::LecternGit;
use crate::model::{Host, Model};

/// What the model handed its host.
pub enum Logged {
    Event(ProjectEvent),
    /// A `KernelControl::workspace_changed` submission, logged once the
    /// facts are built.
    Facts(Arc<dyn WorkspaceFacts>),
}

type Job = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct LogHost {
    jobs: Mutex<VecDeque<Job>>,
    log: Mutex<Vec<(Instant, Logged)>>,
}

impl Host for LogHost {
    fn spawn(&self, work: Job) {
        lock(&self.jobs).push_back(work);
    }

    fn emit(&self, event: ProjectEvent) {
        lock(&self.log).push((Instant::now(), Logged::Event(event)));
    }

    fn workspace_changed(&self, facts: Arc<dyn WorkspaceFacts>) {
        lock(&self.log).push((Instant::now(), Logged::Facts(facts)));
    }

    fn busy(&self) -> Option<BusyGuard> {
        None
    }
}

/// The model `ProjectPlugin` provides, over `lectern-git` reading only each
/// repository's own configuration (`lectern_git::testing::discover`: the
/// machine's git configuration can't change a measurement). Its work
/// queues until [`Driven::run`].
pub struct Driven {
    model: Arc<Model>,
    host: Arc<LogHost>,
}

impl Driven {
    /// `home` as `ProjectDeps::home`; with a watcher, open git projects
    /// refresh from it as in the app.
    pub fn new(home: PathBuf, watcher: Option<Watcher>) -> Driven {
        let git = Arc::new(LecternGit::with_discover(lectern_git::testing::discover));
        let model = Model::new(git, home);
        if let Some(watcher) = watcher {
            model.watch_with(watcher);
        }
        let host = Arc::new(LogHost::default());
        model.attach(host.clone());
        Driven { model, host }
    }

    pub fn projects(&self) -> &dyn Projects {
        &*self.model
    }

    /// Runs the queued work, and the work it queues, on this thread until
    /// none is left. Returns how many jobs ran.
    pub fn run(&self) -> usize {
        let mut ran = 0;
        loop {
            let job = lock(&self.host.jobs).pop_front();
            let Some(job) = job else {
                return ran;
            };
            job();
            ran += 1;
        }
    }

    /// What the model emitted and submitted since the last call, in order,
    /// with the time it did.
    pub fn take_log(&self) -> Vec<(Instant, Logged)> {
        std::mem::take(&mut *lock(&self.host.log))
    }
}

impl Drop for Driven {
    /// Drops the projects and their watches; queued jobs hold the model.
    fn drop(&mut self) {
        self.model.shutdown();
        lock(&self.host.jobs).clear();
        lock(&self.host.log).clear();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
