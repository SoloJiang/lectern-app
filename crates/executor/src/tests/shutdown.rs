//! `shutdown` and submissions after it.

use std::cell::RefCell;
use std::future::poll_fn;
use std::sync::{Arc, Mutex, mpsc};
use std::task::Poll;
use std::thread;
use std::time::Duration;

use super::support::*;
use crate::util::lock;
use crate::{Config, Executor, Priority, Shutdown, TaskError};

#[test]
fn shutdown_drops_suspended_tasks_on_a_worker() {
    let exec = executor(2);
    let log = Log::default();
    let probe = DropProbe::new("suspended", &log);
    let (polled_tx, polled) = mpsc::channel();
    // Polled once, then waits for a wake that never comes: the task is in no
    // queue when `shutdown` runs.
    let task = exec.spawn(
        Priority::Background,
        poll_fn(move |_| -> Poll<()> {
            let _probe = &probe;
            let _ = polled_tx.send(());
            Poll::Pending
        }),
    );
    recv(&polled, "first poll");
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
    let entries = log.snapshot();
    assert_eq!(entries.len(), 1);
    assert!(
        entries[0].starts_with("dropped suspended on executor-"),
        "{entries:?}"
    );
    assert_eq!(block_on(task), Err(TaskError::Cancelled));
}

/// Counts the threads that ran to the end, from a thread-local destructor.
struct ExitProbe(Arc<Mutex<usize>>);

impl Drop for ExitProbe {
    fn drop(&mut self) {
        *self.0.lock().unwrap() += 1;
    }
}

thread_local! {
    static EXIT: RefCell<Option<ExitProbe>> = const { RefCell::new(None) };
}

#[test]
fn shutdown_returns_after_the_workers_exit() {
    let exited = Arc::new(Mutex::new(0));
    let e = exited.clone();
    let exec = Executor::new(Config {
        workers: 3,
        batch_workers: 1,
        on_thread_start: Some(Arc::new(move |_| {
            EXIT.with(|probe| *probe.borrow_mut() = Some(ExitProbe(e.clone())));
        })),
    });
    let tasks: Vec<_> = (0..10)
        .map(|index| exec.run(Priority::Background, move || index))
        .collect();
    for (index, task) in tasks.into_iter().enumerate() {
        assert_eq!(block_on(task), Ok(index));
    }
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
    assert_eq!(*exited.lock().unwrap(), 3);
}

#[test]
fn shutdown_times_out_while_a_task_blocks() {
    let exec = executor(2);
    let (started_tx, started) = mpsc::channel();
    let (release_tx, release) = mpsc::channel::<()>();
    let task = exec.run(Priority::Background, move || {
        let _ = started_tx.send(());
        let _ = release.recv();
    });
    recv(&started, "blocking task");
    assert_eq!(
        exec.shutdown(Duration::from_millis(50)),
        Shutdown::TimedOut { busy_workers: 2 }
    );
    drop(release_tx);
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
    // It finished on its own after being cancelled.
    assert_eq!(block_on(task), Ok(()));
}

#[test]
fn shutdown_before_any_submission_is_clean() {
    let exec = executor(2);
    assert_eq!(exec.shutdown(Duration::ZERO), Shutdown::Clean);
}

/// Takes `lock` when dropped and reports the thread it was dropped on.
struct BlockingDrop(Arc<Mutex<()>>, mpsc::Sender<String>);

impl Drop for BlockingDrop {
    fn drop(&mut self) {
        let _held = self.0.lock().unwrap();
        let name = thread::current().name().unwrap_or("unnamed").to_owned();
        let _ = self.1.send(name);
    }
}

#[test]
fn a_submission_after_shutdown_is_cancelled_and_dropped_on_the_reclaim_thread() {
    let exec = executor(2);
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);

    let ran = Counter::default();
    let r = ran.clone();
    let task = exec.spawn(Priority::Interactive, async move {
        r.bump();
    });
    assert!(task.is_finished());
    assert_eq!(block_on(task), Err(TaskError::Cancelled));

    // The future's `Drop` blocks on a lock this test holds; submitting from
    // another thread still returns at once.
    let lock = Arc::new(Mutex::new(()));
    let (dropped_tx, dropped) = mpsc::channel();
    let held = lock.lock().unwrap();
    let probe = BlockingDrop(lock.clone(), dropped_tx);
    let (submitted_tx, submitted) = mpsc::channel();
    let e = exec.clone();
    thread::spawn(move || {
        let task = e.spawn(Priority::Background, async move {
            let _probe = probe;
        });
        let _ = submitted_tx.send(task.is_finished());
    });
    assert!(recv(&submitted, "submission returned"));
    drop(held);
    assert_eq!(recv(&dropped, "future dropped"), "executor-reclaim");
    assert_eq!(ran.get(), 0);
}

#[test]
fn shutdown_drops_a_dedicated_task_still_waiting_for_its_gate() {
    let plugin = TestPlugin::start();
    let (started_tx, started) = mpsc::channel();
    let started_tx = Mutex::new(started_tx);
    let exec = Executor::new(Config {
        workers: 1,
        batch_workers: 1,
        on_thread_start: Some(Arc::new(move |_| {
            let _ = started_tx.lock().unwrap().send(());
        })),
    });
    let log = Log::default();
    let probe = DropProbe::new("closure", &log);
    let stash = Arc::new(Mutex::new(None));
    let (resume_tx, resume) = mpsc::channel::<()>();
    let (e, s, handle) = (exec.clone(), stash.clone(), plugin.handle.clone());
    // `start` keeps the gate closed until the test resumes it, after shutdown.
    let submitter = thread::spawn(move || {
        handle.own_with(move |gate| {
            let task = e.run_dedicated_gated(&gate, "gated-shutdown", move || {
                let _probe = &probe;
            });
            *s.lock().unwrap() = Some(task);
            let _ = resume.recv_timeout(LIMIT);
        })
    });
    recv(&started, "dedicated thread started");
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
    drop(resume_tx);
    assert!(submitter.join().unwrap());
    let task = stash.lock().unwrap().take().unwrap();
    assert_eq!(block_on(task), Err(TaskError::Cancelled));
    assert_eq!(log.snapshot(), vec!["dropped closure on gated-shutdown"]);
}

#[test]
fn a_shutdown_during_worker_startup_stops_further_spawns_and_joins_the_started_workers() {
    let started = Arc::new(Mutex::new(Vec::new()));
    let exited = Arc::new(Mutex::new(0));
    let (s, e) = (started.clone(), exited.clone());
    let exec = Executor::new(Config {
        workers: 3,
        batch_workers: 1,
        on_thread_start: Some(Arc::new(move |_| {
            let name = thread::current().name().unwrap_or("").to_owned();
            s.lock().unwrap().push(name);
            EXIT.with(|probe| *probe.borrow_mut() = Some(ExitProbe(e.clone())));
        })),
    });
    // The first submission stops before spawning the second worker until
    // the test resumes it.
    let (at_spawn_tx, at_spawn) = mpsc::channel();
    let (resume_tx, resume) = mpsc::channel::<()>();
    *lock(&exec.inner.before_spawn) = Some(Box::new(move |_kind, index| {
        if index == 1 {
            let _ = at_spawn_tx.send(());
            let _ = resume.recv_timeout(LIMIT);
        }
    }));
    let (ran_tx, ran) = mpsc::channel();
    let submitting = exec.clone();
    let submitter = thread::spawn(move || {
        submitting
            .run(Priority::Interactive, move || {
                let _ = ran_tx.send(());
            })
            .detach();
    });
    recv(&at_spawn, "second spawn");
    recv(&ran, "task ran on the first worker");

    let (result_tx, result) = mpsc::channel();
    let shutting = exec.clone();
    thread::spawn(move || {
        let _ = result_tx.send(shutting.shutdown(LIMIT));
    });
    wait_until("shutdown flag", || lock(&exec.inner.sched).shutdown);
    // Startup is still in progress, so shutdown cannot have finished.
    assert!(result.try_recv().is_err());

    drop(resume_tx);
    submitter.join().unwrap();
    assert_eq!(recv(&result, "shutdown"), Shutdown::Clean);
    // No worker was spawned after shutdown began, and the one that was is
    // joined, its thread-local destructors already run.
    assert_eq!(*started.lock().unwrap(), vec!["executor-0".to_owned()]);
    assert_eq!(*exited.lock().unwrap(), 1);
    assert!(lock(&exec.inner.sched).workers.threads.is_empty());
}
