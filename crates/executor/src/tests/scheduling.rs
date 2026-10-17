//! Priorities, lost notifications, lazy thread start, results and the thread
//! start hook.

use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use super::support::*;
use crate::{Config, Executor, Priority, Shutdown, TaskError, ThreadKind};

#[test]
fn interactive_tasks_run_on_the_reserved_worker_while_background_work_fills_the_rest() {
    let exec = executor(2);
    let (started_tx, started) = mpsc::channel();
    let mut releases = Vec::new();
    let mut background = Vec::new();
    for index in 0..2 {
        let (release_tx, release) = mpsc::channel::<()>();
        releases.push(release_tx);
        let started_tx = started_tx.clone();
        background.push(exec.run(Priority::Background, move || {
            let _ = started_tx.send(index);
            let _ = release.recv();
        }));
    }
    // One background task occupies the only general worker; the other waits.
    assert_eq!(recv(&started, "first background task"), 0);

    let interactive = exec.run(Priority::Interactive, || {
        thread::current().name().map(str::to_owned)
    });
    assert_eq!(block_on(interactive), Ok(Some("executor-0".to_owned())));
    assert!(
        started.try_recv().is_err(),
        "the reserved worker took a background task"
    );

    drop(releases);
    for task in background {
        assert_eq!(block_on(task), Ok(()));
    }
    assert_eq!(recv(&started, "second background task"), 1);
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
}

#[test]
fn concurrent_submissions_all_run_while_workers_keep_going_idle() {
    let exec = executor(3);
    let done = Counter::default();
    for _round in 0..20 {
        let submitters: Vec<_> = (0..4)
            .map(|thread_index| {
                let exec = exec.clone();
                let done = done.clone();
                thread::spawn(move || {
                    (0..50)
                        .map(|index| {
                            let done = done.clone();
                            let priority = if (thread_index + index) % 3 == 0 {
                                Priority::Interactive
                            } else {
                                Priority::Background
                            };
                            exec.spawn(priority, async move {
                                Yield(index % 3).await;
                                done.bump();
                            })
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        for submitter in submitters {
            for task in submitter.join().unwrap() {
                assert_eq!(block_on(task), Ok(()));
            }
        }
        // Every worker is back to waiting on an empty queue here.
    }
    assert_eq!(done.get(), 20 * 4 * 50);
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
}

#[test]
fn no_thread_starts_before_the_first_submission() {
    let starts = Arc::new(Mutex::new(Vec::new()));
    let s = starts.clone();
    let exec = Executor::new(Config {
        workers: 3,
        batch_workers: 1,
        on_thread_start: Some(Arc::new(move |kind| {
            let name = thread::current().name().unwrap_or("").to_owned();
            s.lock().unwrap().push((kind, name));
        })),
    });
    assert!(starts.lock().unwrap().is_empty());

    assert_eq!(block_on(exec.run(Priority::Background, || 1 + 1)), Ok(2));
    assert_eq!(block_on(exec.run_dedicated("dedicated-hook", || 3)), Ok(3));
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
    let mut starts = starts.lock().unwrap().clone();
    starts.sort_by(|a, b| a.1.cmp(&b.1));
    assert_eq!(
        starts,
        vec![
            (ThreadKind::Dedicated, "dedicated-hook".to_owned()),
            (ThreadKind::Worker, "executor-0".to_owned()),
            (ThreadKind::Worker, "executor-1".to_owned()),
            (ThreadKind::Worker, "executor-2".to_owned()),
        ]
    );
}

#[test]
fn a_panicking_thread_start_hook_is_contained() {
    let exec = Executor::new(Config {
        workers: 1,
        batch_workers: 1,
        on_thread_start: Some(Arc::new(|_| panic!("hook panicked"))),
    });
    assert_eq!(block_on(exec.run(Priority::Background, || 5)), Ok(5));
    assert_eq!(block_on(exec.run_dedicated("hook-panics", || 6)), Ok(6));
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
}

#[test]
fn an_ungated_panic_becomes_the_result_and_the_worker_keeps_running() {
    let exec = executor(1);
    let task = exec.spawn(Priority::Background, async { panic!("ungated") });
    assert_eq!(
        block_on(task),
        Err::<(), _>(TaskError::Panicked("ungated".into()))
    );
    assert_eq!(
        block_on(exec.run(Priority::Background, || "next")),
        Ok("next")
    );
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
}

#[test]
fn a_task_can_await_another_task() {
    let exec = executor(2);
    let inner = exec.run(Priority::Background, || 20);
    let outer = exec.spawn(Priority::Interactive, async move {
        inner.await.map(|value| value + 1)
    });
    assert_eq!(block_on(outer), Ok(Ok(21)));
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
}

#[test]
fn the_default_config_uses_two_to_six_workers() {
    let config = Config::default();
    assert!((2..=6).contains(&config.workers));
    assert!(config.on_thread_start.is_none());
}
