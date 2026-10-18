//! `watchdog_*`: the watchdog's thread under the plugin (「调度、批次与取消」
//! the watchdog's lifecycle): it exits on deactivation and on the
//! executor's shutdown, there is at most one per activation however often
//! the plugin activates, an extraction running at deactivation returns
//! `Cancelled`, and an activation dead before the thread starts starts none.
//! Thread names come from `/proc/self/task/*/comm` (each test is its own
//! process under nextest).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::extract::{Deadline, ExtractStatus, Lang, extract};
use crate::testing::FakeGit;
use crate::watchdog::{SystemClock, WATCHDOG_THREAD};

use super::plugin::Run;

/// Threads of this process named like the watchdog (`comm` keeps 15 bytes).
fn watchdog_threads() -> usize {
    let name = &WATCHDOG_THREAD.as_bytes()[..15];
    std::fs::read_dir("/proc/self/task")
        .map(|tasks| {
            tasks
                .flatten()
                .filter(|t| {
                    std::fs::read(t.path().join("comm"))
                        .is_ok_and(|c| c.strip_suffix(b"\n").unwrap_or(&c) == name)
                })
                .count()
        })
        .unwrap_or(0)
}

/// Waits until `f` holds (threads exit asynchronously).
fn eventually(what: &str, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < Duration::from_secs(20), "never: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn repo() -> Arc<FakeGit> {
    let git = Arc::new(FakeGit::new());
    git.add("main", &[("a.ts", b"export function one() { return 1; }\n")]);
    git.commit("main");
    git
}

/// Activates and indexes: the first extraction starts the thread.
fn active(run: &Run) {
    run.kernel.activate().expect("activates");
    run.open();
    eventually("the watchdog thread started", || watchdog_threads() == 1);
}

#[test]
fn watchdog_exits_on_deactivation() {
    let run = Run::new(repo(), true);
    assert_eq!(watchdog_threads(), 0);
    active(&run);
    let watchdog = run.service().watchdog().expect("a watchdog").clone();
    run.kernel.deactivate();
    assert!(watchdog.is_stopped());
    eventually("the thread exited", || watchdog_threads() == 0);
}

#[test]
fn watchdog_one_thread_after_20_activations() {
    let run = Run::new(repo(), true);
    for i in 0..20 {
        run.kernel.activate().expect("activates");
        // A new file each time: an extraction, so the thread starts.
        let path = format!("f{i}.ts");
        run.git
            .add("main", &[(&path, format!("export function f{i}() {{}}\n").as_bytes())]);
        run.open();
        assert!(watchdog_threads() <= 1, "activation {i}: {}", watchdog_threads());
        run.kernel.deactivate();
        // Each deactivation ends its own thread: the count never grows.
        eventually("the activation's thread exited", || watchdog_threads() == 0);
    }
}

#[test]
fn watchdog_exits_on_executor_shutdown() {
    let run = Run::new(repo(), true);
    active(&run);
    // The activation is still live: the executor's shutdown stops it.
    let _ = run.executor.shutdown(Duration::from_secs(5));
    eventually("the thread exited", || watchdog_threads() == 0);
    assert!(run.service().watchdog().expect("a watchdog").is_stopped());
}

/// An extraction running when the plugin deactivates returns `Cancelled`
/// and writes nothing.
#[test]
fn watchdog_deactivation_cancels_a_running_extraction() {
    let git = repo();
    git.add("main", &[("b.ts", b"export function two() { return 2; }\n")]);
    git.commit("main");
    let run = Run::new(git, true);
    run.kernel.activate().expect("activates");
    let entered = Arc::new(AtomicBool::new(false));
    let statuses = Arc::new(std::sync::Mutex::new(Vec::new()));
    {
        let (entered, statuses) = (entered.clone(), statuses.clone());
        let watchdog = run.service().watchdog().expect("a watchdog").clone();
        run.service().indexer().expect("an index").set_extractor(
            move |lang: &Lang, bytes: &[u8], deadline: &Deadline, cancel: &Arc<AtomicBool>| {
                entered.store(true, Ordering::Release);
                // Running until deactivation reaches it: the watchdog stops
                // or the session's cancel flag is set.
                let start = Instant::now();
                while !watchdog.is_stopped() && !cancel.load(Ordering::Acquire) {
                    assert!(start.elapsed() < Duration::from_secs(20), "never cancelled");
                    std::thread::sleep(Duration::from_millis(1));
                }
                let e = extract(lang, bytes, deadline, cancel);
                statuses.lock().unwrap().push(e.status);
                e
            },
        );
    }
    let event = run.projects.set(run.git.snapshot(1, "main", Vec::new()));
    run.emit(event);
    std::thread::scope(|s| {
        let executor = run.executor.clone();
        let driver = &run.driver;
        let worker = s.spawn(move || {
            driver.run_until_idle();
            executor.run_until_idle();
        });
        let start = Instant::now();
        while !entered.load(Ordering::Acquire) {
            assert!(start.elapsed() < Duration::from_secs(20), "no extraction started");
            std::thread::sleep(Duration::from_millis(1));
        }
        run.kernel.deactivate();
        worker.join().expect("the executor's thread");
    });
    run.settle();
    let statuses = statuses.lock().unwrap().clone();
    assert!(!statuses.is_empty());
    assert!(
        statuses.iter().all(|s| *s == ExtractStatus::Cancelled),
        "{statuses:?}"
    );
}

/// The watchdog's starter submits the thread through `own_with`: for an
/// activation already dead it starts none, and the extraction still runs
/// (without a deadline enforcer).
#[test]
fn watchdog_no_thread_for_a_dead_activation() {
    let host = crate::testing::host();
    let handle = host.handle.clone();
    host.deactivate();
    let executor = lectern_executor::Executor::manual();
    let watchdog = crate::plugin::watchdog(&handle, &executor, Arc::new(SystemClock));
    let lang = crate::extract::LanguageTable::core()
        .get("ts")
        .cloned()
        .expect("ts");
    let deadline = Deadline::background(&watchdog);
    let e = extract(
        &lang,
        b"export function one() {}\n",
        &deadline,
        &Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(e.status, ExtractStatus::Done);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(watchdog_threads(), 0);
}
