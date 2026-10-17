//! The counting allocator, registered as this test binary's global
//! allocator (docs/design/bench.md「测试」).
//!
//! The counters are process-wide, and a test is not the only thread of its
//! process: libtest runs it on a spawned thread while the main thread keeps
//! its own bookkeeping (it records the running test after spawning the
//! thread, then waits on a channel), and those allocations can land between
//! two readings. Each check therefore retries a few times and passes on the
//! first attempt that saw exactly the expected bytes; a wrong count never
//! matches, so a broken allocator still fails every attempt.

use std::hint::black_box;
use std::sync::Mutex;
use std::sync::mpsc;

use lectern_bench::CountingAlloc;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// Tests in this binary take turns (`cargo test` runs them on parallel
/// threads; nextest runs each in its own process).
static TURN: Mutex<()> = Mutex::new(());

const MIB: usize = 1 << 20;

const ATTEMPTS: usize = 5;

fn delta(from: usize) -> isize {
    CountingAlloc::live() as isize - from as isize
}

/// Calls `check` until it returns `Ok`, at most `ATTEMPTS` times; returns how
/// many attempts it took. Panics with every attempt's report otherwise.
fn retried(mut check: impl FnMut() -> Result<(), String>) -> usize {
    let mut seen = Vec::new();
    for attempt in 1..=ATTEMPTS {
        match check() {
            Ok(()) => return attempt,
            Err(report) => seen.push(report),
        }
    }
    panic!("no clean attempt: {seen:?}");
}

/// Live bytes grow by exactly `expected` while `alloc`'s result is kept and
/// return to the starting value once it is dropped.
fn grows_and_returns<T>(expected: usize, mut alloc: impl FnMut() -> T) -> usize {
    retried(|| {
        let before = CountingAlloc::live();
        let kept = black_box(alloc());
        let held = delta(before);
        drop(kept);
        let after = delta(before);
        if (held, after) == (expected as isize, 0) {
            Ok(())
        } else {
            Err(format!("held {held:+}, after drop {after:+}"))
        }
    })
}

#[test]
fn alloc_live_grows_by_what_is_kept_and_returns_after_freeing() {
    let _turn = TURN.lock().unwrap();
    grows_and_returns(MIB, || vec![1u8; MIB]);
    // `vec![0; n]` goes through `alloc_zeroed`.
    grows_and_returns(MIB, || vec![0u8; MIB]);
}

#[test]
fn alloc_peak_records_the_highest_live_bytes() {
    let _turn = TURN.lock().unwrap();
    retried(|| {
        CountingAlloc::reset_peak();
        let base = CountingAlloc::live();
        if CountingAlloc::peak() != base {
            return Err("peak after reset_peak is not live".into());
        }
        drop(black_box(vec![0u8; 2 * MIB]));
        let second = black_box(vec![0u8; MIB]);
        let live = delta(base);
        let peak = CountingAlloc::peak() as isize - base as isize;
        drop(second);
        if live == MIB as isize && peak == 2 * MIB as isize {
            Ok(())
        } else {
            Err(format!("live {live:+}, peak {peak:+}"))
        }
    });
    CountingAlloc::reset_peak();
    assert_eq!(CountingAlloc::peak(), CountingAlloc::live());
}

#[test]
fn alloc_realloc_moves_the_count() {
    let _turn = TURN.lock().unwrap();
    retried(|| {
        let before = CountingAlloc::live();
        let mut v: Vec<u8> = Vec::with_capacity(MIB);
        v.reserve_exact(3 * MIB);
        let grown = (delta(before), v.capacity());
        v.shrink_to(MIB / 2);
        let shrunk = (delta(before), v.capacity());
        drop(v);
        let after = delta(before);
        if grown.0 == grown.1 as isize && shrunk.0 == shrunk.1 as isize && after == 0 {
            Ok(())
        } else {
            Err(format!(
                "grown {grown:?}, shrunk {shrunk:?}, after {after:+}"
            ))
        }
    });
}

/// The interleaving the retries exist for, made deterministic: another
/// thread allocates (and keeps) 4 KiB between the first attempt's readings.
/// That attempt is rejected and the second one passes.
#[test]
fn alloc_checks_retry_past_another_threads_allocation() {
    let _turn = TURN.lock().unwrap();
    let (go, wait) = mpsc::channel::<()>();
    // The other thread allocates once it is told to and hands the bytes back
    // through `join`, so it has exited (and freed its thread-locals) before
    // the second attempt starts.
    let mut other = Some(std::thread::spawn(move || {
        wait.recv().unwrap();
        black_box(vec![7u8; 4096])
    }));
    let mut foreign = None;
    let attempts = grows_and_returns(MIB, || {
        let kept = vec![1u8; MIB];
        if let Some(thread) = other.take() {
            go.send(()).unwrap();
            foreign = Some(thread.join().unwrap());
        }
        kept
    });
    assert_eq!(attempts, 2);
    assert_eq!(foreign.map(|f| f.len()), Some(4096));
}
