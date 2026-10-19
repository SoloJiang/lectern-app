//! Interaction yield (ADR 0033): `Kernel::set_yield`, yielding guards and the
//! keepalive slots, on a `ManualClock`.
//!
//! The works here are resumable parses: each slice folds 4 KiB into a hash
//! and costs `SLICE` of clock time, and the work checks `yield_requested`
//! after every slice, as a parser's progress callback would. A single-threaded
//! driver polls the woken works and advances the clock by `TICK` only when
//! none is runnable, so every schedule is reproducible.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::thread;
use std::time::{Duration, Instant};

use super::support::*;
use crate::testing::ManualClock;
use crate::{
    BackgroundGuard, BackgroundState, Clock, Grant, Kernel, PluginSet, SLOT_INTERVAL, SLOT_LENGTH,
    SLOT_MAX_BYTES,
};

const SLICE: Duration = Duration::from_millis(10);
const SLICE_BYTES: usize = 4096;
const TICK: Duration = Duration::from_millis(10);

struct Setup {
    kernel: Arc<Kernel>,
    slot: HandleSlot,
    clock: ManualClock,
}

fn setup() -> Setup {
    let slot = HandleSlot::default();
    let kernel = start(PluginSet::new().with("p", handle_plugin(&slot, after_first_frame())));
    wait_until("quiescent", || kernel.is_quiescent());
    let clock = ManualClock::new();
    kernel.set_clock(Arc::new(clock.clone()));
    Setup {
        kernel,
        slot,
        clock,
    }
}

/// A file of `slices` slices, its bytes derived from `seed`.
fn file(seed: u8, slices: usize) -> Arc<[u8]> {
    (0..slices * SLICE_BYTES)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn fold(hash: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(hash, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The hash of a parse that never stopped.
fn reference(data: &[u8]) -> u64 {
    data.chunks(SLICE_BYTES).fold(0xcbf2_9ce4_8422_2325, fold)
}

/// What a work did, read by the test.
#[derive(Default)]
struct Record {
    /// Every grant, with the clock when it was received.
    grants: Vec<(Instant, Grant)>,
    /// Each run between two boundary checks: start and length.
    runs: Vec<(Instant, Duration)>,
    /// Slices done in each run.
    run_slices: Vec<usize>,
    slices: usize,
    results: Vec<u64>,
}

type Rec = Arc<Mutex<Record>>;
type Hook = Box<dyn FnMut(usize) + Send>;

/// A resumable parse of each file in turn. `hook` runs inside every slice
/// with the number of slices done so far, including this one.
fn work(
    guard: BackgroundGuard,
    clock: ManualClock,
    files: Vec<Arc<[u8]>>,
    rec: Rec,
    mut hook: Option<Hook>,
) -> Pin<Box<dyn Future<Output = ()> + Send>> {
    Box::pin(async move {
        for data in files {
            let mut pos = 0;
            let mut hash = 0xcbf2_9ce4_8422_2325;
            while pos < data.len() {
                let grant = guard.proceed_item(data.len() as u64).await;
                let start = clock.now();
                rec.lock().unwrap().grants.push((start, grant));
                let mut slices = 0;
                loop {
                    let end = (pos + SLICE_BYTES).min(data.len());
                    hash = fold(hash, &data[pos..end]);
                    pos = end;
                    clock.advance(SLICE);
                    slices += 1;
                    let done = {
                        let mut rec = rec.lock().unwrap();
                        rec.slices += 1;
                        rec.slices
                    };
                    if let Some(hook) = hook.as_mut() {
                        hook(done);
                    }
                    // The slice boundary: the pause abandons, the yield keeps
                    // `pos` and `hash`.
                    if pos == data.len() || guard.yield_requested() {
                        break;
                    }
                }
                let mut rec = rec.lock().unwrap();
                rec.runs.push((start, clock.now() - start));
                rec.run_slices.push(slices);
            }
            rec.lock().unwrap().results.push(hash);
        }
    })
}

#[derive(Default)]
struct Flag(AtomicBool);

impl Wake for Flag {
    fn wake(self: Arc<Self>) {
        self.0.store(true, SeqCst);
    }
}

struct Task {
    future: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
    woken: Arc<Flag>,
}

/// Polls woken works in order; advances the clock only when none is woken.
struct Driver {
    clock: ManualClock,
    tasks: Vec<Task>,
}

impl Driver {
    fn new(clock: &ManualClock) -> Driver {
        Driver {
            clock: clock.clone(),
            tasks: Vec::new(),
        }
    }

    fn add(&mut self, future: Pin<Box<dyn Future<Output = ()> + Send>>) {
        self.tasks.push(Task {
            future: Some(future),
            woken: Arc::new(Flag(AtomicBool::new(true))),
        });
    }

    fn done(&self, index: usize) -> bool {
        self.tasks[index].future.is_none()
    }

    /// Polls every woken work once; false if none was woken.
    fn poll_woken(&mut self) -> bool {
        let mut any = false;
        for task in &mut self.tasks {
            if task.future.is_none() || !task.woken.0.swap(false, SeqCst) {
                continue;
            }
            any = true;
            let waker = Waker::from(task.woken.clone());
            let future = task.future.as_mut().expect("checked");
            if future
                .as_mut()
                .poll(&mut Context::from_waker(&waker))
                .is_ready()
            {
                task.future = None;
            }
        }
        any
    }

    /// Runs until nothing is woken without advancing the clock.
    fn settle(&mut self) {
        while self.poll_woken() {}
    }

    /// Runs until the clock has moved `total` past now, idling in `TICK`s.
    fn run_for(&mut self, total: Duration) {
        let end = self.clock.now() + total;
        while self.clock.now() < end {
            if !self.poll_woken() {
                self.clock.advance(TICK);
            }
        }
    }

    /// Runs until `index` finishes, failing after `limit` of clock time.
    fn run_until_done(&mut self, index: usize, limit: Duration) {
        let end = self.clock.now() + limit;
        while !self.done(index) {
            assert!(self.clock.now() < end, "work {index} did not finish");
            if !self.poll_woken() {
                self.clock.advance(TICK);
            }
        }
    }
}

fn slot_grants(rec: &Rec) -> Vec<Instant> {
    rec.lock()
        .unwrap()
        .grants
        .iter()
        .filter_map(|(_, grant)| match grant {
            Grant::Slot { deadline } => Some(*deadline - SLOT_LENGTH),
            Grant::Batch => None,
        })
        .collect()
}

#[test]
fn an_in_flight_item_stops_at_a_slice_boundary_and_resumes_with_the_same_result() {
    let s = setup();
    let quiet = s.kernel.quiescent_epoch().expect("quiescent");
    let guard = s.slot.get().background_yielding().unwrap();
    assert!(guard.yields());
    let before = s.kernel.background();
    let data = file(1, 64);
    assert_eq!(data.len() as u64, SLOT_MAX_BYTES);
    let rec = Rec::default();
    let kernel = s.kernel.clone();
    // The switch turns on while slice 10 is running.
    let hook: Hook = Box::new(move |done| {
        if done == 10 {
            kernel.set_yield(true);
        }
    });
    let mut driver = Driver::new(&s.clock);
    driver.add(work(
        guard,
        s.clock.clone(),
        vec![data.clone()],
        rec.clone(),
        Some(hook),
    ));
    driver.settle();
    {
        let r = rec.lock().unwrap();
        // The first run ended at the boundary right after slice 10, and the
        // first slot started at once (none had started before).
        assert_eq!(r.run_slices[0], 10);
        assert_eq!(r.grants[0].1, Grant::Batch);
        assert!(matches!(r.grants[1].1, Grant::Slot { .. }));
    }
    // Waiting for a slot is not a stop.
    assert_eq!(s.kernel.background(), before);
    assert_eq!(s.kernel.background().state, BackgroundState::Running);
    driver.run_until_done(0, Duration::from_secs(10));

    let r = rec.lock().unwrap();
    assert_eq!(r.results, vec![reference(&data)]);
    assert_eq!(r.slices, 64);
    // After the first run every run was one slot of at most 50 ms.
    assert!(
        r.grants[1..]
            .iter()
            .all(|(_, grant)| matches!(grant, Grant::Slot { .. }))
    );
    assert!(r.runs[1..].iter().all(|(_, length)| *length <= SLOT_LENGTH));
    let expected: Vec<usize> = std::iter::repeat_n(5, 10).chain([4]).collect();
    assert_eq!(r.run_slices[1..], expected[..]);
    drop(r);
    // The finished work dropped its guard: one end, no other change.
    assert_eq!(s.kernel.background().epoch, before.epoch + 1);
    assert_eq!(s.kernel.background().state, BackgroundState::Idle);
    assert_eq!(s.kernel.quiescent_epoch(), Some(quiet));
}

#[test]
fn guards_that_do_not_yield_are_unaffected() {
    let s = setup();
    s.kernel.set_yield(true);
    let tier3 = s.slot.get().background().unwrap();
    assert!(!tier3.yields());
    assert!(!tier3.yield_requested());
    let waker = Waker::from(Arc::new(Flag::default()));
    let mut cx = Context::from_waker(&waker);
    assert_eq!(tier3.poll_proceed(&mut cx), Poll::Ready(()));
    assert_eq!(
        tier3.poll_proceed_item(&mut cx, 1),
        Poll::Ready(Grant::Batch)
    );
    assert_eq!(
        tier3.poll_proceed_item(&mut cx, SLOT_MAX_BYTES * 4),
        Poll::Ready(Grant::Batch)
    );

    // Beside two yielding works, the tier-3 work runs each file in one go.
    let files = vec![file(2, 30), file(3, 30), file(4, 80)];
    let mut driver = Driver::new(&s.clock);
    let tier3_rec = Rec::default();
    driver.add(work(
        tier3,
        s.clock.clone(),
        files.clone(),
        tier3_rec.clone(),
        None,
    ));
    for seed in 5..7 {
        let guard = s.slot.get().background_yielding().unwrap();
        driver.add(work(
            guard,
            s.clock.clone(),
            vec![file(seed, 30)],
            Rec::default(),
            None,
        ));
    }
    let start = s.clock.now();
    driver.poll_woken();
    assert!(driver.done(0), "the tier-3 work finished in its first poll");
    let r = tier3_rec.lock().unwrap();
    assert!(r.grants.iter().all(|(_, grant)| *grant == Grant::Batch));
    assert_eq!(r.run_slices, vec![30, 30, 80]);
    assert_eq!(
        r.results,
        files.iter().map(|f| reference(f)).collect::<Vec<_>>()
    );
    // Only its own slices and the yielding works' first slot took clock time.
    assert_eq!(s.clock.now() - start, SLICE * (140 + 5));
}

#[test]
fn three_yielding_works_run_at_most_500_ms_in_5_s() {
    let s = setup();
    s.kernel.set_yield(true);
    let mut driver = Driver::new(&s.clock);
    let recs: Vec<Rec> = (0..3).map(|_| Rec::default()).collect();
    for (seed, rec) in recs.iter().enumerate() {
        let guard = s.slot.get().background_yielding().unwrap();
        let files = (0..20).map(|n| file(seed as u8 * 20 + n, 20)).collect();
        driver.add(work(guard, s.clock.clone(), files, rec.clone(), None));
    }
    driver.run_for(Duration::from_secs(5));

    let runs: Vec<(Instant, Duration)> = recs
        .iter()
        .flat_map(|rec| rec.lock().unwrap().runs.clone())
        .collect();
    let total: Duration = runs.iter().map(|(_, length)| *length).sum();
    assert!(total <= Duration::from_millis(500), "ran {total:?}");
    assert!(runs.iter().all(|(_, length)| *length <= SLOT_LENGTH));
    // Every run was in a slot; one slot at a time, starts 500 ms apart.
    let mut starts: Vec<Instant> = recs.iter().flat_map(slot_grants).collect();
    assert_eq!(starts.len(), runs.len());
    starts.sort();
    assert_eq!(starts.len(), 10);
    for pair in starts.windows(2) {
        assert!(pair[1] - pair[0] >= SLOT_INTERVAL);
    }
    let mut ordered = runs.clone();
    ordered.sort();
    for pair in ordered.windows(2) {
        assert!(pair[0].0 + pair[0].1 <= pair[1].0, "two runs overlap");
    }
    // First come, first served: every work moved.
    for rec in &recs {
        assert!(rec.lock().unwrap().runs.len() >= 3);
    }
}

#[test]
fn items_over_256_kib_never_enter_a_slot() {
    let s = setup();
    let large = file(7, 75);
    assert!(large.len() as u64 > SLOT_MAX_BYTES);
    let large_rec = Rec::default();
    let kernel = s.kernel.clone();
    // The large item is in flight when the switch turns on.
    let hook: Hook = Box::new(move |done| {
        if done == 5 {
            kernel.set_yield(true);
        }
    });
    let mut driver = Driver::new(&s.clock);
    let guard = s.slot.get().background_yielding().unwrap();
    driver.add(work(
        guard,
        s.clock.clone(),
        vec![large.clone()],
        large_rec.clone(),
        Some(hook),
    ));
    let small_rec = Rec::default();
    let guard = s.slot.get().background_yielding().unwrap();
    let small = (0..40).map(|n| file(100 + n, 8)).collect();
    driver.add(work(guard, s.clock.clone(), small, small_rec.clone(), None));
    driver.run_for(Duration::from_secs(5));

    {
        let r = large_rec.lock().unwrap();
        assert_eq!(r.slices, 5);
        assert_eq!(r.grants.len(), 1);
        assert_eq!(r.grants[0].1, Grant::Batch);
    }
    // The slots went to the small work meanwhile.
    assert!(slot_grants(&small_rec).len() >= 9);
    assert!(slot_grants(&large_rec).is_empty());

    s.kernel.set_yield(false);
    driver.run_until_done(0, Duration::from_secs(10));
    let r = large_rec.lock().unwrap();
    assert_eq!(r.grants[1].1, Grant::Batch);
    assert_eq!(r.run_slices, vec![5, 70]);
    assert_eq!(r.results, vec![reference(&large)]);
}

#[test]
fn turning_the_switch_off_resumes_everything() {
    let s = setup();
    let quiet = s.kernel.quiescent_epoch().expect("quiescent");
    let epoch = s.kernel.background().epoch;
    let mut driver = Driver::new(&s.clock);
    let mut files: Vec<Vec<Arc<[u8]>>> = Vec::new();
    let mut recs: Vec<Rec> = Vec::new();
    let mut hook: Option<Hook> = {
        let kernel = s.kernel.clone();
        Some(Box::new(move |done| {
            if done == 3 {
                kernel.set_yield(true);
            }
        }))
    };
    for (index, slices) in [12, 12, 70].into_iter().enumerate() {
        let guard = s.slot.get().background_yielding().unwrap();
        let list: Vec<Arc<[u8]>> = (0..3).map(|n| file(index as u8 * 3 + n, slices)).collect();
        let rec = Rec::default();
        driver.add(work(
            guard,
            s.clock.clone(),
            list.clone(),
            rec.clone(),
            hook.take(),
        ));
        files.push(list);
        recs.push(rec);
    }
    driver.run_for(Duration::from_secs(2));
    assert!((0..3).all(|index| !driver.done(index)));
    let slots_before: usize = recs.iter().map(|rec| slot_grants(rec).len()).sum();
    assert!(slots_before >= 4);

    s.kernel.set_yield(false);
    // Every waiting work was woken and runs to the end without the clock
    // idling: nothing waits for a slot any more.
    driver.settle();
    assert!((0..3).all(|index| driver.done(index)));
    for (rec, list) in recs.iter().zip(&files) {
        let r = rec.lock().unwrap();
        assert_eq!(
            r.results,
            list.iter().map(|f| reference(f)).collect::<Vec<_>>()
        );
    }
    let slots_after: usize = recs.iter().map(|rec| slot_grants(rec).len()).sum();
    assert_eq!(slots_after, slots_before);
    // Every slice ran exactly once: the stopped items continued.
    let total: usize = recs.iter().map(|rec| rec.lock().unwrap().slices).sum();
    assert_eq!(total, 3 * 12 + 3 * 12 + 3 * 70);
    // Neither the switch nor the waiting changed the background epoch beyond
    // the three starts and three ends, nor quiescence.
    assert_eq!(s.kernel.background().epoch, epoch + 6);
    assert_eq!(s.kernel.background().state, BackgroundState::Idle);
    assert_eq!(s.kernel.quiescent_epoch(), Some(quiet));
}

#[test]
fn the_pause_wins_over_the_yield_and_its_state_is_unchanged() {
    let s = setup();
    s.kernel.set_yield(true);
    let guards: Vec<BackgroundGuard> = (0..3)
        .map(|_| s.slot.get().background_yielding().unwrap())
        .collect();
    let flags: Vec<Arc<Flag>> = (0..3).map(|_| Arc::new(Flag::default())).collect();
    let poll = |index: usize, bytes: u64| {
        let waker = Waker::from(flags[index].clone());
        guards[index].poll_proceed_item(&mut Context::from_waker(&waker), bytes)
    };
    // One holds the slot, one waits for the next, one is too large.
    assert!(matches!(poll(0, 1), Poll::Ready(Grant::Slot { .. })));
    assert!(!guards[0].pause_requested());
    assert!(!guards[0].yield_requested());
    assert!(guards[1].yield_requested());
    assert_eq!(poll(1, 1), Poll::Pending);
    assert_eq!(poll(2, SLOT_MAX_BYTES + 1), Poll::Pending);
    let running = s.kernel.background();
    assert_eq!(running.state, BackgroundState::Running);

    s.kernel.pause();
    assert!(guards[1].pause_requested() && guards[2].pause_requested());
    // The waiters were woken to see the pause; the holder sees it in its
    // progress callback and comes back to the boundary.
    assert!(flags[1].0.swap(false, SeqCst) && flags[2].0.swap(false, SeqCst));
    for index in 0..3 {
        assert_eq!(poll(index, 1), Poll::Pending);
    }
    let paused = s.kernel.background();
    assert_eq!(paused.state, BackgroundState::Paused);
    assert_eq!(paused.epoch, running.epoch + 3);

    // Resumed while still yielding: the slot is free but the next start is
    // 500 ms after the first one.
    s.kernel.resume();
    assert_eq!(s.kernel.background().state, BackgroundState::Running);
    for flag in &flags {
        flag.0.store(false, SeqCst);
    }
    for index in 0..3 {
        assert_eq!(poll(index, 1), Poll::Pending);
    }
    s.clock.advance(SLOT_INTERVAL);
    // The first waiter in line was picked and woken; it takes the slot.
    assert!(flags[0].0.swap(false, SeqCst));
    assert!(matches!(poll(0, 1), Poll::Ready(Grant::Slot { .. })));
    assert!(!flags[1].0.load(SeqCst) && !flags[2].0.load(SeqCst));

    s.kernel.set_yield(false);
    assert!(flags[1].0.load(SeqCst) && flags[2].0.load(SeqCst));
    for (index, guard) in guards.iter().enumerate() {
        assert_eq!(poll(index, SLOT_MAX_BYTES * 2), Poll::Ready(Grant::Batch));
        assert!(!guard.yield_requested());
    }
    // The yield switch changed nothing the pause reports.
    assert_eq!(s.kernel.background().epoch, paused.epoch + 3);
    drop(guards);
    assert_eq!(s.kernel.background().state, BackgroundState::Idle);
    assert_eq!(s.clock.pending_timers(), 0);
}

#[test]
fn a_dropped_holder_or_waiter_hands_the_slot_on() {
    let s = setup();
    s.kernel.set_yield(true);
    let flags: Vec<Arc<Flag>> = (0..3).map(|_| Arc::new(Flag::default())).collect();
    let guards: Vec<BackgroundGuard> = (0..3)
        .map(|_| s.slot.get().background_yielding().unwrap())
        .collect();
    let poll = |guard: &BackgroundGuard, flag: &Arc<Flag>| {
        let waker = Waker::from(flag.clone());
        guard.poll_proceed_item(&mut Context::from_waker(&waker), 1)
    };
    let mut guards = guards.into_iter();
    let (a, b, c) = (
        guards.next().unwrap(),
        guards.next().unwrap(),
        guards.next().unwrap(),
    );
    assert!(matches!(
        poll(&a, &flags[0]),
        Poll::Ready(Grant::Slot { .. })
    ));
    assert_eq!(poll(&b, &flags[1]), Poll::Pending);
    assert_eq!(poll(&c, &flags[2]), Poll::Pending);
    // Dropping the holder frees the slot; the next one is still 500 ms after
    // the first start. Then `b` is picked, and dropping it hands on to `c`.
    drop(a);
    assert!(!flags[1].0.load(SeqCst));
    s.clock.advance(SLOT_INTERVAL);
    assert!(flags[1].0.swap(false, SeqCst));
    drop(b);
    assert!(flags[2].0.swap(false, SeqCst));
    assert!(matches!(
        poll(&c, &flags[2]),
        Poll::Ready(Grant::Slot { .. })
    ));
    // The holder's slot ends at its deadline.
    assert!(!c.yield_requested());
    s.clock.advance(SLOT_LENGTH);
    assert!(c.yield_requested());
}

/// Blocks the thread on `future`, parking until woken.
fn block_on<F: Future>(future: F) -> F::Output {
    struct Unpark(thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(Unpark(thread::current())));
    let mut future = std::pin::pin!(future);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut Context::from_waker(&waker)) {
            return output;
        }
        thread::park();
    }
}

#[test]
fn one_slot_at_a_time_under_concurrent_yielding_work() {
    const THREADS: usize = 4;
    const ITEMS: usize = 40;
    let s = setup();
    let in_slot = Arc::new(AtomicUsize::new(0));
    let finished = Arc::new(AtomicUsize::new(0));
    let starts = Arc::new(Mutex::new(Vec::new()));
    let workers: Vec<_> = (0..THREADS)
        .map(|index| {
            let guard = s.slot.get().background_yielding().unwrap();
            let (in_slot, finished, starts) = (in_slot.clone(), finished.clone(), starts.clone());
            let clock = s.clock.clone();
            thread::spawn(move || {
                for item in 0..ITEMS {
                    // Every fourth item of thread 0 is too large for a slot.
                    let bytes = if index == 0 && item % 4 == 0 {
                        SLOT_MAX_BYTES + 1
                    } else {
                        1024
                    };
                    if let Grant::Slot { deadline } = block_on(guard.proceed_item(bytes)) {
                        assert!(bytes <= SLOT_MAX_BYTES);
                        assert_eq!(in_slot.fetch_add(1, SeqCst), 0, "two slots at once");
                        starts.lock().unwrap().push(deadline - SLOT_LENGTH);
                        for _ in 0..8 {
                            if guard.yield_requested() || clock.now() >= deadline {
                                break;
                            }
                            thread::yield_now();
                        }
                        in_slot.fetch_sub(1, SeqCst);
                    }
                }
                finished.fetch_add(1, SeqCst);
            })
        })
        .collect();

    // Advance the clock and flip the switches until about half the work is
    // done, then let the rest run as usual.
    let mut step = 0usize;
    while finished.load(SeqCst) < THREADS / 2 {
        step += 1;
        assert!(step < 2_000_000, "workers stalled");
        if step.is_multiple_of(37) {
            s.kernel.set_yield(!step.is_multiple_of(74));
        }
        if step.is_multiple_of(101) {
            s.kernel.pause();
        }
        if step % 101 == 7 {
            s.kernel.resume();
        }
        s.clock.advance(TICK);
        thread::yield_now();
    }
    s.kernel.resume();
    s.kernel.set_yield(false);
    for worker in workers {
        worker.join().expect("worker");
    }
    let mut starts = starts.lock().unwrap().clone();
    starts.sort();
    for pair in starts.windows(2) {
        assert!(pair[1] - pair[0] >= SLOT_INTERVAL);
    }
    assert_eq!(s.kernel.background().state, BackgroundState::Idle);
}

#[test]
fn the_system_clock_wakes_at_the_instant_and_not_before() {
    /// Sends its instant and the time it was woken.
    struct Report(Instant, Mutex<std::sync::mpsc::Sender<(Instant, Instant)>>);
    impl Wake for Report {
        fn wake(self: Arc<Self>) {
            let _ = self.1.lock().unwrap().send((self.0, Instant::now()));
        }
    }
    let clock = crate::SystemClock;
    let (tx, rx) = std::sync::mpsc::channel();
    let start = clock.now();
    // Out of order, and one already due.
    let ats = [
        start + Duration::from_millis(30),
        start + Duration::from_millis(10),
        start,
    ];
    for at in ats {
        let waker = Waker::from(Arc::new(Report(at, Mutex::new(tx.clone()))));
        clock.wake_at(at, waker);
    }
    // Waits for the wakes themselves; the timeout only keeps a hang short.
    let mut woken: Vec<Instant> = Vec::new();
    for _ in 0..3 {
        let (at, when) = rx.recv_timeout(Duration::from_secs(10)).expect("woken");
        assert!(when >= at, "woken before its instant");
        woken.push(at);
    }
    woken.sort();
    let mut ats = ats.to_vec();
    ats.sort();
    assert_eq!(woken, ats, "each timer woke once");
}
