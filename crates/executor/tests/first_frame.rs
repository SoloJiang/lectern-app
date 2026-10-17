//! No `executor-*` thread exists before the first submission, and no batch
//! thread before the first batch submission, read from the process's thread
//! list. One test in its own binary, so no other test's executor shares the
//! process.

#![cfg(target_os = "linux")]

use std::sync::mpsc;
use std::time::{Duration, Instant};

use lectern_executor::{Config, Executor, Priority, Shutdown};

const LIMIT: Duration = Duration::from_secs(10);

/// Polls the thread list until it is `expected`. Each thread names itself
/// once it runs, and the kernel may list a thread for a moment after it is
/// joined.
fn wait_for_threads(expected: &[&str]) {
    let deadline = Instant::now() + LIMIT;
    while executor_threads() != expected {
        assert!(Instant::now() < deadline, "{:?}", executor_threads());
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn executor_threads() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir("/proc/self/task")
        .expect("thread list")
        .filter_map(|entry| std::fs::read_to_string(entry.ok()?.path().join("comm")).ok())
        .map(|name| name.trim_end().to_owned())
        .filter(|name| name.starts_with("executor-"))
        .collect();
    names.sort();
    names
}

#[test]
fn executor_threads_exist_only_between_the_first_submission_and_shutdown() {
    let exec = Executor::new(Config {
        workers: 2,
        batch_workers: 2,
        on_thread_start: None,
    });
    assert!(executor_threads().is_empty());

    let (done_tx, done) = mpsc::channel();
    exec.run(Priority::Background, move || {
        let _ = done_tx.send(());
    })
    .detach();
    done.recv_timeout(LIMIT).expect("task ran");
    // Only the workers: no batch work was submitted.
    wait_for_threads(&["executor-0", "executor-1"]);

    let (done_tx, done) = mpsc::channel();
    exec.run(Priority::Batch, move || {
        let _ = done_tx.send(());
    })
    .detach();
    done.recv_timeout(LIMIT).expect("batch task ran");
    // Linux keeps 15 bytes of a thread name, so `executor-batch-N` reads as
    // `executor-batch-`.
    wait_for_threads(&[
        "executor-0",
        "executor-1",
        "executor-batch-",
        "executor-batch-",
    ]);

    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);
    wait_for_threads(&[]);
}
