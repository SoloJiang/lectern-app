//! `prefetch_*`: at most four targets, one task at a time, the results only
//! in the LRU (as background work, found by the first screen), and the
//! parse stopping at a slice boundary when the yield switch turns on.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use lectern_git::{HashKind, Oid};
use lectern_services::reader::Anchor;

use super::rig::{FLIP_AT, Rig, Threaded, VIEW};
use super::{full, lang};
use crate::doc::Probe;
use crate::git_access::blob_id;
use crate::limits::PREFETCH_MAX;
use crate::lru::{Class, LruKey, SpansOf, WHOLE};
use crate::{DocState, Highlighting, Trace, testing};

fn whole_key(bytes: &[u8], name: &str) -> LruKey {
    let oid = blob_id(&Oid::for_blob(HashKind::Sha1, bytes));
    SpansOf::new(oid, lang(name)).spans(WHOLE)
}

/// A line's start and its tokens' start, length and kind.
type Line = (u64, Vec<(u32, u32, crate::TokenKind)>);

/// The tokens of every line, for comparing spans.
fn lines(spans: &crate::WindowSpans) -> Vec<Line> {
    spans
        .lines()
        .map(|(at, tokens)| {
            let tokens = tokens
                .iter()
                .map(|t| (t.start(), t.len(), t.kind()))
                .collect();
            (at, tokens)
        })
        .collect()
}

#[test]
fn prefetch_computes_at_most_four_and_only_fills_the_lru() {
    let rig = Rig::new();
    let files: Vec<(String, Vec<u8>)> = (0..6)
        .map(|i| (format!("m{i}.ts"), testing::ts(16 << 10, i * 100)))
        .collect();
    for (path, bytes) in &files {
        rig.write(path, bytes);
    }
    let targets = files
        .iter()
        .map(|(path, _)| (rig.target(path, Anchor::Start), rig.root.clone()))
        .collect();
    rig.engine.prefetch(targets);
    rig.settle();
    let lru = &rig.engine.inner().lru;
    for (i, (path, bytes)) in files.iter().enumerate() {
        let key = whole_key(bytes, "ts");
        if i < PREFETCH_MAX {
            assert_eq!(lru.class(&key), Some(Class::Background), "{path}");
        } else {
            assert!(!lru.contains(&key), "{path} is past the first four");
        }
    }
    // Prefetched again: found before any read or parse, still background.
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = events.clone();
    rig.engine
        .set_trace(move |event| log.lock().unwrap().push(event));
    let (path, bytes) = &files[0];
    rig.engine
        .prefetch(vec![(rig.target(path, Anchor::Start), rig.root.clone())]);
    rig.settle();
    assert_eq!(*events.lock().unwrap(), vec![Trace::PrefetchTask]);
    assert_eq!(lru.class(&whole_key(bytes, "ts")), Some(Class::Background));
    // Nothing was opened or told.
    assert_eq!(rig.notified.load(Ordering::SeqCst), 0);
    assert_eq!(rig.engine.tasks_in_flight(), 0);
    // The first screen of a prefetched file is its whole spans, exact,
    // the very ones the prefetch computed; opening it makes them the
    // user's.
    let (path, bytes) = &files[1];
    let first = rig
        .engine
        .first_screen(rig.target(path, Anchor::Start), rig.root.clone(), VIEW);
    assert_eq!(first.state, DocState::Ready);
    assert_eq!(first.highlight, Highlighting::On);
    let cached = lru
        .spans(&whole_key(bytes, "ts"), Class::Background)
        .unwrap();
    assert!(Arc::ptr_eq(&first.spans[0], &cached));
    assert_eq!(lines(&cached), lines(&full(lang("ts"), bytes)));
    assert_eq!(lru.class(&whole_key(bytes, "ts")), Some(Class::Opened));
}

#[test]
fn prefetch_a_new_request_replaces_the_targets_not_started() {
    let rig = Rig::new();
    let files: Vec<(String, Vec<u8>)> = (0..3)
        .map(|i| (format!("r{i}.rs"), testing::rust(8 << 10, i * 50)))
        .collect();
    for (path, bytes) in &files {
        rig.write(path, bytes);
    }
    let target = |i: usize| (rig.target(&files[i].0, Anchor::Start), rig.root.clone());
    rig.engine.prefetch(vec![target(0), target(1)]);
    rig.engine.prefetch(vec![target(2)]);
    rig.settle();
    let lru = &rig.engine.inner().lru;
    // The first request's task took nothing before the second came.
    assert!(!lru.contains(&whole_key(&files[0].1, "rust")));
    assert!(!lru.contains(&whole_key(&files[1].1, "rust")));
    assert!(lru.contains(&whole_key(&files[2].1, "rust")));
}

#[test]
fn prefetch_runs_one_task_at_a_time() {
    let t = Threaded::new(3);
    let in_flight = t.engine.inner().in_flight();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    let count = in_flight.clone();
    t.engine.set_trace(move |event| {
        if event == Trace::PrefetchTask {
            log.lock().unwrap().push(count.load(Ordering::SeqCst));
        }
    });
    let mut targets = Vec::new();
    for i in 0..PREFETCH_MAX {
        let path = format!("f{i}.ts");
        t.write(&path, &testing::ts(64 << 10, i * 10));
        targets.push((t.target(&path, Anchor::Start), t.root.clone()));
    }
    t.engine.prefetch(targets);
    assert_eq!(in_flight.load(Ordering::SeqCst), 1, "one task queued");
    let start = Instant::now();
    while t.engine.tasks_in_flight() > 0 {
        assert!(start.elapsed() < Duration::from_secs(60));
        std::thread::sleep(Duration::from_millis(1));
    }
    // Every turn found its task alone: one task, a file a turn.
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen, vec![1; PREFETCH_MAX]);
}

#[test]
fn prefetch_larger_files_get_the_first_screens_window() {
    let rig = Rig::new();
    let bytes = testing::ts(3 << 20, 0);
    rig.write("big.ts", &bytes);
    let at = Anchor::Byte {
        byte: 2 << 20,
        line: 0,
        column: 0,
    };
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = events.clone();
    rig.engine
        .set_trace(move |event| log.lock().unwrap().push(event));
    rig.engine
        .prefetch(vec![(rig.target("big.ts", at), rig.root.clone())]);
    rig.settle();
    let hashed = |events: &[Trace]| {
        events
            .iter()
            .filter(|e| matches!(e, Trace::PrefetchHash { .. }))
            .count()
    };
    assert_eq!(hashed(&events.lock().unwrap()), 1);
    let lru = &rig.engine.inner().lru;
    let screens: Vec<LruKey> = lru
        .keys()
        .into_iter()
        .filter(|k| matches!(k, LruKey::Screen { .. }))
        .collect();
    assert_eq!(screens.len(), 1, "{:?}", lru.keys());
    assert_eq!(lru.class(&screens[0]), Some(Class::Background));
    // Prefetched again (the pointer rests there once more): the file is
    // not hashed again and the window stays background work, the same one.
    let before = lru.spans(&screens[0], Class::Background).unwrap();
    events.lock().unwrap().clear();
    rig.engine
        .prefetch(vec![(rig.target("big.ts", at), rig.root.clone())]);
    rig.settle();
    let again = events.lock().unwrap().clone();
    assert_eq!(hashed(&again), 0, "{again:?}");
    assert!(again.contains(&Trace::PrefetchDone {
        path: "big.ts".into()
    }));
    assert_eq!(lru.class(&screens[0]), Some(Class::Background));
    let after = lru.spans(&screens[0], Class::Background).unwrap();
    assert!(Arc::ptr_eq(&before, &after));
    // The line checkpoints were written with the OID.
    let oid = blob_id(&Oid::for_blob(HashKind::Sha1, &bytes));
    assert!(
        rig.engine
            .inner()
            .checkpoints(&rig.project(), &oid)
            .is_some()
    );
    // Opening there takes the window from the LRU: no parse.
    let first = rig
        .engine
        .first_screen(rig.target("big.ts", at), rig.root.clone(), VIEW);
    assert_eq!(first.highlight, Highlighting::On);
    let cached = lru.spans(&screens[0], Class::Opened).unwrap();
    assert!(Arc::ptr_eq(&first.spans[0], &cached));
    assert!(first.spans[0].range().contains(&(2 << 20)));
}

#[test]
fn prefetch_stops_at_a_slice_boundary_when_yielding() {
    let t = Threaded::new(1);
    let bytes = testing::ts((1 << 20) - (16 << 10), 0);
    t.write("one.ts", &bytes);
    // Turned on from inside a step of the parse, at a slice boundary: in
    // flight by construction, however fast the machine parses.
    let flip = t.flip_inside(FLIP_AT, |p| matches!(p, Probe::Prefetch { .. }));
    t.engine
        .prefetch(vec![(t.target("one.ts", Anchor::Start), t.root.clone())]);
    let (yielded, at) = t.wait_for(0, |e| matches!(e, Trace::PrefetchYielded { .. }));
    let (_, switched) = flip.lock().unwrap().on.expect("turned on inside a step");
    let (from, began) = t.wait_for(0, |e| matches!(e, Trace::PrefetchParse { .. }));
    assert!(from < yielded && began <= switched && switched <= at);
    assert!(
        !t.events()[from..]
            .iter()
            .any(|(_, e)| matches!(e, Trace::PrefetchDone { .. })),
        "stopped before its end"
    );
    // Stopped at the next slice boundary: the step asked once more.
    assert_eq!(
        flip.lock().unwrap().after,
        vec![Probe::Prefetch { asked: FLIP_AT + 1 }]
    );
    let stop = at.duration_since(switched);
    eprintln!("prefetch stopped {stop:?} after the switch");
    assert!(stop <= Duration::from_millis(10), "{stop:?}");
    // A file over 256 KiB waits for the switch to turn off, keeping its
    // state: no keepalive slot (no timer on the clock), no probe asked, no
    // event. That nothing moves is watched for 100 ms (a slow machine
    // cannot make it fail); then it goes on to the same spans as one parse.
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(t.clock.pending_timers(), 0);
    assert_eq!(flip.lock().unwrap().after.len(), 1);
    assert_eq!(
        t.events().len(),
        yielded + 1,
        "{:?}",
        &t.events()[yielded..]
    );
    t.host.kernel().set_yield(false);
    t.wait_for(yielded, |e| matches!(e, Trace::PrefetchDone { .. }));
    let spans = t
        .engine
        .inner()
        .lru
        .spans(&whole_key(&bytes, "ts"), Class::Background)
        .unwrap();
    assert_eq!(lines(&spans), lines(&full(lang("ts"), &bytes)));
}
