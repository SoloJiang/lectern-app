//! `batch_order_*`: the engine's batch work on the executor's real batch
//! threads (one and three) beside simulated `code_intel` batch tasks, which
//! loop as `index/batch.rs` does: a batch of a set length, then
//! `yield_now()`, back of the first-in first-out `Batch` queue
//! (docs/design/file_reader.md「批量线程上的先后」). A prefetch starts within
//! each thread's batch and one window task; a window task parses for at most
//! 50 ms and takes turns with the batches; the batches go on while a 64 MiB
//! file's chain runs; a window in flight stops within a slice when the yield
//! switch turns on. What a step parsed for, and how long a step went on
//! after the switch, are the batch thread's CPU time (`rig::ran`): a
//! machine busy with other tests keeps the thread waiting for a CPU, and
//! that wait is not parsing.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lectern_executor::{Executor, Priority};
use lectern_kernel::SLOT_INTERVAL;
use lectern_services::reader::Anchor;

use super::rig::{FLIP_AT, Threaded, ran};
use crate::doc::Probe;
use crate::limits::BATCH_SEGMENT;
use crate::util::yield_now;
use crate::{StepEnd, Trace, testing};

/// A simulated batch's length: shorter than code_intel's (about 350 ms) so
/// that the tests stay short; the order does not depend on it.
const BATCH: Duration = Duration::from_millis(30);

/// What a window step may take beyond its budget here: one query chunk,
/// the checks between tree-sitter's progress callbacks, and the trace hook.
const STEP_SLACK: Duration = Duration::from_millis(100);

/// The wall time a window step stays under however busy the machine is:
/// a step does not wait on anything, so only one that blocks reaches it.
const STEP_WALL_LIMIT: Duration = Duration::from_secs(2);

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

/// The window steps of the log: start, end, budget, how it ended, and the
/// indices in `events` of its two events.
fn steps(events: &[(Instant, Trace)]) -> Vec<(Instant, Instant, Duration, StepEnd, Range<usize>)> {
    let mut out = Vec::new();
    let mut open = None;
    for (i, (at, event)) in events.iter().enumerate() {
        match event {
            Trace::ChainStep { budget, .. } => open = Some((i, *at, *budget)),
            Trace::ChainStepEnd { end, .. } => {
                if let Some((first, began, budget)) = open.take() {
                    out.push((began, *at, budget, *end, first..i));
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

/// On one batch thread a window task parses for at most 50 ms (the batch
/// thread's CPU time) and the simulated batches run between two window
/// tasks.
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
    let cpu = &t.cpu()[from..];
    let batches = sim.batches();
    for (i, (began, ended, budget, end, at)) in steps.iter().enumerate() {
        assert_eq!(*budget, BATCH_SEGMENT);
        // Over at its budget or at its window's end: nothing else stops it.
        assert!(
            matches!(end, StepEnd::TimedOut | StepEnd::Done),
            "step {i}: {end:?}"
        );
        let wall = ended.duration_since(*began);
        let took = ran((*began, cpu[at.start]), (*ended, cpu[at.end]));
        eprintln!("step {i}: parsed {took:?}, {wall:?} on the clock ({end:?})");
        assert!(
            took <= BATCH_SEGMENT + STEP_SLACK,
            "step {i}: {took:?} ({end:?}), {wall:?} on the clock"
        );
        assert!(wall < STEP_WALL_LIMIT, "step {i}: {wall:?} ({end:?})");
        if let Some((next, ..)) = steps.get(i + 1) {
            // One batch thread: the steps and the batches run one after
            // another on it, so their times are in that order.
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
/// simulated task keeps its pace and so does the chain: they take turns,
/// one batch between every two window steps. The pace is counted in turns,
/// not in wall time: a turn is a 30 ms batch and a 50 ms step only while
/// the thread has a CPU. On a machine busy with other tests, a step runs on
/// past its budget to its next check, and the work between steps (taking
/// the window, reading it from the file, landing its spans) waits for a CPU
/// too, so a turn can take many times that on the clock.
#[test]
fn batch_order_batches_go_on_while_a_64mib_chain_runs() {
    const STEPS: usize = 16;
    let t = Threaded::new(1);
    t.write("huge.ts", &testing::ts(64 << 20, 0));
    let sim = Sim::start(&t.executor, 1);
    let doc = t.open(t.target("huge.ts", Anchor::Start));
    let (from, _) = t.wait_for(0, |e| matches!(e, Trace::ChainStep { .. }));
    let mut to = from;
    for _ in 0..STEPS {
        let (end, _) = t.wait_for(to, |e| matches!(e, Trace::ChainStepEnd { .. }));
        to = end + 1;
    }
    let steps = steps(&t.events()[from..to]);
    assert_eq!(steps.len(), STEPS);
    // One batch thread: the steps and the batches run one after another on
    // it, so a batch between two steps' times ran between them. The queue
    // is first in, first out, and each task goes to its end after a batch
    // or a step, so the two take turns.
    let batches = sim.batches();
    let turns: Vec<usize> = steps
        .windows(2)
        .map(|pair| {
            let (ended, next) = (pair[0].1, pair[1].0);
            batches
                .iter()
                .filter(|(_, s, e)| *s > ended && *e < next)
                .count()
        })
        .collect();
    // What the turns took on the clock, and how much of it the batch thread
    // ran (its CPU time, the batches' included): on a busy machine the
    // first is many times the second.
    let cpu = &t.cpu()[from..to];
    let (first, last) = (&steps[0], &steps[STEPS - 1]);
    let took = last.1.duration_since(first.0);
    let busy = ran((first.0, cpu[first.4.start]), (last.1, cpu[last.4.end]));
    let slowest = steps
        .windows(2)
        .map(|pair| pair[1].0.duration_since(pair[0].0))
        .max()
        .unwrap_or_default();
    eprintln!(
        "batches between the {STEPS} window steps: {turns:?}; {took:?} on the \
         clock, the batch thread ran {busy:?}; the slowest turn {slowest:?}"
    );
    for (i, turns) in turns.iter().enumerate() {
        assert_eq!(*turns, 1, "batches between window steps {i} and {}", i + 1);
    }
    let (_, done) = doc.chain().unwrap();
    assert!(!done);
}

/// A batch window in flight stops within a slice once the yield switch is
/// on (measured as `code_intel/yield_stop` is: from the switch to the step
/// returning with its state kept), then moves only in keepalive slots, and
/// goes on once the switch is off. The switch turns on from inside a step,
/// at a slice boundary (`Threaded::flip_inside`), so the step is in flight
/// by construction, however fast the machine parses.
///
/// On a busy machine the step can run out of its 50 ms of wall time in the
/// very check the switch turned on in: it ends `TimedOut` with no ask after
/// the switch, stopped by its budget and not by the switch, and its window
/// goes on in a keepalive slot. Then the switch is turned off (the clock
/// moved past the slot interval, so the next slot can start at once) and
/// turned on again in a later step, at most 20 times; any other end of the
/// step fails.
#[test]
fn batch_order_window_stops_within_a_slice_when_yielding() {
    const ATTEMPTS: usize = 20;
    let t = Threaded::new(1);
    t.write("big.ts", &testing::ts(8 << 20, 0));
    let flip = t.flip_inside(FLIP_AT, |p| matches!(p, Probe::Chain { .. }));
    let doc = t.open(t.target("big.ts", Anchor::Start));
    let mut attempts = 0;
    let (ended, at, on, switched, on_cpu) = loop {
        attempts += 1;
        assert!(
            attempts <= ATTEMPTS,
            "the step never stopped for the switch"
        );
        t.wait_until("the switch", || flip.lock().unwrap().on.is_some());
        let (on, switched, on_cpu, event) = {
            let flip = flip.lock().unwrap();
            let (on, switched) = flip.on.expect("turned on");
            (on, switched, flip.on_cpu, flip.event)
        };
        let Probe::Chain { start, .. } = on else {
            panic!("{on:?}");
        };
        // The step it was turned on in: the next step end, and its step
        // began before the switch. No step stops for the switch before it
        // is on.
        let (ended, at) = t.wait_for(event, |e| matches!(e, Trace::ChainStepEnd { .. }));
        let events = t.events();
        assert!(
            !events[..event].iter().any(|(_, e)| matches!(
                e,
                Trace::ChainStepEnd {
                    end: StepEnd::Yielded,
                    ..
                }
            )),
            "stopped before the switch"
        );
        let (began, ref step) = events[ended - 1];
        assert!(
            matches!(step, Trace::ChainStep { start: s, .. } if *s == start),
            "{step:?}"
        );
        assert!(began <= switched && switched <= at);
        let Trace::ChainStepEnd { start: s, end } = events[ended].1 else {
            unreachable!("matched above");
        };
        assert_eq!(s, start);
        if end == StepEnd::Yielded {
            break (ended, at, on, switched, on_cpu);
        }
        // Over at its budget in the check the switch turned on in: no ask
        // of the step after it, and more than the budget on the clock.
        let after = flip.lock().unwrap().after.clone();
        let took = at.duration_since(began);
        assert!(
            end == StepEnd::TimedOut && after.is_empty() && took > BATCH_SEGMENT,
            "attempt {attempts}: {end:?} after {took:?}, asked {after:?} after the switch"
        );
        eprintln!("attempt {attempts}: the step ran out of its budget at the switch ({took:?})");
        // Its window goes on in a keepalive slot, then waits for the next.
        t.wait_until("the wait for the next slot", || {
            t.clock.pending_timers() == 1
        });
        t.host.kernel().set_yield(false);
        t.clock.advance(SLOT_INTERVAL);
        flip.lock().unwrap().rearm();
    };
    let Probe::Chain { start, asked } = on else {
        panic!("{on:?}");
    };
    // Stopped at the next slice boundary: the step asked once more.
    let after = flip.lock().unwrap().after.clone();
    assert_eq!(
        after,
        vec![Probe::Chain {
            start,
            asked: asked + 1
        }]
    );
    // The switch and the step's end are both on the batch thread.
    let wall = at.duration_since(switched);
    let stop = ran((switched, on_cpu), (at, t.cpu()[ended]));
    eprintln!("stopped {stop:?} after the switch ({wall:?} on the clock)");
    assert!(
        stop <= Duration::from_millis(10),
        "{stop:?}, {wall:?} on the clock"
    );
    assert!(wall < STEP_WALL_LIMIT, "{wall:?}");
    // The manual clock does not move: the window goes on in one keepalive
    // slot, then the task waits for the next one, its timer pending on the
    // clock, and nothing moves.
    t.wait_until("the wait for the next slot", || {
        t.clock.pending_timers() == 1
    });
    let during = steps(&t.events()[ended + 1..]);
    assert_eq!(during.len(), 1, "one step in the slot: {during:?}");
    let in_flight = |events: &[(Instant, Trace)]| {
        events
            .iter()
            .filter(|(_, e)| matches!(e, Trace::ChainStep { .. }))
            .count()
    };
    assert_eq!(
        in_flight(&t.events()[ended + 1..]),
        1,
        "no step begun after it"
    );
    // The next slot: one more step, then the wait again.
    let quiet = t.events().len();
    t.clock.advance(SLOT_INTERVAL);
    t.wait_until("the wait after the second slot", || {
        t.clock.pending_timers() == 1
    });
    assert_eq!(steps(&t.events()[quiet..]).len(), 1);
    assert_eq!(in_flight(&t.events()[quiet..]), 1);
    let quiet = t.events().len();
    t.host.kernel().set_yield(false);
    t.wait_for(quiet, |e| matches!(e, Trace::ChainStepEnd { .. }));
    drop(doc);
}
