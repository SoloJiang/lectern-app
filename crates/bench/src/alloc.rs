//! The counting allocator of the `memory` target: live bytes and their peak.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static CALLS: AtomicUsize = AtomicUsize::new(0);

/// The global allocator of the `memory` benchmark target: wraps `System`
/// and counts live bytes and their peak in two atomics. Registered only
/// there (`#[global_allocator]`), so `time` measurements never go through
/// it. Without registration the counters stay at 0.
pub struct CountingAlloc;

impl CountingAlloc {
    /// Bytes allocated and not yet freed.
    pub fn live() -> usize {
        LIVE.load(Ordering::Relaxed)
    }

    /// The highest `live` since start or the last [`reset_peak`](Self::reset_peak).
    pub fn peak() -> usize {
        PEAK.load(Ordering::Relaxed)
    }

    /// Allocations so far (`alloc`, `alloc_zeroed` and `realloc` calls).
    pub fn allocations() -> usize {
        CALLS.load(Ordering::Relaxed)
    }

    /// Sets the peak to the current live bytes.
    pub fn reset_peak() {
        PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
    }
}

fn grow(bytes: usize) {
    CALLS.fetch_add(1, Ordering::Relaxed);
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

fn shrink(bytes: usize) {
    LIVE.fetch_sub(bytes, Ordering::Relaxed);
}

// SAFETY: every method forwards to `System` with the caller's arguments
// unchanged and returns its result; the bookkeeping only touches two
// atomics and never allocates.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract, which
        // is `System::alloc`'s.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            grow(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: as for `alloc`.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            grow(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` was allocated by this allocator, that is by
        // `System`, with `layout` (the caller's contract).
        unsafe { System.dealloc(ptr, layout) };
        shrink(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `ptr` and `layout` come from this allocator, that is from
        // `System`, and `new_size` meets `realloc`'s contract (the caller's).
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            shrink(layout.size());
            grow(new_size);
        }
        new
    }
}
