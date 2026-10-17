//! `Executor::manual` and `run_until_idle`.

use super::support::*;
use crate::{Executor, Priority};

#[test]
fn run_until_idle_runs_tasks_in_queue_order() {
    let exec = Executor::manual();
    let log = Log::default();
    let mut tasks = Vec::new();
    for name in ["a", "b", "c"] {
        let l = log.clone();
        tasks.push(exec.spawn(Priority::Background, async move {
            l.push(format!("{name} 1"));
            Yield(1).await;
            l.push(format!("{name} 2"));
        }));
    }
    let l = log.clone();
    tasks.push(exec.spawn(Priority::Interactive, async move {
        l.push("interactive");
    }));
    // Nothing runs until asked.
    assert!(log.snapshot().is_empty());

    assert_eq!(exec.run_until_idle(), 7);
    assert_eq!(
        log.snapshot(),
        vec!["interactive", "a 1", "b 1", "c 1", "a 2", "b 2", "c 2"]
    );
    assert!(tasks.iter().all(crate::Task::is_finished));
    assert_eq!(exec.run_until_idle(), 0);
}

#[test]
fn a_manual_executor_starts_no_worker_thread() {
    let exec = Executor::manual();
    let task = exec.run(Priority::Background, || {
        std::thread::current().name().map(str::to_owned)
    });
    exec.run_until_idle();
    let here = std::thread::current().name().map(str::to_owned);
    assert_eq!(block_on(task), Ok(here));
}
