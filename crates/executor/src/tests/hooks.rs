//! `on_shutdown` callbacks.

use std::sync::{Arc, Barrier, mpsc};
use std::thread;

use super::support::*;
use crate::{Executor, Priority, Shutdown};

/// Logs `ran <name> on <thread>`.
fn note(log: &Log, name: &str) {
    let thread = thread::current();
    let thread = thread.name().unwrap_or("unnamed");
    log.push(format!("ran {name} on {thread}"));
}

#[test]
fn a_callback_runs_once_on_the_shutdown_thread_before_the_workers_are_joined() {
    let exec = executor(2);
    let log = Log::default();
    // A worker blocks until the callback releases it: `shutdown` can only
    // return `Clean` if the callback runs before it waits for the workers.
    let (started_tx, started) = mpsc::channel();
    let (release_tx, release) = mpsc::channel::<()>();
    let task = exec.run(Priority::Background, move || {
        started_tx.send(()).unwrap();
        let _ = release.recv();
    });
    recv(&started, "task start");
    let l = log.clone();
    let _hook = exec.on_shutdown(Box::new(move || {
        note(&l, "stop");
        let _ = release_tx.send(());
    }));
    let e = exec.clone();
    let result = thread::Builder::new()
        .name("closer".into())
        .spawn(move || (e.shutdown(LIMIT), e.shutdown(LIMIT)))
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(result, (Shutdown::Clean, Shutdown::Clean));
    assert_eq!(log.snapshot(), ["ran stop on closer"]);
    assert_eq!(block_on(task), Ok(()));
}

#[test]
fn a_callback_stops_a_dedicated_loop() {
    let exec = executor(1);
    let (started_tx, started) = mpsc::channel();
    let (stop_tx, stop) = mpsc::channel::<()>();
    let (exited_tx, exited) = mpsc::channel();
    // `shutdown` does not wait for dedicated threads; the callback is how the
    // loop (a stand-in for a watchdog) learns about it.
    let worker = exec.run_dedicated("loop", move || {
        started_tx.send(()).unwrap();
        let _ = stop.recv();
        exited_tx.send(()).unwrap();
    });
    recv(&started, "loop start");
    let _hook = exec.on_shutdown(Box::new(move || drop(stop_tx)));
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
    recv(&exited, "loop exit");
    drop(worker);
}

#[test]
fn a_dropped_hook_does_not_run_and_its_callback_is_dropped_at_once() {
    let exec = executor(1);
    let log = Log::default();
    let (l, probe) = (log.clone(), DropProbe::new("callback", &log));
    let hook = exec.on_shutdown(Box::new(move || {
        let _probe = &probe;
        note(&l, "dropped hook");
    }));
    let l = log.clone();
    let _kept = exec.on_shutdown(Box::new(move || note(&l, "kept hook")));
    let thread = thread::Builder::new()
        .name("dropper".into())
        .spawn(move || drop(hook))
        .unwrap();
    thread.join().unwrap();
    assert_eq!(log.snapshot(), ["dropped callback on dropper"]);
    let e = exec.clone();
    let result = thread::Builder::new()
        .name("closer".into())
        .spawn(move || e.shutdown(LIMIT))
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(result, Shutdown::Clean);
    assert_eq!(
        log.snapshot(),
        ["dropped callback on dropper", "ran kept hook on closer"]
    );
}

#[test]
fn registering_after_shutdown_started_runs_the_callback_at_once() {
    let exec = executor(1);
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
    let log = Log::default();
    let l = log.clone();
    let hook = thread::Builder::new()
        .name("late".into())
        .spawn({
            let exec = exec.clone();
            move || exec.on_shutdown(Box::new(move || note(&l, "late")))
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(log.snapshot(), ["ran late on late"]);
    drop(hook);
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
    assert_eq!(log.snapshot(), ["ran late on late"]);
}

#[test]
fn a_registration_racing_shutdown_runs_exactly_once() {
    for _ in 0..200 {
        let exec = executor(1);
        let ran = Counter::default();
        let start = Arc::new(Barrier::new(2));
        let registrar = thread::spawn({
            let (exec, ran, start) = (exec.clone(), ran.clone(), start.clone());
            move || {
                start.wait();
                exec.on_shutdown(Box::new(move || {
                    ran.bump();
                }))
            }
        });
        start.wait();
        assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
        let hook = registrar.join().unwrap();
        // Either `shutdown` took it or the registration ran it; both are
        // done by now.
        assert_eq!(ran.get(), 1);
        drop(hook);
        assert_eq!(ran.get(), 1);
    }
}

#[test]
fn a_panicking_callback_stops_neither_the_others_nor_the_shutdown() {
    let exec = executor(2);
    let log = Log::default();
    let task = exec.run(Priority::Background, || 7);
    assert_eq!(block_on(task), Ok(7));
    let hooks: Vec<_> = ["first", "panics", "last"]
        .into_iter()
        .map(|name| {
            let l = log.clone();
            exec.on_shutdown(Box::new(move || {
                note(&l, name);
                assert_ne!(name, "panics", "shutdown hook failed");
            }))
        })
        .collect();
    let e = exec.clone();
    let result = thread::Builder::new()
        .name("closer".into())
        .spawn(move || e.shutdown(LIMIT))
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(result, Shutdown::Clean);
    assert_eq!(
        log.snapshot(),
        [
            "ran first on closer",
            "ran panics on closer",
            "ran last on closer"
        ]
    );
    drop(hooks);
}

#[test]
fn the_manual_executor_runs_callbacks_on_shutdown() {
    let exec = Executor::manual();
    let ran = Counter::default();
    let r = ran.clone();
    let _hook = exec.on_shutdown(Box::new(move || {
        r.bump();
    }));
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
    assert_eq!(ran.get(), 1);
}
