//! Scheduler state, worker threads and the reclaim thread.

use std::collections::{HashMap, VecDeque};
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use lectern_kernel::contain;

use crate::util::{lock, log, wait, wait_timeout};
use crate::{Hook, Priority, Shutdown, ThreadKind};

pub(crate) type TaskId = u64;

#[cfg(test)]
pub(crate) type BeforeSpawn = Box<dyn Fn(usize) + Send>;

/// A task the scheduler knows about: a pool task or a dedicated task.
pub(crate) trait Job: Send + Sync {
    /// Pool tasks: one step on a worker (or in `run_until_idle`). Dedicated
    /// tasks: the whole body of their thread.
    fn run(self: Arc<Self>);
    /// Sets the cancelled flag and wakes the task so it is dropped where it
    /// normally runs. Never drops the future or closure on the calling thread.
    fn cancel(self: Arc<Self>);
    /// Drops the unrun future or closure under `contain`, then finishes the
    /// task as cancelled. Used for tasks that were never handed to a worker.
    fn discard(self: Arc<Self>);
}

pub(crate) struct Sched {
    pub(crate) interactive: VecDeque<Arc<dyn Job>>,
    pub(crate) background: VecDeque<Arc<dyn Job>>,
    /// Live pool tasks: inserted on submission, removed once finished.
    pub(crate) live: HashMap<TaskId, Arc<dyn Job>>,
    /// Live dedicated tasks.
    pub(crate) dedicated: HashMap<TaskId, Arc<dyn Job>>,
    pub(crate) shutdown: bool,
    /// Set by the first submission, which starts the workers.
    started: bool,
    /// Set while the first submission is still starting workers; `shutdown`
    /// waits for it to clear.
    starting: bool,
    /// Workers that have been started and have not exited.
    alive: usize,
    /// Handles of the started workers, recorded before `starting` clears.
    pub(crate) threads: Vec<JoinHandle<()>>,
}

pub(crate) struct Inner {
    pub(crate) sched: Mutex<Sched>,
    /// Workers that take any task wait here.
    work: Condvar,
    /// The reserved worker, which only takes `Interactive` tasks, waits here.
    reserved: Condvar,
    /// `shutdown` waits here for the workers to exit.
    exited: Condvar,
    workers: usize,
    pub(crate) hook: Option<Hook>,
    manual: bool,
    next_id: AtomicU64,
    /// Called before each worker is spawned, outside every lock.
    #[cfg(test)]
    pub(crate) before_spawn: Mutex<Option<BeforeSpawn>>,
}

impl Inner {
    pub(crate) fn new(workers: usize, hook: Option<Hook>, manual: bool) -> Arc<Inner> {
        Arc::new(Inner {
            sched: Mutex::new(Sched {
                interactive: VecDeque::new(),
                background: VecDeque::new(),
                live: HashMap::new(),
                dedicated: HashMap::new(),
                shutdown: false,
                started: false,
                starting: false,
                alive: 0,
                threads: Vec::new(),
            }),
            work: Condvar::new(),
            reserved: Condvar::new(),
            exited: Condvar::new(),
            workers: workers.max(1),
            hook,
            manual,
            next_id: AtomicU64::new(0),
            #[cfg(test)]
            before_spawn: Mutex::new(None),
        })
    }

    pub(crate) fn next_id(&self) -> TaskId {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Registers and queues a new pool task, starting the workers on the
    /// first submission. After shutdown the task goes to a reclaim thread and
    /// this returns false.
    pub(crate) fn submit(
        self: &Arc<Self>,
        id: TaskId,
        priority: Priority,
        job: Arc<dyn Job>,
    ) -> bool {
        let start = {
            let mut sched = lock(&self.sched);
            if sched.shutdown {
                drop(sched);
                reclaim(job);
                return false;
            }
            sched.live.insert(id, job.clone());
            self.push(&mut sched, priority, job);
            let start = !self.manual && !sched.started;
            if start {
                sched.started = true;
                sched.starting = true;
            }
            start
        };
        if start {
            self.start_workers();
        }
        true
    }

    /// Puts a woken task back on its queue.
    pub(crate) fn enqueue(&self, priority: Priority, job: Arc<dyn Job>) {
        let mut sched = lock(&self.sched);
        self.push(&mut sched, priority, job);
    }

    /// Pushes under the scheduler lock and notifies before it is released, so
    /// a worker that just found the queues empty cannot miss the job.
    fn push(&self, sched: &mut Sched, priority: Priority, job: Arc<dyn Job>) {
        match priority {
            Priority::Interactive => {
                sched.interactive.push_back(job);
                self.work.notify_one();
                self.reserved.notify_one();
            }
            Priority::Background => {
                sched.background.push_back(job);
                self.work.notify_one();
                if sched.shutdown {
                    // The reserved worker helps drain after shutdown.
                    self.reserved.notify_one();
                }
            }
        }
    }

    /// Removes a finished pool task from the registry. The last removal after
    /// shutdown lets the workers exit.
    pub(crate) fn finished(&self, id: TaskId) {
        let removed = {
            let mut sched = lock(&self.sched);
            let removed = sched.live.remove(&id);
            if sched.shutdown && sched.live.is_empty() {
                self.work.notify_all();
                self.reserved.notify_all();
            }
            removed
        };
        drop(removed);
    }

    /// Registers a dedicated task. Returns false after shutdown, when the task
    /// has gone to a reclaim thread.
    pub(crate) fn register_dedicated(&self, id: TaskId, job: Arc<dyn Job>) -> bool {
        let mut sched = lock(&self.sched);
        if sched.shutdown {
            drop(sched);
            reclaim(job);
            return false;
        }
        sched.dedicated.insert(id, job);
        true
    }

    pub(crate) fn finished_dedicated(&self, id: TaskId) {
        let removed = lock(&self.sched).dedicated.remove(&id);
        drop(removed);
    }

    /// Starts the workers one by one. Each one is counted in `alive` before
    /// it is spawned and its handle is recorded before `starting` clears, so
    /// `shutdown` waits for all of them and joins every one. A shutdown that
    /// begins meanwhile stops further spawns; the first worker is still
    /// started when cancelled tasks are left to drop.
    fn start_workers(self: &Arc<Self>) {
        let reserve = self.workers >= 2;
        for index in 0..self.workers {
            #[cfg(test)]
            if let Some(before_spawn) = &*lock(&self.before_spawn) {
                before_spawn(index);
            }
            {
                let mut sched = lock(&self.sched);
                if sched.shutdown && (index > 0 || sched.live.is_empty()) {
                    break;
                }
                sched.alive += 1;
            }
            let inner = self.clone();
            let reserved = reserve && index == 0;
            let spawned = thread::Builder::new()
                .name(format!("executor-{index}"))
                .spawn(move || worker(inner, reserved));
            let mut sched = lock(&self.sched);
            match spawned {
                Ok(handle) => sched.threads.push(handle),
                Err(error) => {
                    sched.alive -= 1;
                    drop(sched);
                    log(format_args!("could not start worker {index}: {error}"));
                }
            }
        }
        lock(&self.sched).starting = false;
        self.exited.notify_all();
    }

    /// Hands a value to a worker to drop under `contain` (after shutdown, to
    /// the reclaim thread), so the calling thread never runs its `Drop`.
    pub(crate) fn drop_later(self: &Arc<Self>, value: Box<dyn Send>) {
        let id = self.next_id();
        let job = Arc::new(Unread {
            id,
            value: Mutex::new(Some(value)),
            exec: Arc::downgrade(self),
        });
        self.submit(id, Priority::Background, job);
    }

    /// Runs queued tasks on the calling thread until the queues are empty.
    pub(crate) fn run_until_idle(&self) -> usize {
        let mut runs = 0;
        loop {
            let job = {
                let mut sched = lock(&self.sched);
                sched
                    .interactive
                    .pop_front()
                    .or_else(|| sched.background.pop_front())
            };
            let Some(job) = job else { return runs };
            run_job(job);
            runs += 1;
        }
    }

    pub(crate) fn shutdown(&self, timeout: Duration) -> Shutdown {
        let deadline = Instant::now() + timeout;
        let (pool, dedicated) = {
            let mut sched = lock(&self.sched);
            sched.shutdown = true;
            // Idle workers recheck the exit condition.
            self.work.notify_all();
            self.reserved.notify_all();
            let pool: Vec<_> = sched.live.values().cloned().collect();
            let dedicated: Vec<_> = sched.dedicated.values().cloned().collect();
            (pool, dedicated)
        };
        for job in pool {
            job.cancel();
        }
        for job in dedicated {
            job.cancel();
        }
        if self.manual {
            self.run_until_idle();
            return Shutdown::Clean;
        }
        let mut sched = lock(&self.sched);
        while sched.starting || sched.alive > 0 {
            let now = Instant::now();
            if now >= deadline {
                return Shutdown::TimedOut {
                    busy_workers: sched.alive,
                };
            }
            sched = wait_timeout(&self.exited, sched, deadline - now);
        }
        // Startup is over and every worker has left its loop; joining waits
        // only for the threads to finish exiting.
        let threads = std::mem::take(&mut sched.threads);
        drop(sched);
        for thread in threads {
            let _ = thread.join();
        }
        Shutdown::Clean
    }
}

pub(crate) fn run_hook(hook: Option<&Hook>, kind: ThreadKind) {
    if let Some(hook) = hook
        && let Err(message) = contain(|| hook(kind))
    {
        log(format_args!("thread start hook panicked: {message}"));
    }
}

fn worker(inner: Arc<Inner>, reserved: bool) {
    run_hook(inner.hook.as_ref(), ThreadKind::Worker);
    let mut sched = lock(&inner.sched);
    loop {
        let job = match sched.interactive.pop_front() {
            Some(job) => Some(job),
            // After shutdown only cancelled tasks are left; the reserved
            // worker helps drop them (it may be the only one started).
            None if !reserved || sched.shutdown => sched.background.pop_front(),
            None => None,
        };
        if let Some(job) = job {
            drop(sched);
            run_job(job);
            sched = lock(&inner.sched);
            continue;
        }
        if sched.shutdown && sched.live.is_empty() {
            break;
        }
        let condvar = if reserved {
            &inner.reserved
        } else {
            &inner.work
        };
        sched = wait(condvar, sched);
    }
    sched.alive -= 1;
    inner.exited.notify_all();
}

/// A task result nobody will read, waiting to be dropped.
struct Unread {
    id: TaskId,
    value: Mutex<Option<Box<dyn Send>>>,
    exec: Weak<Inner>,
}

impl Unread {
    fn release(&self) {
        let value = lock(&self.value).take();
        if let Err(message) = contain(move || drop(value)) {
            log(format_args!("dropping a task result panicked: {message}"));
        }
    }
}

impl Job for Unread {
    fn run(self: Arc<Self>) {
        self.release();
        if let Some(exec) = self.exec.upgrade() {
            exec.finished(self.id);
        }
    }

    /// Already queued; dropping the value is all it does.
    fn cancel(self: Arc<Self>) {}

    fn discard(self: Arc<Self>) {
        self.release();
    }
}

/// Every panic inside a step is already caught; this is a last guard so a bug
/// cannot take a worker down.
fn run_job(job: Arc<dyn Job>) {
    if let Err(message) = contain(move || job.run()) {
        log(format_args!(
            "a task step panicked outside its guard: {message}"
        ));
    }
}

/// Drops a task submitted after shutdown on a new detached thread, so the
/// submitting thread never waits for the future's `Drop`.
fn reclaim(job: Arc<dyn Job>) {
    // If the thread cannot be spawned the closure is dropped here, and
    // `ManuallyDrop` turns that into a leak instead of a drop on this thread.
    let job = ManuallyDrop::new(job);
    let spawned = thread::Builder::new()
        .name("executor-reclaim".to_owned())
        .spawn(move || ManuallyDrop::into_inner(job).discard());
    if spawned.is_err() {
        log(format_args!(
            "could not spawn a reclaim thread; leaking a task submitted after shutdown"
        ));
    }
}
