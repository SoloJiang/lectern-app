//! `requeue_*`: the batch loop gives its thread back at every batch
//! boundary (`yield_now()` after each `Step::Worked`, `index/batch.rs`), so
//! the batch tasks go to the back of the executor's first-in first-out
//! `Batch` queue, carrying their `Parked` item with its `LargeTurn`
//! (file_reader design 「批量线程上的先后」, ADR 0041). These run on a real
//! executor with one and with two batch threads (`Options::batch_threads`),
//! one batch task per thread as in production; tasks queued from outside
//! stand for `syntax`'s batch work. Every step waits for a signal from the
//! code under test (a hook, a channel, `LargeSlot::wait_for_waits`); the
//! polling in `wait_until` only waits for results (units in the store, an
//! idle pool).

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use lectern_executor::Priority;
use lectern_services::project::BlobId;

use crate::extract::ExtractStatus;
use crate::index::batch::BATCH_TIME;
use crate::testing::synthetic;

use super::support::{Harness, Options, packages, ts};

/// How long a step may take before the test fails. Only reached when the
/// property under test does not hold (a task never gets a thread).
const LIMIT: Duration = Duration::from_secs(30);

/// `threads` batch threads and as many batch tasks.
fn threaded(threads: usize) -> Harness {
    Harness::with(Options {
        batch_tasks: threads,
        batch_threads: Some(threads),
        ..Options::default()
    })
}

/// Runs fs_watch's manual driver until `done` holds.
fn wait_until(h: &Harness, what: &str, done: impl Fn() -> bool) {
    let start = Instant::now();
    loop {
        h.driver.run_until_idle();
        if done() {
            return;
        }
        assert!(start.elapsed() < LIMIT, "{what}: not within {LIMIT:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// No batch task runs and none has work.
fn idle(h: &Harness) -> bool {
    let inner = h.indexer.inner();
    inner.batch.lock().unwrap().running == 0 && !inner.runnable()
}

fn stored(h: &Harness, oid: BlobId) -> bool {
    h.stored(oid, "ts")
}

/// The statuses the extractions of `oid` returned, in order.
fn statuses(h: &Harness, oid: BlobId) -> Vec<ExtractStatus> {
    h.seen
        .results
        .lock()
        .unwrap()
        .iter()
        .filter(|(o, _)| *o == oid)
        .map(|(_, s)| *s)
        .collect()
}

/// Gives the executor back `n` times, going to the back of the queue each
/// time.
fn yield_times(n: usize) -> impl Future<Output = ()> {
    struct Yield(usize);
    impl Future for Yield {
        type Output = ();
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.0 == 0 {
                Poll::Ready(())
            } else {
                self.0 -= 1;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }
    Yield(n)
}

/// What the extraction hook of `requeue_outside_task_*` shares with the
/// test.
#[derive(Default)]
struct Gate {
    state: Mutex<GateState>,
    changed: Condvar,
}

#[derive(Default)]
struct GateState {
    /// The threads whose first extraction waits for `released`.
    first: HashSet<ThreadId>,
    released: bool,
    /// The task queued from outside has started.
    started: bool,
    /// Extraction batches ended when it started.
    ended_at_start: usize,
    /// Extractions of a batch begun after the task was queued that found
    /// it not started yet.
    early: usize,
    /// Of those, the ones that waited `LIMIT` in vain.
    starved: usize,
}

impl Gate {
    fn wait(&self, until: impl Fn(&GateState) -> bool) -> std::sync::MutexGuard<'_, GateState> {
        let st = self.state.lock().unwrap();
        self.changed
            .wait_timeout_while(st, LIMIT, |st| !until(st))
            .unwrap()
            .0
    }
}

/// All batch threads are in a batch of code_intel's tasks when a `Batch`
/// task comes from outside: it starts no later than each thread's next
/// batch boundary. Every thread's first extraction waits until the task is
/// queued and the manual clock is past `BATCH_TIME`, so each of those
/// batches ends after that file; any extraction after that is in a batch
/// begun later, and waits for the outside task to have started. Were the
/// batch tasks not to give their thread back at the boundary, or to go
/// before the outside task, those extractions would wait in vain.
fn outside_task_starts_by_the_next_boundary(threads: usize) {
    const FILES: usize = 24;
    let h = threaded(threads);
    let sources: Vec<Vec<u8>> = (0..FILES).map(|i| ts(&format!("f{i}"))).collect();
    for (i, source) in sources.iter().enumerate() {
        h.git.add("main", &[(&format!("lib/f{i:02}.ts"), source)]);
    }
    h.git.commit("main");
    let gate = Arc::new(Gate::default());
    let g = gate.clone();
    h.set_hook(move |_, _| {
        let mut st = g.state.lock().unwrap();
        if !st.released && st.first.insert(std::thread::current().id()) {
            g.changed.notify_all();
            drop(st);
            drop(g.wait(|st| st.released));
            return;
        }
        if st.started {
            return;
        }
        st.early += 1;
        drop(st);
        let mut st = g.wait(|st| st.started);
        if !st.started {
            st.starved += 1;
        }
    });
    // Every file is tier 4 (no current text).
    h.publish("main", packages(&["app", "lib"]));
    {
        let st = gate.wait(|st| st.first.len() == threads);
        assert_eq!(
            st.first.len(),
            threads,
            "{threads}: every thread in a batch"
        );
    }
    let inner = h.indexer.inner().clone();
    let before = h.batches().len();
    h.clock.advance(BATCH_TIME + Duration::from_millis(1));
    let g = gate.clone();
    h.executor
        .spawn(Priority::Batch, async move {
            let ended = inner.batch_log.lock().unwrap().len();
            let mut st = g.state.lock().unwrap();
            st.started = true;
            st.ended_at_start = ended;
            g.changed.notify_all();
        })
        .detach();
    gate.state.lock().unwrap().released = true;
    gate.changed.notify_all();
    let oids: Vec<BlobId> = sources.iter().map(|s| h.git.oid(s)).collect();
    wait_until(&h, "every file stored", || {
        oids.iter().all(|o| stored(&h, *o))
    });
    let st = gate.state.lock().unwrap();
    assert!(st.started, "{threads}");
    assert_eq!(
        st.starved, 0,
        "{threads}: a batch went before the outside task"
    );
    // At least the batch that gave its thread to it ended, and at most one
    // batch on each thread.
    let ended = st.ended_at_start - before;
    assert!(
        (1..=threads).contains(&ended),
        "{threads}: {ended} batches ended"
    );
    assert!(
        h.batches().len() > before + threads,
        "{threads}: batches went on"
    );
}

#[test]
fn requeue_outside_task_starts_by_the_next_boundary_one_thread() {
    outside_task_starts_by_the_next_boundary(1);
}

#[test]
fn requeue_outside_task_starts_by_the_next_boundary_two_threads() {
    outside_task_starts_by_the_next_boundary(2);
}

/// One batch task stops a large file (over 256 KiB, `LargeSlot`) to yield
/// and goes back to the queue with it and its `LargeTurn`; with two
/// threads the other task waits in `LargeSlot::acquire` for its own large
/// file. The holder still gets a thread, and once the switch is off both
/// files are done, the parked one from where it stopped.
///
/// One thread (one task, so nobody waits in `acquire`): a task queued from
/// outside runs between the holder's boundary and its next poll, and
/// another while the holder waits for the switch to turn off.
///
/// Two threads: the holder's hook runs at its first yield check, holding
/// the slot. It waits until the other task waits in `acquire`, queues a
/// task from outside that occupies a thread (`occupy`), and turns the
/// switch on: the waiter gives up waiting, goes to the back of the queue
/// behind `occupy`, which takes its thread. The holder parks, goes back to
/// the queue and, on the one free thread, both register at the boundary,
/// the waiter first (a second task from outside yields a few times, going
/// behind both, so both have been polled when it ends). With the switch off the waiter runs first and waits in
/// `acquire` for the slot the parked holder keeps, on the one free thread;
/// the holder is in the queue. Once `occupy` ends the holder gets that
/// thread and finishes, then the waiter.
fn parked_large_item_requeued(threads: usize) {
    let h = threaded(threads);
    let kernel = h.host.kernel();
    let large = h.indexer.inner().large.clone();
    // First the project, done with one small file: tiers 4 and 5 have no
    // other work left (a keepalive slot would take work that fits it).
    let small = ts("small");
    h.git.add("main", &[("lib/small.ts", &small)]);
    h.git.commit("main");
    h.publish("main", packages(&["app", "lib"]));
    let small_oid = h.git.oid(&small);
    wait_until(&h, "the small file stored", || {
        stored(&h, small_oid) && idle(&h)
    });
    let big = [
        synthetic::source("ts", 21, 300 << 10),
        synthetic::source("ts", 22, 300 << 10),
    ];
    let oids = [h.git.oid(&big[0]), h.git.oid(&big[1])];
    let problems: Arc<Mutex<Vec<String>>> = Arc::default();
    let (hooked_tx, hooked_rx) = mpsc::channel::<()>();
    let (between_tx, between_rx) = mpsc::channel::<(bool, bool)>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let mut once = Some((
        h.executor.clone(),
        kernel.clone(),
        large.clone(),
        problems.clone(),
        hooked_tx,
        between_tx,
        go_rx,
    ));
    *h.indexer.inner().yield_hook.lock().unwrap() = Some(Box::new(move || {
        let Some((executor, kernel, large, problems, hooked, between, go)) = once.take() else {
            return;
        };
        if threads == 1 {
            // Runs once the holder is back in the queue, before its next
            // poll.
            executor
                .spawn(Priority::Batch, async move {
                    let _ = between.send((large.parked(), large.busy()));
                })
                .detach();
            kernel.set_yield(true);
        } else {
            if !large.wait_for_waits(1, LIMIT) {
                problems
                    .lock()
                    .unwrap()
                    .push("the other task never waited for the slot".into());
            }
            let (started_tx, started_rx) = mpsc::channel::<()>();
            // `occupy`: queued while both threads are busy.
            executor
                .spawn(Priority::Batch, async move {
                    let _ = started_tx.send(());
                    let _ = go.recv_timeout(LIMIT);
                })
                .detach();
            kernel.set_yield(true);
            // The waiter gave up waiting and went to the back of the queue.
            if started_rx.recv_timeout(LIMIT).is_err() {
                problems
                    .lock()
                    .unwrap()
                    .push("the occupying task never started".into());
            }
            drop(between);
        }
        let _ = hooked.send(());
    }));
    for (path, source) in [("lib/a_big.ts", &big[0]), ("lib/b_big.ts", &big[1])] {
        h.git.add("main", &[(path, source)]);
    }
    h.git.commit("main");
    let event = h.projects.set_files(&h.key(), h.git.file_list("main"));
    h.indexer.project_event(&event);
    hooked_rx
        .recv_timeout(LIMIT)
        .expect("a large file's extraction reached a yield check");
    assert_eq!(*problems.lock().unwrap(), Vec::<String>::new(), "{threads}");
    if threads == 1 {
        let (parked, busy) = between_rx
            .recv_timeout(LIMIT)
            .expect("the outside task ran");
        assert!(parked && busy, "{threads}: requeued with the parked item");
    }
    // A task from outside runs while the holder waits for the switch: it
    // doesn't keep a thread. It yields a few times, so with two threads
    // the waiter and the holder are both polled before it ends.
    let (done_tx, done_rx) = mpsc::channel::<()>();
    h.executor
        .spawn(Priority::Batch, async move {
            yield_times(4).await;
            let _ = done_tx.send(());
        })
        .detach();
    done_rx.recv_timeout(LIMIT).expect("the outside task ran");
    assert!(large.parked() && large.busy(), "{threads}: still parked");
    assert!(!oids.iter().any(|o| stored(&h, *o)), "{threads}");
    kernel.set_yield(false);
    if threads == 2 {
        assert!(
            large.wait_for_waits(2, LIMIT),
            "{threads}: the waiter runs first and waits for the slot"
        );
        // Both free threads are taken (the waiter, `occupy`): the holder
        // is in the queue, its state kept.
        assert!(large.parked(), "{threads}: the holder hasn't run");
        assert!(!oids.iter().any(|o| stored(&h, *o)), "{threads}");
        go_tx.send(()).expect("`occupy` is waiting");
    }
    wait_until(&h, "both large files stored", || {
        oids.iter().all(|o| stored(&h, *o))
    });
    // The holder's file went on from where it stopped; the other one ran
    // once, after the slot was free.
    let parked: Vec<BlobId> = oids
        .iter()
        .copied()
        .filter(|o| statuses(&h, *o).contains(&ExtractStatus::Parked))
        .collect();
    assert_eq!(parked.len(), 1, "{threads}: one holder");
    let other = *oids.iter().find(|o| **o != parked[0]).expect("two files");
    assert_eq!(
        statuses(&h, parked[0]),
        [ExtractStatus::Parked, ExtractStatus::Done],
        "{threads}"
    );
    assert_eq!(statuses(&h, other), [ExtractStatus::Done], "{threads}");
    let waits = if threads == 1 { 0 } else { 2 };
    assert_eq!(large.waits(), waits, "{threads}");
    assert!(!large.busy(), "{threads}: the slot is free");
    assert_eq!(*problems.lock().unwrap(), Vec::<String>::new(), "{threads}");
}

#[test]
fn requeue_parked_large_item_one_thread() {
    parked_large_item_requeued(1);
}

#[test]
fn requeue_parked_large_item_two_threads() {
    parked_large_item_requeued(2);
}
