//! Cancellation by dropping the handle, awaiting cancelled tasks, detach.

use std::future::poll_fn;
use std::sync::{Arc, Mutex};
use std::task::Poll;

use lectern_kernel::PluginError;

use super::support::*;
use crate::{Executor, Priority, TaskError};

#[test]
fn dropping_the_handle_stops_polling_and_drops_the_future() {
    let exec = Executor::manual();
    let polls = Counter::default();
    let log = Log::default();
    let probe = DropProbe::new("future", &log);
    let stash: Arc<Mutex<Option<std::task::Waker>>> = Arc::default();
    let (p, s) = (polls.clone(), stash.clone());
    let task = exec.spawn(
        Priority::Background,
        poll_fn(move |cx| -> Poll<()> {
            let _probe = &probe;
            p.bump();
            *s.lock().unwrap() = Some(cx.waker().clone());
            Poll::Pending
        }),
    );
    assert_eq!(exec.run_until_idle(), 1);
    stash.lock().unwrap().clone().unwrap().wake();
    assert_eq!(exec.run_until_idle(), 1);
    assert_eq!(polls.get(), 2);
    assert!(!task.is_finished());

    drop(task);
    // Dropping only flags the task; the next step drops the future.
    assert!(log.snapshot().is_empty());
    assert_eq!(exec.run_until_idle(), 1);
    assert_eq!(log.snapshot().len(), 1);
    // A wake after cancellation is ignored.
    stash.lock().unwrap().clone().unwrap().wake();
    assert_eq!(exec.run_until_idle(), 0);
    assert_eq!(polls.get(), 2);
}

#[test]
fn awaiting_a_cancelled_task_gives_cancelled() {
    let plugin = TestPlugin::start();
    let exec = Executor::manual();
    let stash = Arc::new(Mutex::new(None));
    let (e, s) = (exec.clone(), stash.clone());
    assert!(plugin.handle.own_with(move |gate| {
        let inner = e.spawn_gated(&gate, Priority::Background, std::future::pending::<u32>());
        *s.lock().unwrap() = Some(inner);
    }));
    let inner = stash.lock().unwrap().take().unwrap();
    let outer = exec.spawn(Priority::Background, inner);
    assert_eq!(exec.run_until_idle(), 2);
    assert!(!outer.is_finished());

    // The activation dies: the gated task is dropped, the awaiting task gets
    // `Cancelled`.
    plugin.handle.fail(PluginError::new("stop"));
    assert_eq!(exec.run_until_idle(), 2);
    assert!(outer.is_finished());
    assert_eq!(block_on(outer), Ok(Err(TaskError::Cancelled)));
}

#[test]
fn a_cancelled_handle_is_finished_and_resolves_to_cancelled() {
    let exec = Executor::manual();
    exec.shutdown(LIMIT);
    let task = exec.run(Priority::Interactive, || 1);
    assert!(task.is_finished());
    assert_eq!(block_on(task), Err(TaskError::Cancelled));
}

#[test]
fn a_detached_task_runs_to_completion() {
    let exec = Executor::manual();
    let done = Counter::default();
    let d = done.clone();
    exec.spawn(Priority::Background, async move {
        Yield(2).await;
        d.bump();
    })
    .detach();
    assert_eq!(exec.run_until_idle(), 3);
    assert_eq!(done.get(), 1);
}
