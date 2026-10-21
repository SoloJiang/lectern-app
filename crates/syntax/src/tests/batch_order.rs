//! `batch_order_*`: the engine's batch work on the executor's real batch
//! threads (one and three) beside simulated `code_intel` batch tasks, which
//! loop as `index/batch.rs` does: a batch of a set length, then
//! `yield_now()`, back of the first-in first-out `Batch` queue
//! (docs/design/file_reader.md「批量线程上的先后」). A prefetch starts within
//! each thread's batch and one window task; a window task parses for at most
//! 50 ms and takes turns with the batches; the batches go on while a 64 MiB
//! file's chain runs; a window in flight stops within a slice when the yield
//! switch turns on.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lectern_executor::{Executor, Priority};
use lectern_services::reader::Anchor;

use super::rig::Threaded;
use crate::limits::BATCH_SEGMENT;
use crate::util::yield_now;
use crate::{StepEnd, Trace, testing};

/// A simulated batch's length: shorter than code_intel's (about 350 ms) so
/// that the tests stay short; the order does not depend on it.
const BATCH: Duration = Duration::from_millis(30);

/// What a window step may take beyond its budget here: one query chunk,
/// and the scheduling of a test machine running other tests.
const STEP_SLACK: Duration = Duration::from_millis(100);

/// Simulated code_intel batch tasks: each logs its batches' start and end.
struct Sim {
    batches: Arc<Mutex<Vec<(usize, Instant, Instant)>>>,
    stop: Arc<AtomicBool>,
}

impl Sim {
    fn start(executor: &Executor, tasks: usize) -> Sim {
        let batches = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        for id in 0..tasks {
            let (batches, stop) = (batches.clone(), stop.clone());
            executor
                .spawn(Priority::Batch, async move {
                    while !stop.load(Ordering::SeqCst) {
                        let start = Instant::now();
                        while start.elapsed() < BATCH {
                            std::hint::spin_loop();
                        }
                        batches.lock().unwrap().push((id, start, Instant::now()));
                        yield_now().await;
                    }
                })
                .detach();
        }
        Sim { batches, stop }
    }

    fn batches(&self) -> Vec<(usize, Instant, Instant)> {
        self.batches.lock().unwrap().clone()
    }

    /// Waits until every task has run `n` batches.
    fn wait(&self, tasks: usize, n: usize) {
        let start = Instant::now();
        while (0..tasks).any(|id| self.batches().iter().filter(|b| b.0 == id).count() < n) {
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "simulated batches"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

impl Drop for Sim {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// The window steps of the log: start, end, budget and how it ended.
fn steps(events: &[(Instant, Trace)]) -> Vec<(Instant, Instant, Duration, StepEnd)> {
    let mut out = Vec::new();
    let mut open = None;
    for (at, event) in events {
        match event {
            Trace::ChainStep { budget, .. } => open = Some((*at, *budget)),
            Trace::ChainStepEnd { end, .. } => {
                if let Some((began, budget)) = open.take() {
                    out.push((began, *at, budget, *end));
                }
            }
            _ => {}
        }
    }
    out
}

/// A prefetch queued while every batch thread runs a simulated batch and a
/// chain's window tasks come and go starts before any simulated task ran a
/// second batch, and before a second window task ran: within each thread's
/// batch and one window task.
fn prefetch_starts_within_a_batch_and_a_window(threads: usize) {
    const ROUNDS: usize = 6;
    let t = Threaded::new(threads);
    t.write("big.ts", &testing::ts(8 << 20, 0));
    for round in 0..ROUNDS {
        t.write(&format!("p{round}.ts"), &testing::ts(16 << 10, round * 100));
    }
    let doc = t.open(t.target("big.ts", Anchor::Start));
    t.wait_for(0, |e| matches!(e, Trace::ChainStep { .. }));
    let sim = Sim::start(&t.executor, threads);
    sim.wait(threads, 2);
    for round in 0..ROUNDS {
        let from = t.events().len();
        let queued = Instant::now();
        t.engine.prefetch(vec![(
            t.target(&format!("p{round}.ts"), Anchor::Start),
            t.root.clone(),
        )]);
        let (_, started) = t.wait_for(from, |e| *e == Trace::PrefetchTask);
        let waited = started.duration_since(queued);
        // Batches run entirely between the two: the ones queued ahead.
        for id in 0..threads {
            let between = sim
                .batches()
                .iter()
                .filter(|(i, s, e)| *i == id && *s > queued && *e < started)
                .count();
            assert!(between <= 1, "round {round}, task {id}: {between} batches");
        }
        let windows = steps(&t.events()[from..])
            .iter()
            .filter(|(s, e, ..)| *s > queued && *e < started)
            .count();
        assert!(windows <= 1, "round {round}: {windows} window tasks");
        eprintln!("round {round}: the prefetch waited {waited:?}");
        assert!(
            waited < BATCH + BATCH_SEGMENT + Duration::from_secs(1),
            "{waited:?}"
        );
        t.wait_for(from, |e| matches!(e, Trace::PrefetchDone { .. }));
    }
    // The chain was in its course all along.
    let (_, done) = doc.chain().unwrap();
    assert!(!done, "the 8 MiB chain outlasts the rounds");
}

#[test]
fn batch_order_prefetch_starts_within_a_batch_and_a_window_on_one_thread() {
    prefetch_starts_within_a_batch_and_a_window(1);
}

#[test]
fn batch_order_prefetch_starts_within_a_batch_and_a_window_on_three_threads() {
    prefetch_starts_within_a_batch_and_a_window(3);
}

/// On one batch thread a window task parses for at most 50 ms and the
/// simulated batches run between two window tasks.
#[test]
fn batch_order_window_tasks_take_turns_with_batches() {
    let t = Threaded::new(1);
    t.write("big.ts", &testing::ts(8 << 20, 0));
    let sim = Sim::start(&t.executor, 1);
    let doc = t.open(t.target("big.ts", Anchor::Start));
    let (from, _) = t.wait_for(0, |e| matches!(e, Trace::ChainStep { .. }));
    let mut seen = 0;
    while seen < 12 {
        t.wait_for(from + seen + 1, |e| matches!(e, Trace::ChainStepEnd { .. }));
        seen = steps(&t.events()[from..]).len();
    }
    let steps = steps(&t.events()[from..]);
    let batches = sim.batches();
    for (i, (began, ended, budget, end)) in steps.iter().enumerate() {
        assert_eq!(*budget, BATCH_SEGMENT);
        let took = ended.duration_since(*began);
        assert!(
            took <= BATCH_SEGMENT + STEP_SLACK,
            "step {i}: {took:?} ({end:?})"
        );
        if let Some((next, ..)) = steps.get(i + 1) {
            let turns = batches
                .iter()
                .filter(|(_, s, _)| s > ended && s < next)
                .count();
            assert!(turns >= 1, "a batch between window steps {i} and {}", i + 1);
        }
    }
    drop(doc);
}

/// While a 64 MiB file's chain runs on the only batch thread, the
/// simulated task keeps its pace: a batch at least every batch and window
/// task.
#[test]
fn batch_order_batches_go_on_while_a_64mib_chain_runs() {
    let t = Threaded::new(1);
    t.write("huge.ts", &testing::ts(64 << 20, 0));
    let sim = Sim::start(&t.executor, 1);
    let doc = t.open(t.target("huge.ts", Anchor::Start));
    let (from, _) = t.wait_for(0, |e| matches!(e, Trace::ChainStep { .. }));
    let begin = Instant::now();
    let span = Duration::from_millis(1500);
    std::thread::sleep(span);
    let end = Instant::now();
    let batches = sim
        .batches()
        .iter()
        .filter(|(_, s, e)| *s >= begin && *e <= end)
        .count();
    let windows = steps(&t.events()[from..])
        .iter()
        .filter(|(s, e, ..)| *s >= begin && *e <= end)
        .count();
    // One batch and at most one window task each turn.
    let turn = BATCH + BATCH_SEGMENT + STEP_SLACK;
    let least = (span.as_millis() / turn.as_millis()) as usize;
    eprintln!("{batches} batches and {windows} window steps in {span:?}");
    assert!(batches >= least, "{batches} batches, at least {least}");
    assert!(windows >= least, "{windows} window steps, at least {least}");
    let (_, done) = doc.chain().unwrap();
    assert!(!done);
}

/// A batch window in flight stops within a slice once the yield switch is
/// on (measured as `code_intel/yield_stop` is: from the switch to the step
/// returning with its state kept), then moves only in keepalive slots, and
/// goes on once the switch is off.
#[test]
fn batch_order_window_stops_within_a_slice_when_yielding() {
    let t = Threaded::new(1);
    t.write("big.ts", &testing::ts(8 << 20, 0));
    let doc = t.open(t.target("big.ts", Anchor::Start));
    let (from, _) = t.wait_for(0, |e| matches!(e, Trace::ChainStep { .. }));
    // A step from here on is one in flight.
    let (step, _) = t.wait_for(from + 1, |e| matches!(e, Trace::ChainStep { .. }));
    std::thread::sleep(Duration::from_millis(3));
    let switched = Instant::now();
    t.host.kernel().set_yield(true);
    let (ended, at) = t.wait_for(step, |e| matches!(e, Trace::ChainStepEnd { .. }));
    // Zero when the step ended on its own before the switch.
    let stop = at.saturating_duration_since(switched);
    let how = t.events()[ended].1.clone();
    eprintln!("{how:?} {stop:?} after the switch");
    assert!(stop <= Duration::from_millis(10), "{how:?} {stop:?}");
    // The manual clock does not move: one keepalive slot at most, then
    // nothing until the switch is off.
    std::thread::sleep(Duration::from_millis(200));
    let during = steps(&t.events()[ended + 1..]).len();
    assert!(during <= 2, "{during} steps while yielding");
    let quiet = t.events().len();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(t.events().len(), quiet, "nothing moves between slots");
    t.host.kernel().set_yield(false);
    t.wait_for(quiet, |e| matches!(e, Trace::ChainStepEnd { .. }));
    drop(doc);
}
