//! Cancellation by dropping the handle, awaiting cancelled tasks, detach.

use std::future::poll_fn;
use std::sync::{Arc, Mutex, mpsc};
use std::task::Poll;
use std::thread;

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

/// Runs `f` on a new thread named `ui` and waits for it.
fn on_ui_thread(f: impl FnOnce() + Send + 'static) {
    thread::Builder::new()
        .name("ui".to_owned())
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn an_unread_result_is_dropped_by_the_executor_not_by_the_thread_dropping_the_handle() {
    let exec = Executor::manual();
    let log = Log::default();
    let (l1, l2) = (log.clone(), log.clone());
    let dropped = exec.run(Priority::Background, move || DropProbe::new("dropped", &l1));
    let detached = exec.run(Priority::Background, move || {
        DropProbe::new("detached", &l2)
    });
    let panicking = exec.run(Priority::Background, || PanicOnDrop);
    assert_eq!(exec.run_until_idle(), 3);
    assert!(dropped.is_finished() && detached.is_finished() && panicking.is_finished());

    // The results are already stored; giving up the handles only queues them.
    on_ui_thread(move || {
        drop(dropped);
        detached.detach();
        drop(panicking);
    });
    assert!(log.snapshot().is_empty());

    // Dropped by the executor under `contain`: the panic stays there.
    assert_eq!(exec.run_until_idle(), 3);
    let here = thread::current();
    let here = here.name().unwrap_or("unnamed");
    assert_eq!(
        log.snapshot(),
        vec![
            format!("dropped dropped on {here}"),
            format!("dropped detached on {here}"),
        ]
    );
}

/// Sends the name of the thread it is dropped on.
struct SendOnDrop(mpsc::Sender<String>);

impl Drop for SendOnDrop {
    fn drop(&mut self) {
        let name = thread::current().name().unwrap_or("unnamed").to_owned();
        let _ = self.0.send(name);
    }
}

#[test]
fn an_unread_result_left_after_shutdown_is_dropped_on_the_reclaim_thread() {
    let exec = Executor::manual();
    let (dropped_tx, dropped) = mpsc::channel();
    let task = exec.run(Priority::Interactive, move || SendOnDrop(dropped_tx));
    assert_eq!(exec.run_until_idle(), 1);
    assert_eq!(exec.shutdown(LIMIT), crate::Shutdown::Clean);
    on_ui_thread(move || drop(task));
    assert_eq!(recv(&dropped, "result dropped"), "executor-reclaim");
}
