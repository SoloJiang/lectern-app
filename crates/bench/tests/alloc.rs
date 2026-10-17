//! The counting allocator, registered as this test binary's global
//! allocator (docs/design/bench.md「测试」).

use std::hint::black_box;
use std::sync::Mutex;

use lectern_bench::CountingAlloc;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// The counters are process-wide: tests in this binary take turns.
static TURN: Mutex<()> = Mutex::new(());

const MIB: usize = 1 << 20;

#[test]
fn alloc_live_grows_by_what_is_kept_and_returns_after_freeing() {
    let _turn = TURN.lock().unwrap();
    let before = CountingAlloc::live();
    let kept = black_box(vec![1u8; MIB]);
    assert_eq!(CountingAlloc::live(), before + MIB);
    drop(kept);
    assert_eq!(CountingAlloc::live(), before);
}

#[test]
fn alloc_peak_records_the_highest_live_bytes() {
    let _turn = TURN.lock().unwrap();
    CountingAlloc::reset_peak();
    let base = CountingAlloc::live();
    assert_eq!(CountingAlloc::peak(), base);
    let first = black_box(vec![0u8; 2 * MIB]);
    drop(first);
    let second = black_box(vec![0u8; MIB]);
    assert_eq!(CountingAlloc::live(), base + MIB);
    assert!(CountingAlloc::peak() >= base + 2 * MIB);
    assert!(CountingAlloc::peak() < base + 3 * MIB);
    drop(second);
    CountingAlloc::reset_peak();
    assert_eq!(CountingAlloc::peak(), CountingAlloc::live());
}

#[test]
fn alloc_realloc_moves_the_count() {
    let _turn = TURN.lock().unwrap();
    let before = CountingAlloc::live();
    let mut v: Vec<u8> = Vec::with_capacity(MIB);
    v.reserve_exact(3 * MIB);
    assert_eq!(CountingAlloc::live(), before + v.capacity());
    v.shrink_to(MIB / 2);
    assert_eq!(CountingAlloc::live(), before + v.capacity());
    drop(v);
    assert_eq!(CountingAlloc::live(), before);
}
