//! `wake` only enqueues, a running task is never re-entered, a finished task
//! is never polled again.

use std::future::poll_fn;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex, mpsc};
use std::task::{Poll, Waker};
use std::thread;

use super::support::*;
use crate::{Executor, Priority, TaskError};

/// Takes `lock` when dropped and reports it.
struct LockOnDrop(Arc<Mutex<()>>, mpsc::Sender<()>);

impl Drop for LockOnDrop {
    fn drop(&mut self) {
        let _held = self.0.lock().unwrap();
        let _ = self.1.send(());
    }
}

#[test]
fn waking_or_cancelling_while_holding_the_tasks_lock_does_not_deadlock() {
    let exec = executor(2);
    let lock = Arc::new(Mutex::new(()));
    let stash: Arc<Mutex<Option<Waker>>> = Arc::default();
    let (polled_tx, polled) = mpsc::channel();
    let (dropped_tx, dropped) = mpsc::channel();
    let probe = LockOnDrop(lock.clone(), dropped_tx);
    let (l, s) = (lock.clone(), stash.clone());
    let task = exec.spawn(
        Priority::Background,
        poll_fn(move |cx| {
            let _probe = &probe;
            let _held = l.lock().unwrap();
            *s.lock().unwrap() = Some(cx.waker().clone());
            let _ = polled_tx.send(());
            Poll::<()>::Pending
        }),
    );
    recv(&polled, "first poll");
    let waker = stash.lock().unwrap().clone().unwrap();

    // The task's poll and drop both take `lock`; the test holds it while
    // another thread wakes and cancels the task. Both calls must return.
    let held = lock.lock().unwrap();
    let (returned_tx, returned) = mpsc::channel();
    thread::spawn(move || {
        waker.wake_by_ref();
        let _ = returned_tx.send("wake");
        drop(task);
        let _ = returned_tx.send("drop");
        waker.wake();
        let _ = returned_tx.send("wake after cancel");
    });
    assert_eq!(recv(&returned, "wake"), "wake");
    assert_eq!(recv(&returned, "drop"), "drop");
    assert_eq!(recv(&returned, "wake after cancel"), "wake after cancel");
    drop(held);

    // The future is dropped by a worker once the lock is free.
    recv(&dropped, "future dropped");
    assert_eq!(exec.shutdown(LIMIT), crate::Shutdown::Clean);
}

#[test]
fn a_task_woken_while_running_is_polled_once_more_and_never_on_two_threads() {
    let exec = executor(4);
    let polls = Counter::default();
    let inside = Counter::default();
    let reentered = Arc::new(AtomicBool::new(false));
    let stale: Arc<Mutex<Option<Waker>>> = Arc::default();
    let (p, i, r, s) = (
        polls.clone(),
        inside.clone(),
        reentered.clone(),
        stale.clone(),
    );
    let task = exec.spawn(
        Priority::Interactive,
        poll_fn(move |cx| {
            let n = p.bump();
            if i.bump() != 1 {
                r.store(true, SeqCst);
            }
            let result = if n < 3 {
                // Another thread wakes the task many times while this poll is
                // still running; every wake lands on the running task.
                let waker = cx.waker().clone();
                thread::spawn(move || {
                    for _ in 0..1000 {
                        waker.wake_by_ref();
                    }
                })
                .join()
                .unwrap();
                Poll::Pending
            } else {
                *s.lock().unwrap() = Some(cx.waker().clone());
                Poll::Ready(n)
            };
            i.drop_one();
            result
        }),
    );
    assert_eq!(block_on(task), Ok(3));
    // A wake after completion is ignored.
    stale.lock().unwrap().take().unwrap().wake();
    assert_eq!(exec.shutdown(LIMIT), crate::Shutdown::Clean);
    assert_eq!(polls.get(), 3);
    assert!(!reentered.load(SeqCst));
}

/// Wakes `waker` from another thread and waits for that thread.
fn wake_from_another_thread(waker: Waker) {
    thread::spawn(move || waker.wake()).join().unwrap();
}

struct WakeOnDrop(Arc<Mutex<Option<Waker>>>);

impl Drop for WakeOnDrop {
    fn drop(&mut self) {
        if let Some(waker) = self.0.lock().unwrap().clone() {
            wake_from_another_thread(waker);
        }
    }
}

#[test]
fn a_cloned_waker_called_while_the_task_panics_does_not_poll_it_again() {
    let exec = Executor::manual();
    let polls = Counter::default();
    let stash: Arc<Mutex<Option<Waker>>> = Arc::default();
    let on_drop = WakeOnDrop(stash.clone());
    let (p, s) = (polls.clone(), stash.clone());
    let task = exec.spawn(
        Priority::Background,
        poll_fn(move |cx| -> Poll<()> {
            let _on_drop = &on_drop;
            p.bump();
            *s.lock().unwrap() = Some(cx.waker().clone());
            // Woken from another thread while this poll runs, then panics;
            // the drop of the future wakes it again.
            wake_from_another_thread(cx.waker().clone());
            panic!("task panicked");
        }),
    );
    assert_eq!(exec.run_until_idle(), 1);
    assert_eq!(
        block_on(task),
        Err(TaskError::Panicked("task panicked".into()))
    );
    // And once more after it finished.
    wake_from_another_thread(stash.lock().unwrap().clone().unwrap());
    assert_eq!(exec.run_until_idle(), 0);
    assert_eq!(polls.get(), 1);
}
