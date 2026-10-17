//! The batch pool (ADR 0029): `Priority::Batch` runs only on `executor-batch-N`,
//! the pool starts on the first batch submission, and it shuts down, cancels
//! and contains panics like the worker pool.

use std::cell::{Cell, RefCell};
use std::future::poll_fn;
use std::sync::{Arc, Mutex, mpsc};
use std::task::Poll;
use std::thread;
use std::time::Duration;

use lectern_kernel::PluginState;

use super::support::*;
use crate::util::lock;
use crate::{Config, Executor, Priority, Shutdown, TaskError, ThreadKind};

thread_local! {
    /// The kind the start hook was called with on this thread.
    static KIND: Cell<Option<ThreadKind>> = const { Cell::new(None) };
}

type Starts = Arc<Mutex<Vec<(ThreadKind, String)>>>;

/// An executor whose start hook records `(kind, thread name)` and stores the
/// kind in `KIND`.
fn recording(workers: usize, batch_workers: usize) -> (Executor, Starts) {
    let starts = Starts::default();
    let s = starts.clone();
    let exec = Executor::new(Config {
        workers,
        batch_workers,
        on_thread_start: Some(Arc::new(move |kind| {
            KIND.with(|cell| cell.set(Some(kind)));
            let name = thread::current().name().unwrap_or("").to_owned();
            s.lock().unwrap().push((kind, name));
        })),
    });
    (exec, starts)
}

fn sorted(starts: &Starts) -> Vec<(ThreadKind, String)> {
    let mut starts = starts.lock().unwrap().clone();
    starts.sort_by(|a, b| a.1.cmp(&b.1));
    starts
}

/// The name of this thread and the kind its start hook was called with.
fn here() -> (String, Option<ThreadKind>) {
    let name = thread::current().name().unwrap_or("unnamed").to_owned();
    (name, KIND.with(Cell::get))
}

#[test]
fn batch_tasks_run_only_on_batch_threads_and_other_tasks_never_do() {
    let (exec, _starts) = recording(2, 2);
    let mut batch = Vec::new();
    let mut pooled = Vec::new();
    for index in 0..300 {
        // Each task yields a few times, so it is requeued after wakes too.
        batch.push(exec.spawn(Priority::Batch, async move {
            Yield(index % 4).await;
            here()
        }));
        let priority = if index % 2 == 0 {
            Priority::Interactive
        } else {
            Priority::Background
        };
        pooled.push(exec.spawn(priority, async move {
            Yield(index % 3).await;
            here()
        }));
    }
    for task in batch {
        let (name, kind) = block_on(task).unwrap();
        assert!(name.starts_with("executor-batch-"), "{name}");
        assert_eq!(kind, Some(ThreadKind::Batch), "{name}");
    }
    for task in pooled {
        let (name, kind) = block_on(task).unwrap();
        assert!(
            name.starts_with("executor-") && !name.starts_with("executor-batch-"),
            "{name}"
        );
        assert_eq!(kind, Some(ThreadKind::Worker), "{name}");
    }
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
}

#[test]
fn batch_threads_start_on_the_first_batch_submission_with_the_batch_hook() {
    let (exec, starts) = recording(2, 3);
    assert!(starts.lock().unwrap().is_empty());

    assert_eq!(block_on(exec.run(Priority::Interactive, || 1)), Ok(1));
    assert_eq!(block_on(exec.run(Priority::Background, || 2)), Ok(2));
    // The worker pool is running; the batch pool has no thread.
    {
        let sched = lock(&exec.inner.sched);
        assert!(!sched.workers.threads.is_empty());
        assert!(sched.batch_pool.threads.is_empty());
    }
    assert!(
        starts
            .lock()
            .unwrap()
            .iter()
            .all(|(kind, _)| *kind == ThreadKind::Worker)
    );

    let task = exec.run(Priority::Batch, here);
    assert_eq!(
        block_on(task).unwrap().1,
        Some(ThreadKind::Batch),
        "the batch hook ran before the task"
    );
    assert_eq!(lock(&exec.inner.sched).batch_pool.threads.len(), 3);
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
    let mut expected = starts_of_kind(ThreadKind::Batch, 3, "executor-batch-");
    expected.extend(starts_of_kind(ThreadKind::Worker, 2, "executor-"));
    expected.sort_by(|a, b| a.1.cmp(&b.1));
    assert_eq!(sorted(&starts), expected);
}

fn starts_of_kind(kind: ThreadKind, count: usize, prefix: &str) -> Vec<(ThreadKind, String)> {
    (0..count)
        .map(|index| (kind, format!("{prefix}{index}")))
        .collect()
}

#[test]
fn batch_only_work_starts_no_worker_thread() {
    let (exec, starts) = recording(3, 1);
    let log = Log::default();
    let probe = DropProbe::new("result", &log);
    let task = exec.run(Priority::Batch, move || probe);
    wait_until("batch task finished", || task.is_finished());
    // The stored result is dropped by the batch pool, not by a worker.
    drop(task);
    wait_until("result dropped", || !log.snapshot().is_empty());
    assert_eq!(log.snapshot(), vec!["dropped result on executor-batch-0"]);
    assert!(lock(&exec.inner.sched).workers.threads.is_empty());
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
    assert_eq!(
        sorted(&starts),
        vec![(ThreadKind::Batch, "executor-batch-0".to_owned())]
    );
}

#[test]
fn the_default_batch_thread_count_is_a_quarter_of_the_parallelism_clamped_to_one_to_three() {
    for (parallelism, batch, workers) in [
        (1, 1, 2),
        (2, 1, 2),
        (3, 1, 2),
        (4, 1, 3),
        (7, 1, 6),
        (8, 2, 6),
        (11, 2, 6),
        (12, 3, 6),
        (16, 3, 6),
        (64, 3, 6),
    ] {
        let config = Config::for_parallelism(parallelism);
        assert_eq!(config.batch_workers, batch, "parallelism {parallelism}");
        assert_eq!(config.workers, workers, "parallelism {parallelism}");
        assert!(config.on_thread_start.is_none());
    }
    assert!((1..=3).contains(&Config::default().batch_workers));
}

#[test]
fn the_configured_number_of_batch_threads_start_and_zero_counts_as_one() {
    for (configured, started) in [(0, 1), (1, 1), (2, 2), (3, 3)] {
        let (exec, starts) = recording(1, configured);
        assert_eq!(block_on(exec.run(Priority::Batch, || ())), Ok(()));
        assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
        let batch = sorted(&starts)
            .into_iter()
            .filter(|(kind, _)| *kind == ThreadKind::Batch)
            .count();
        assert_eq!(batch, started, "batch_workers {configured}");
    }
}

#[test]
fn interactive_and_background_work_is_not_delayed_by_saturated_batch_threads() {
    let exec = pools(2, 2);
    let (started_tx, started) = mpsc::channel();
    let mut releases = Vec::new();
    let mut blocking = Vec::new();
    for index in 0..2 {
        let (release_tx, release) = mpsc::channel::<()>();
        releases.push(release_tx);
        let started_tx = started_tx.clone();
        blocking.push(exec.run(Priority::Batch, move || {
            let _ = started_tx.send(index);
            let _ = release.recv();
        }));
    }
    recv(&started, "first batch task");
    recv(&started, "second batch task");
    // Both batch threads are busy; more batch work queues behind them.
    let queued_ran = Counter::default();
    let queued: Vec<_> = (0..20)
        .map(|_| {
            let ran = queued_ran.clone();
            exec.run(Priority::Batch, move || {
                ran.bump();
            })
        })
        .collect();

    let pooled: Vec<_> = (0..100)
        .map(|index| {
            let priority = if index % 2 == 0 {
                Priority::Interactive
            } else {
                Priority::Background
            };
            exec.run(priority, move || index)
        })
        .collect();
    for (index, task) in pooled.into_iter().enumerate() {
        assert_eq!(block_on(task), Ok(index));
    }
    assert_eq!(queued_ran.get(), 0, "a worker took a batch task");

    drop(releases);
    for task in blocking.into_iter().chain(queued) {
        assert_eq!(block_on(task), Ok(()));
    }
    assert_eq!(queued_ran.get(), 20);
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
}

/// Counts the batch threads that ran to the end, from a thread-local
/// destructor.
struct ExitProbe(Arc<Mutex<usize>>);

impl Drop for ExitProbe {
    fn drop(&mut self) {
        *self.0.lock().unwrap() += 1;
    }
}

thread_local! {
    static EXIT: RefCell<Option<ExitProbe>> = const { RefCell::new(None) };
}

/// An executor whose batch threads count themselves in `exited` when their
/// thread-local destructors run.
fn counting_exits(workers: usize, batch_workers: usize) -> (Executor, Arc<Mutex<usize>>) {
    let exited = Arc::new(Mutex::new(0));
    let e = exited.clone();
    let exec = Executor::new(Config {
        workers,
        batch_workers,
        on_thread_start: Some(Arc::new(move |kind| {
            if kind == ThreadKind::Batch {
                EXIT.with(|probe| *probe.borrow_mut() = Some(ExitProbe(e.clone())));
            }
        })),
    });
    (exec, exited)
}

#[test]
fn shutdown_joins_the_batch_threads() {
    let (exec, exited) = counting_exits(2, 3);
    let tasks: Vec<_> = (0..10)
        .map(|index| exec.run(Priority::Batch, move || index))
        .collect();
    for (index, task) in tasks.into_iter().enumerate() {
        assert_eq!(block_on(task), Ok(index));
    }
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
    assert_eq!(*exited.lock().unwrap(), 3);
    assert!(lock(&exec.inner.sched).batch_pool.threads.is_empty());
}

#[test]
fn shutdown_drops_suspended_batch_tasks_on_a_batch_thread() {
    let exec = pools(2, 1);
    let log = Log::default();
    let probe = DropProbe::new("suspended", &log);
    let (polled_tx, polled) = mpsc::channel();
    let task = exec.spawn(
        Priority::Batch,
        poll_fn(move |_| -> Poll<()> {
            let _probe = &probe;
            let _ = polled_tx.send(());
            Poll::Pending
        }),
    );
    recv(&polled, "first poll");
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
    assert_eq!(
        log.snapshot(),
        vec!["dropped suspended on executor-batch-0"]
    );
    assert_eq!(block_on(task), Err(TaskError::Cancelled));
}

#[test]
fn shutdown_times_out_while_a_batch_task_blocks() {
    let exec = pools(2, 1);
    let (started_tx, started) = mpsc::channel();
    let (release_tx, release) = mpsc::channel::<()>();
    let task = exec.run(Priority::Batch, move || {
        let _ = started_tx.send(());
        let _ = release.recv();
    });
    recv(&started, "blocking batch task");
    // Only the batch pool was started.
    assert_eq!(
        exec.shutdown(Duration::from_millis(50)),
        Shutdown::TimedOut { busy_workers: 1 }
    );
    drop(release_tx);
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
    assert_eq!(block_on(task), Ok(()));
}

#[test]
fn a_shutdown_during_batch_startup_stops_further_spawns_and_joins_the_started_ones() {
    let (exec, exited) = counting_exits(1, 3);
    let (at_spawn_tx, at_spawn) = mpsc::channel();
    let (resume_tx, resume) = mpsc::channel::<()>();
    *lock(&exec.inner.before_spawn) = Some(Box::new(move |kind, index| {
        if kind == ThreadKind::Batch && index == 1 {
            let _ = at_spawn_tx.send(());
            let _ = resume.recv_timeout(LIMIT);
        }
    }));
    let (ran_tx, ran) = mpsc::channel();
    let submitting = exec.clone();
    let submitter = thread::spawn(move || {
        submitting
            .run(Priority::Batch, move || {
                let _ = ran_tx.send(());
            })
            .detach();
    });
    recv(&at_spawn, "second batch spawn");
    recv(&ran, "task ran on the first batch thread");

    let (result_tx, result) = mpsc::channel();
    let shutting = exec.clone();
    thread::spawn(move || {
        let _ = result_tx.send(shutting.shutdown(LIMIT));
    });
    wait_until("shutdown flag", || lock(&exec.inner.sched).shutdown);
    assert!(result.try_recv().is_err());

    drop(resume_tx);
    submitter.join().unwrap();
    assert_eq!(recv(&result, "shutdown"), Shutdown::Clean);
    assert_eq!(*exited.lock().unwrap(), 1);
    assert!(lock(&exec.inner.sched).batch_pool.threads.is_empty());
}

#[test]
fn a_submission_after_shutdown_is_cancelled_and_starts_no_batch_thread() {
    let (exec, starts) = recording(1, 2);
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
    let ran = Counter::default();
    let r = ran.clone();
    let task = exec.run(Priority::Batch, move || {
        r.bump();
    });
    assert_eq!(block_on(task), Err(TaskError::Cancelled));
    assert_eq!(ran.get(), 0);
    assert!(starts.lock().unwrap().is_empty());
}

#[test]
fn dropping_a_batch_handle_drops_the_future_on_a_batch_thread() {
    let exec = pools(1, 1);
    let log = Log::default();
    let probe = DropProbe::new("cancelled", &log);
    let (polled_tx, polled) = mpsc::channel();
    let task = exec.spawn(
        Priority::Batch,
        poll_fn(move |_| -> Poll<()> {
            let _probe = &probe;
            let _ = polled_tx.send(());
            Poll::Pending
        }),
    );
    recv(&polled, "first poll");
    drop(task);
    wait_until("future dropped", || !log.snapshot().is_empty());
    assert_eq!(
        log.snapshot(),
        vec!["dropped cancelled on executor-batch-0"]
    );
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
}

#[test]
fn a_panicking_batch_task_becomes_the_result_and_the_batch_thread_keeps_running() {
    let exec = pools(1, 1);
    let task = exec.spawn(Priority::Batch, async { panic!("batch panicked") });
    assert_eq!(
        block_on(task),
        Err::<(), _>(TaskError::Panicked("batch panicked".into()))
    );
    assert_eq!(
        block_on(exec.run(Priority::Batch, || here().0)),
        Ok("executor-batch-0".to_owned())
    );
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
}

#[test]
fn a_gated_batch_task_runs_after_start_and_its_panic_fails_the_plugin() {
    let plugin = TestPlugin::start();
    let exec = pools(1, 1);
    let log = Log::default();
    let (done_tx, done) = mpsc::channel();
    let (e, l) = (exec.clone(), log.clone());
    assert!(plugin.handle.own_with(move |gate| {
        let l2 = l.clone();
        let task = e.spawn_gated(&gate, Priority::Batch, async move {
            l2.push(format!("task ran on {}", here().0));
            let _ = done_tx.send(());
        });
        l.push("start returns");
        task
    }));
    recv(&done, "gated batch task ran");
    assert_eq!(
        log.snapshot(),
        vec!["start returns", "task ran on executor-batch-0"]
    );
    assert_eq!(plugin.state(), PluginState::Active);

    let e = exec.clone();
    assert!(plugin.handle.own_with(move |gate| {
        e.spawn_gated(&gate, Priority::Batch, async { panic!("gated batch") })
    }));
    plugin.wait_failed("gated batch");
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
}

#[test]
fn deactivating_the_plugin_drops_its_suspended_batch_tasks_on_a_batch_thread() {
    let plugin = TestPlugin::start();
    let exec = pools(1, 1);
    let log = Log::default();
    let (polled_tx, polled) = mpsc::channel();
    let probe = DropProbe::new("task", &log);
    let e = exec.clone();
    assert!(plugin.handle.own_with(move |gate| {
        e.spawn_gated(&gate, Priority::Batch, async move {
            let _probe = probe;
            let _ = polled_tx.send(());
            std::future::pending::<()>().await;
        })
    }));
    recv(&polled, "task polled");
    plugin.kernel.set_enabled("p", false);
    plugin.kernel.barrier();
    wait_until("task dropped", || !log.snapshot().is_empty());
    assert_eq!(log.snapshot(), vec!["dropped task on executor-batch-0"]);
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
}

#[test]
fn run_until_idle_runs_batch_tasks_after_background_ones() {
    let exec = Executor::manual();
    let log = Log::default();
    let mut tasks = Vec::new();
    for (name, priority) in [
        ("batch", Priority::Batch),
        ("background", Priority::Background),
        ("interactive", Priority::Interactive),
    ] {
        let l = log.clone();
        tasks.push(exec.spawn(priority, async move {
            l.push(name);
        }));
    }
    assert_eq!(exec.run_until_idle(), 3);
    assert_eq!(log.snapshot(), vec!["interactive", "background", "batch"]);
    assert!(tasks.iter().all(crate::Task::is_finished));
}
