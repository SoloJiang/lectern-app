//! `schedule_*`: the tiers' order, their guards and priorities, batches and
//! their limits, the pause at batch boundaries and in flight, a worktree
//! switch, one OID on two paths, the key from the bytes read, and tier 5.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lectern_executor::Priority;
use lectern_kernel::BackgroundState;
use lectern_services::code_intel::{Focus, TextOrigin, TextRef, TierState};
use lectern_services::project::BlobId;

use crate::extract::ExtractStatus;
use crate::index::host::Guard;
use crate::testing::{pathological_js, synthetic};

use super::support::{Gate, Harness, packages, ts};
use super::warm::fill;

#[test]
fn schedule_tiers_in_order_with_their_guards() {
    let h = Harness::new();
    let mut all = Vec::new();
    for dir in ["app", "lib", ""] {
        for i in 0..4 {
            let path = if dir.is_empty() {
                format!("c{i}.ts")
            } else {
                format!("{dir}/{}{i}.ts", &dir[..1])
            };
            let source = ts(&format!("f_{}_{i}", dir.replace('/', "_")));
            h.git.add("main", &[(&path, &source)]);
            all.push((path, h.git.oid(&source)));
        }
    }
    h.git.commit("main");
    h.git.add_worktree("feature");
    let only_there = ts("only_in_feature");
    h.git.add("feature", &[("feature.ts", &only_there)]);
    let oid_of = |p: &str| all.iter().find(|(q, _)| q == p).map(|(_, o)| *o).expect("a path");
    // The kernel's state during each extraction.
    let kernel = h.host.kernel();
    let during: Arc<Mutex<Vec<(BlobId, bool, usize)>>> = Arc::default();
    let d = during.clone();
    let hash = h.git.hash();
    h.set_hook(move |_, bytes| {
        let oid = crate::git_access::hash_blob(hash, bytes);
        d.lock()
            .unwrap()
            .push((oid, kernel.is_quiescent(), kernel.background().count));
    });
    h.publish("main", packages(&["app", "lib"]));
    let wt = h.git.worktree_key("main");
    h.indexer.set_focus(
        &h.key(),
        Some(&wt),
        Focus::Current(Some(TextRef {
            worktree: Some(wt.clone()),
            path: "app/a0.ts".into(),
            oid: oid_of("app/a0.ts"),
            origin: TextOrigin::Worktree,
        })),
    );
    h.indexer
        .set_focus(&h.key(), Some(&wt), Focus::Review(Arc::from(vec!["lib/l1.ts".into()])));
    h.settle();
    // Tier 1, tier 2, tier 3 (the current text's sub-project), tier 4 in
    // path order, tier 5.
    let mut want = vec![oid_of("app/a0.ts"), oid_of("lib/l1.ts")];
    want.extend(["app/a1.ts", "app/a2.ts", "app/a3.ts"].map(oid_of));
    let mut rest: Vec<&(String, BlobId)> = all
        .iter()
        .filter(|(p, _)| !p.starts_with("app/") && p != "lib/l1.ts")
        .collect();
    rest.sort();
    want.extend(rest.iter().map(|(_, o)| *o));
    want.push(h.git.oid(&only_there));
    assert_eq!(h.extracted(), want);
    // Tiers 1 and 2 hold a BusyGuard on Interactive / Background; the batch
    // tiers a BackgroundGuard on Batch.
    let log = h.indexer.inner().host.log.lock().unwrap().clone();
    assert!(log.contains(&("tier1", Priority::Interactive, Guard::Busy)), "{log:?}");
    assert!(log.contains(&("tier2", Priority::Background, Guard::Busy)), "{log:?}");
    assert!(log.contains(&("batch", Priority::Batch, Guard::Background)), "{log:?}");
    assert!(log.iter().all(|(what, p, g)| match *what {
        "batch" => *p == Priority::Batch && *g == Guard::Background,
        _ => *p != Priority::Batch && *g == Guard::Busy,
    }));
    let during = during.lock().unwrap().clone();
    let (tier1, _, _) = during[0];
    assert_eq!(tier1, want[0]);
    assert!(!during[0].1, "tier 1 keeps the kernel busy");
    assert!(!during[1].1, "tier 2 keeps the kernel busy");
    for (oid, _, background) in &during[2..] {
        assert!(*background >= 1, "{oid}: a batch holds a BackgroundGuard");
    }
    let c = h.coverage();
    assert_eq!(
        [c.focus, c.subproject, c.worktree, c.other_worktrees],
        [TierState::Done; 4]
    );
}

#[test]
fn schedule_pause_takes_effect_at_batch_boundaries() {
    let h = Harness::new();
    fill(&h, 150);
    let kernel = h.host.kernel();
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let k = kernel.clone();
    h.set_hook(move |_, _| {
        if c.fetch_add(1, Ordering::AcqRel) + 1 == 70 {
            k.pause();
        }
    });
    h.open();
    // The second batch stops when the pause comes: five files written, the
    // seventieth abandoned (the pause is checked when it registers).
    assert_eq!(h.batches(), [64, 5]);
    assert_eq!(kernel.background().state, BackgroundState::Paused);
    let abandoned = h.extracted()[69];
    assert_eq!(h.status(abandoned), Some(ExtractStatus::Cancelled));
    assert_eq!(h.coverage().subproject, TierState::Pending);
    let before = h.extractions();
    kernel.resume();
    h.settle();
    // After the resume the abandoned file goes first.
    assert_eq!(h.extracted()[before], abandoned);
    assert_eq!(h.extractions(), 151);
    assert_eq!(h.coverage().subproject, TierState::Done);
    assert_eq!(kernel.background().state, BackgroundState::Idle);
}

#[test]
fn schedule_paused_before_the_first_batch() {
    let h = Harness::new();
    fill(&h, 10);
    let kernel = h.host.kernel();
    kernel.pause();
    h.open();
    assert_eq!(h.extractions(), 0);
    assert_eq!(kernel.background().state, BackgroundState::Paused);
    kernel.resume();
    h.settle();
    assert_eq!(h.extractions(), 10);
}

#[test]
fn schedule_pause_abandons_in_flight() {
    let h = Harness::new();
    // Under the 1 MiB limit with the closing brackets.
    let big = pathological_js((1 << 20) - 8192);
    assert!(big.len() < 1 << 20);
    h.git.add("main", &[("big.js", &big), ("small.ts", &ts("small"))]);
    h.git.commit("main");
    let big_oid = h.git.oid(&big);
    let gate = Arc::new(Gate::default());
    let g = gate.clone();
    let hash = h.git.hash();
    h.set_hook(move |_, bytes| {
        if crate::git_access::hash_blob(hash, bytes) == big_oid {
            g.entered.store(true, Ordering::Release);
        }
    });
    let kernel = h.host.kernel();
    h.publish("main", Vec::new());
    let executor = h.executor.clone();
    let paused_at = std::thread::scope(|s| {
        let worker = s.spawn(move || {
            // fs_watch needs nothing here: the reconcile and the batch run on
            // the manual executor.
            executor.run_until_idle();
        });
        gate.wait_entered();
        // Parsing has started (the pause is not just seen at registration).
        std::thread::sleep(Duration::from_millis(20));
        let at = Instant::now();
        kernel.pause();
        h.clock.advance(Duration::from_millis(50));
        worker.join().expect("the executor thread");
        at
    });
    let took = paused_at.elapsed();
    assert_eq!(h.status(big_oid), Some(ExtractStatus::Cancelled));
    assert!(took < Duration::from_secs(3), "the pause took {took:?}");
    assert!(!h.stored(big_oid, "js"), "an abandoned file is not written");
    assert!(!h.hinted("big.js"));
    assert_eq!(kernel.background().state, BackgroundState::Paused);
    // After the resume it is the first file extracted again; it times out
    // this time (the clock passes its deadline) and that is stored.
    let before = h.extractions();
    gate.entered.store(false, Ordering::Release);
    kernel.resume();
    let executor = h.executor.clone();
    std::thread::scope(|s| {
        let worker = s.spawn(move || {
            executor.run_until_idle();
        });
        gate.wait_entered();
        h.clock.advance(Duration::from_millis(300));
        worker.join().expect("the executor thread");
    });
    h.settle();
    assert_eq!(h.extracted()[before], big_oid);
    assert_eq!(h.status(big_oid), Some(ExtractStatus::TimedOut));
    assert!(h.stored(big_oid, "js"));
    assert!(h.hinted("big.js"));
}

#[test]
fn schedule_batch_limits() {
    struct Case {
        name: &'static str,
        files: usize,
        len: usize,
        /// The clock advances this much per extraction.
        step: fn(usize) -> Duration,
        batches: &'static [usize],
    }
    let cases = [
        Case {
            name: "64 files",
            files: 150,
            len: 400,
            step: |_| Duration::ZERO,
            batches: &[64, 64, 22],
        },
        Case {
            name: "2 MiB read",
            files: 10,
            len: 400 << 10,
            step: |_| Duration::ZERO,
            batches: &[6, 4],
        },
        Case {
            name: "50 ms",
            files: 10,
            len: 400,
            step: |_| Duration::from_millis(20),
            batches: &[3, 3, 3, 1],
        },
        Case {
            name: "one 200 ms file runs to its end",
            files: 3,
            len: 400,
            step: |i| Duration::from_millis(if i == 0 { 200 } else { 1 }),
            batches: &[1, 2],
        },
    ];
    for case in cases {
        let h = Harness::new();
        for i in 0..case.files {
            let source = synthetic::source("ts", i, case.len);
            h.git.add("main", &[(&format!("f{i:03}.ts"), &source)]);
        }
        h.git.commit("main");
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let clock = h.clock.clone();
        let step = case.step;
        h.set_hook(move |_, _| {
            let i = c.fetch_add(1, Ordering::AcqRel);
            clock.advance(step(i));
        });
        // The deadline is not what ends these batches.
        h.open();
        assert_eq!(h.batches(), case.batches, "{}", case.name);
        assert_eq!(h.extractions(), case.files, "{}", case.name);
    }
}

#[test]
fn schedule_switch_worktree_rebuilds_without_extracting_again() {
    let h = Harness::new();
    let files = fill(&h, 20);
    h.open();
    assert_eq!(h.extractions(), 20);
    let builds = h.stats().cursor_builds;
    // A linked worktree with the same files, one of them changed, one new.
    h.git.add_worktree("feature");
    for (path, _, _) in &files {
        let bytes = std::fs::read(h.git.root("main").join(path)).expect("a file");
        h.git.add("feature", &[(path, &bytes)]);
    }
    let changed = ts("changed_in_feature");
    let new = ts("new_in_feature");
    h.git.add("feature", &[(&files[0].0, &changed), ("new.ts", &new)]);
    h.open_with("feature", Vec::new());
    let after: Vec<BlobId> = h.extracted()[20..].to_vec();
    let mut want = vec![h.git.oid(&changed), h.git.oid(&new)];
    want.sort();
    let mut got = after.clone();
    got.sort();
    assert_eq!(got, want, "only what the cache lacks");
    assert!(h.stats().cursor_builds > builds);
    assert!(h.hinted("new.ts"));
}

#[test]
fn schedule_one_oid_on_two_paths_once() {
    let h = Harness::new();
    let same = ts("shared");
    h.git.add(
        "main",
        &[("a/same.ts", &same), ("b/same.ts", &same), ("c.ts", &ts("other"))],
    );
    h.git.commit("main");
    h.open();
    assert_eq!(h.extractions(), 2);
    for path in ["a/same.ts", "b/same.ts", "c.ts"] {
        assert!(h.hinted(path), "{path}");
    }
}

#[test]
fn schedule_key_from_bytes_read() {
    let h = Harness::new();
    h.git.add("main", &[("0.ts", &ts("zero"))]);
    h.git.commit("main");
    h.open();
    let a = ts("first");
    let old = ts("queued_as_this");
    let new = ts("read_as_this");
    h.git.add("main", &[("a.ts", &a), ("b.ts", &old)]);
    h.git.commit("main");
    // While `a.ts` is extracted, `b.ts` changes on disk, after the batch
    // queued it under its index OID and before it is read; no event yet.
    let root = h.git.root("main");
    let a_oid = h.git.oid(&a);
    let hash = h.git.hash();
    let replaced = Arc::new(AtomicBool::new(false));
    let r = replaced.clone();
    let new_bytes = new.clone();
    h.set_hook(move |_, bytes| {
        if crate::git_access::hash_blob(hash, bytes) == a_oid && !r.swap(true, Ordering::AcqRel) {
            std::fs::write(root.join("b.ts"), &new_bytes).expect("rewrite b.ts");
        }
    });
    // The new index arrives as a new file list; the worktree's events come
    // later.
    h.refresh_files();
    assert!(h.stored(h.git.oid(&new), "ts"), "written under the bytes read");
    assert!(!h.stored(h.git.oid(&old), "ts"), "nothing under the queued OID");
    assert!(!h.hinted("b.ts"), "the queued OID's hint is not set");
    // The event comes: the overlay takes the new OID, which is indexed.
    h.emit(&[("b.ts", super::support::CHANGED)]);
    h.settle();
    assert!(h.hinted("b.ts"));
    assert_eq!(h.extractions(), 3);
}

#[test]
fn schedule_tier5_extracts_only_missing_clean_files() {
    let h = Harness::new();
    let shared = ts("in_both");
    h.git.add("main", &[("shared.ts", &shared), ("main.ts", &ts("main_only"))]);
    h.git.commit("main");
    h.git.add_worktree("feature");
    let fresh = ts("only_in_feature");
    let staged = ts("staged_in_feature");
    let on_disk = ts("edited_in_feature");
    h.git.add("feature", &[("shared.ts", &shared), ("fresh.ts", &fresh)]);
    h.git.add("feature", &[("dirty.ts", &staged)]);
    h.git.write("feature", "dirty.ts", &on_disk);
    h.open();
    let extracted = h.extracted();
    assert!(extracted.contains(&h.git.oid(&fresh)));
    assert_eq!(
        extracted.iter().filter(|o| **o == h.git.oid(&shared)).count(),
        1,
        "the shared blob is extracted once"
    );
    // The edited file waits until `feature` is the current worktree.
    assert!(!extracted.contains(&h.git.oid(&on_disk)));
    assert!(!extracted.contains(&h.git.oid(&staged)));
    assert_eq!(h.extractions(), 3);
    assert_eq!(h.coverage().other_worktrees, TierState::Done);
}

/// `code_intel.tags` with a 24 KiB quota: indexing overflows it.
static TINY: lectern_cache::Tables = lectern_cache::Tables {
    namespace: crate::store::NAMESPACE,
    schema: crate::store::SCHEMA,
    statements: crate::store::STATEMENTS,
    quota: 24 << 10,
};

#[test]
fn schedule_over_the_quota_extracts_each_file_once() {
    let h = Harness::new();
    *h.indexer.inner().tables.lock().unwrap() = Some(&TINY);
    fill(&h, 80);
    h.open();
    let store = h.indexer.store(&h.key()).expect("store");
    assert!(store.evictions() > 0, "the quota was exceeded");
    // The walk finishes: what its own writes evicted is not extracted again
    // in this session (a query or the next open does it).
    let mut extracted = h.extracted();
    let n = extracted.len();
    extracted.sort();
    extracted.dedup();
    assert_eq!(extracted.len(), n, "no file extracted twice");
    assert_eq!(n, 80);
    let c = h.coverage();
    assert_eq!([c.subproject, c.worktree], [TierState::Done; 2]);
}

#[test]
fn schedule_session_switch_during_an_extraction() {
    let h = Harness::new();
    fill(&h, 12);
    h.git.add_worktree("feature");
    h.git.add("feature", &[("f.ts", &ts("feature"))]);
    let inner = h.indexer.inner().clone();
    let projects = h.projects.clone();
    let snapshot = h.git.snapshot(100, "feature", Vec::new());
    let key = h.key();
    let once = Arc::new(Mutex::new(Some(snapshot)));
    h.set_hook(move |_, _| {
        let next = once.lock().unwrap().take();
        if let Some(snapshot) = next {
            // The active worktree changes and the new session starts while
            // this extraction (of the old session) runs.
            projects.set(snapshot);
            inner.reconcile_now(&key);
        }
    });
    h.open();
    // The old session's batch ended without touching the new session's
    // counters; the new session indexed its worktree.
    let c = h.coverage();
    assert_eq!([c.focus, c.subproject, c.worktree], [TierState::Done; 3]);
    assert!(h.hinted("f.ts"));
}
