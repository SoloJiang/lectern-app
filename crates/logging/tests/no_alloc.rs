//! 「性能」: a written call (info, 4 fields) does not allocate on the calling
//! thread. Its own test binary, with a global allocator that counts the
//! allocations of the thread that asked it to.

#![cfg(all(feature = "sink", feature = "test-support"))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::Path;
use std::time::Duration;

use lectern_logging::{Config, EarlyConfig, Header, Value, info, install_early, testing};

struct PerThread;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<u64> = const { Cell::new(0) };
}

// SAFETY: forwards every call to `System` unchanged; the bookkeeping uses
// const-initialized thread locals without destructors, which never allocate.
unsafe impl GlobalAlloc for PerThread {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = COUNTING.try_with(|c| {
            if c.get() {
                let _ = COUNT.try_with(|n| n.set(n.get() + 1));
            }
        });
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` was allocated by `System` with `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOC: PerThread = PerThread;

fn record(path: &Path, i: u64) {
    info!("index batch"; files = i, bytes = 4096_u64, path = Value::path(path), took = Duration::from_micros(12));
}

#[test]
fn written_call_does_not_allocate() {
    let dir = tempfile::tempdir().expect("dir");
    let logging = install_early(EarlyConfig {
        dir: Some(dir.path().to_owned()),
        cli_filter: Some("info".to_owned()),
    })
    .expect("install")
    .start(Config {
        header: Header {
            version: "0.1.0",
            build: "test",
            os: "TestOS".to_owned(),
            arch: "test64",
        },
        on_writer_start: None,
    });
    testing::set_callsite_limit(u32::MAX);
    let path = Path::new("/tmp/project/src/main.rs");
    // The first records of a thread set up its thread locals.
    for i in 0..10 {
        record(path, i);
    }
    assert!(testing::flush(&logging, Duration::from_secs(5)));
    // The counter sees this thread's allocations.
    COUNTING.with(|c| c.set(true));
    drop(std::hint::black_box(Box::new(1_u64)));
    COUNTING.with(|c| c.set(false));
    assert_eq!(COUNT.with(|n| n.replace(0)), 1);
    COUNTING.with(|c| c.set(true));
    for i in 0..100 {
        record(path, i);
    }
    COUNTING.with(|c| c.set(false));
    assert_eq!(
        COUNT.with(Cell::get),
        0,
        "allocations on the calling thread"
    );
    assert!(testing::flush(&logging, Duration::from_secs(5)));
    let status = logging.status();
    assert_eq!(status.dropped, 0);
}
