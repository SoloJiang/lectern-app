//! 「测试」缓冲.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::{Fixture, records, with_msg};
use crate::callsite::LECTERN_LIMIT;
use crate::engine::logger;
use crate::hooks::imp as hooks;
use crate::sink::{EARLY_CAP_FOR_TESTS, frames};

fn numbered(i: u64) {
    crate::info!("numbered record"; i = i, pad = "padding to make the record a little longer");
}

#[test]
fn early_buffer_overflow_drops_newest_and_counts() {
    let fx = Fixture::new();
    let _early = fx.install(None, None);
    LECTERN_LIMIT.store(u32::MAX, Ordering::Relaxed);
    let shared = logger::current().expect("installed");
    let state = shared.sink().expect("sink");
    for i in 0..1000 {
        numbered(i);
    }
    let buffers = state.buffers.lock().expect("lock");
    assert!(buffers.early.len() <= EARLY_CAP_FOR_TESTS);
    assert_eq!(
        buffers.early.capacity(),
        EARLY_CAP_FOR_TESTS,
        "no reallocation"
    );
    let kept: Vec<serde_json::Value> = frames(&buffers.early)
        .map(|(_, line)| serde_json::from_slice(line).expect("JSON"))
        .collect();
    assert!(kept.len() > 100 && kept.len() < 1000, "{}", kept.len());
    for (i, r) in kept.iter().enumerate() {
        assert_eq!(r["i"], i as u64, "the oldest records are kept");
    }
    assert_eq!(
        shared.dropped.load(Ordering::Relaxed),
        1000 - kept.len() as u64
    );
}

#[test]
fn running_buffer_never_blocks_callers() {
    let fx = Fixture::new();
    let logging = fx.start(None);
    LECTERN_LIMIT.store(u32::MAX, Ordering::Relaxed);
    hooks::stall_writer(true);
    let started = Instant::now();
    let threads: Vec<_> = (0..4)
        .map(|_| {
            std::thread::spawn(|| {
                for i in 0..10_000 {
                    numbered(i);
                }
            })
        })
        .collect();
    for t in threads {
        t.join().expect("no panic");
    }
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "callers did not wait for the writer"
    );
    assert!(
        logging.status().dropped > 0,
        "the full buffer dropped records"
    );
    hooks::stall_writer(false);
    assert!(logging.flush(Duration::from_secs(5)));
}

#[test]
fn drops_are_reported_on_the_next_write() {
    let fx = Fixture::new();
    let logging = fx.start(None);
    LECTERN_LIMIT.store(u32::MAX, Ordering::Relaxed);
    hooks::stall_writer(true);
    for i in 0..5000 {
        numbered(i);
    }
    let dropped = logging.status().dropped;
    assert!(dropped > 0);
    hooks::stall_writer(false);
    assert!(logging.flush(Duration::from_secs(5)));
    let records = records(fx.path());
    let reports = with_msg(&records, "records dropped");
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert_eq!(reports[0]["lv"], "warn");
    assert_eq!(reports[0]["dropped"], dropped);
    assert_eq!(
        with_msg(&records, "numbered record").len() as u64,
        5000 - dropped
    );
}
