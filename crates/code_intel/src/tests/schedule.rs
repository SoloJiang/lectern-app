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
    let oid_of = |p: &str| {
        all.iter()
            .find(|(q, _)| q == p)
            .map(|(_, o)| *o)
            .expect("a path")
    };
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
    h.indexer.set_focus(
        &h.key(),
        Some(&wt),
        Focus::Review(Arc::from(vec!["lib/l1.ts".into()])),
    );
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
    assert!(
        log.contains(&("tier1", Priority::Interactive, Guard::Busy)),
        "{log:?}"
    );
    assert!(
        log.contains(&("tier2", Priority::Background, Guard::Busy)),
        "{log:?}"
    );
    assert!(
        log.contains(&("batch", Priority::Batch, Guard::Background)),
        "{log:?}"
    );
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
    h.git
        .add("main", &[("big.js", &big), ("small.ts", &ts("small"))]);
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
    h.git
        .add("feature", &[(&files[0].0, &changed), ("new.ts", &new)]);
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
        &[
            ("a/same.ts", &same),
            ("b/same.ts", &same),
            ("c.ts", &ts("other")),
        ],
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
    assert!(
        h.stored(h.git.oid(&new), "ts"),
        "written under the bytes read"
    );
    assert!(
        !h.stored(h.git.oid(&old), "ts"),
        "nothing under the queued OID"
    );
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
    h.git.add(
        "main",
        &[("shared.ts", &shared), ("main.ts", &ts("main_only"))],
    );
    h.git.commit("main");
    h.git.add_worktree("feature");
    let fresh = ts("only_in_feature");
    let staged = ts("staged_in_feature");
    let on_disk = ts("edited_in_feature");
    h.git
        .add("feature", &[("shared.ts", &shared), ("fresh.ts", &fresh)]);
    h.git.add("feature", &[("dirty.ts", &staged)]);
    h.git.write("feature", "dirty.ts", &on_disk);
    h.open();
    let extracted = h.extracted();
    assert!(extracted.contains(&h.git.oid(&fresh)));
    assert_eq!(
        extracted
            .iter()
            .filter(|o| **o == h.git.oid(&shared))
            .count(),
        1,
        "the shared blob is extracted once"
    );
    // The edited file waits until `feature` is the current worktree.
    assert!(!extracted.contains(&h.git.oid(&on_disk)));
    assert!(!extracted.contains(&h.git.oid(&staged)));
    assert_eq!(h.extractions(), 3);
    assert_eq!(h.coverage().other_worktrees, TierState::Done);
}

#[test]
fn schedule_large_files_take_turns() {
    use crate::index::batch::LargeSlot;

    let slot = LargeSlot::default();
    let first = slot.acquire(|| false).expect("free");
    let stop = AtomicBool::new(false);
    // The waiter checks its stop probe before each wait: two checks mean
    // it found the slot busy and waited at least once (no guessing from
    // elapsed time).
    let checks = std::sync::atomic::AtomicUsize::new(0);
    let waited = |n: usize| {
        let start = Instant::now();
        while checks.load(Ordering::Acquire) < n {
            assert!(start.elapsed() < Duration::from_secs(20), "never waited");
            std::thread::sleep(Duration::from_millis(1));
        }
    };
    std::thread::scope(|s| {
        // A second batch task waits while the first holds the slot, and
        // gives up when it is told to stop (a pause, a cancel).
        let waiter = s.spawn(|| {
            slot.acquire(|| {
                checks.fetch_add(1, Ordering::AcqRel);
                stop.load(Ordering::Acquire)
            })
            .is_some()
        });
        waited(2);
        assert!(!waiter.is_finished(), "waits for the slot");
        stop.store(true, Ordering::Release);
        assert!(!waiter.join().expect("waiter"), "gave up on stop");
    });
    checks.store(0, Ordering::Release);
    std::thread::scope(|s| {
        let waiter = s.spawn(|| {
            slot.acquire(|| {
                checks.fetch_add(1, Ordering::AcqRel);
                false
            })
            .is_some()
        });
        waited(2);
        drop(first);
        assert!(
            waiter.join().expect("waiter"),
            "its turn once the first ends"
        );
    });
}

#[test]
fn schedule_folder_project_indexes_at_open() {
    use lectern_services::code_intel::{
        CandidateSource, Completeness, Position, QueryKind, QueryState,
    };
    use lectern_services::project::{
        ActiveTree, FileKind, FileList, ProjectKey, ProjectSnapshot, Scan,
    };

    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().canonicalize().expect("canonical");
    std::fs::write(root.join("a.ts"), ts("in_a_folder")).expect("write");
    std::fs::write(
        root.join("b.ts"),
        b"export function run() { return in_a_folder(1); }\n",
    )
    .expect("write");
    std::fs::write(root.join("notes.md"), b"# notes\n").expect("write");
    let key = ProjectKey::Folder {
        root: Arc::from(root.as_path()),
    };
    let open = |h: &Harness| {
        let mut files = FileList::builder();
        for path in ["a.ts", "b.ts", "notes.md"] {
            files.push(path, FileKind::File);
        }
        let event = h.projects.set(ProjectSnapshot {
            key: key.clone(),
            generation: 1,
            name: "folder".into(),
            worktrees: Arc::from(Vec::new()),
            active: Some(ActiveTree {
                root: Arc::from(root.as_path()),
                worktree: None,
                opened_at: "".into(),
                files: Scan::Ready(Arc::new(files.build())),
                packages: Scan::Ready(Arc::from(Vec::new())),
            }),
        });
        h.indexer.project_event(&event);
        h.settle();
    };
    let h = Harness::new();
    open(&h);
    // Every file read: its raw bytes' SHA-1 is its OID; every tier done.
    let map = h.indexer.map(&key).expect("open");
    let a = h.git.oid(&ts("in_a_folder"));
    assert_eq!(map.effective("a.ts").map(|e| e.oid), Some(a));
    assert!(
        map.effective("notes.md").is_none(),
        "no language: not in the map"
    );
    let c = h.indexer.coverage(&key).expect("open");
    assert_eq!(
        [c.focus, c.subproject, c.worktree, c.other_worktrees],
        [TierState::Done; 4]
    );
    assert_eq!(h.extractions(), 2);
    // No HEAD: never clean.
    assert!(!h.indexer.head_clean(&key, "a.ts"));
    // A jump answers from the index, verifying the folder's raw OIDs.
    let b = std::fs::read(root.join("b.ts")).expect("read");
    let at = lectern_services::code_intel::TextRef {
        worktree: None,
        path: "b.ts".into(),
        oid: h.git.oid(&b),
        origin: lectern_services::code_intel::TextOrigin::Worktree,
    };
    let position = Position {
        line: 0,
        column: 32,
    };
    let id = h.indexer.query(lectern_services::code_intel::Query {
        project: key.clone(),
        kind: QueryKind::Definition { at, position },
    });
    h.settle();
    let r = h.indexer.results(id).expect("kept");
    assert_eq!(r.candidates.len(), 1, "{r:?}");
    assert_eq!(&*r.candidates[0].definition.location.path, "a.ts");
    assert_eq!(r.candidates[0].source, CandidateSource::Index);
    assert_eq!(r.state, QueryState::Done(Completeness::Complete));
    // The next run reads every file again (no index to know the OIDs
    // from) and extracts nothing.
    let h = h.reopen(super::support::Options::default());
    open(&h);
    assert_eq!(h.extractions(), 0);
    let c = h.indexer.coverage(&key).expect("open");
    assert_eq!(c.subproject, TierState::Done);
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

/// Over the quota, focus changes do not restart the batch walk: the batch
/// tiers extract nothing again; tier 1 extracts at most the opened file
/// (it needs `Done`, and the file may have been evicted).
#[test]
fn schedule_over_the_quota_focus_changes_extract_nothing_again() {
    let h = Harness::new();
    *h.indexer.inner().tables.lock().unwrap() = Some(&TINY);
    let files = fill(&h, 80);
    h.open();
    let store = h.indexer.store(&h.key()).expect("store");
    assert!(store.evictions() > 0, "the quota was exceeded");
    let batch: usize = h.batches().iter().sum();
    let first = h.extractions();
    let builds = h.stats().cursor_builds;
    for (round, (path, _, _)) in files.iter().take(5).enumerate() {
        h.current(path);
        h.settle();
        assert_eq!(
            h.batches().iter().sum::<usize>(),
            batch,
            "focus change {round}: the batch tiers extracted again"
        );
        assert!(h.extractions() <= first + round + 1);
    }
    assert_eq!(h.stats().cursor_builds, builds, "no cursor rebuild");
    let c = h.coverage();
    assert_eq!([c.subproject, c.worktree], [TierState::Done; 2]);
}

/// Files the batch found missing and queued are then put in the review set
/// and written by tier 2: the batch does not extract or write them again
/// (a replace would count as an eviction).
#[test]
fn schedule_review_files_written_by_tier_2_are_not_extracted_again() {
    let h = Harness::new();
    let files = fill(&h, 150);
    let kernel = h.host.kernel();
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let k = kernel.clone();
    h.set_hook(move |_, _| {
        if c.fetch_add(1, Ordering::AcqRel) + 1 == 10 {
            k.pause();
        }
    });
    h.open();
    let store = h.indexer.store(&h.key()).expect("store");
    let evictions = store.evictions();
    let review: Vec<lectern_services::project::RelPath> = files[100..110]
        .iter()
        .map(|(p, _, _)| p.as_str().into())
        .collect();
    h.set_focus(Focus::Review(review.into()));
    h.settle();
    kernel.resume();
    h.settle();
    let mut extracted = h.extracted();
    let n = extracted.len();
    extracted.sort();
    extracted.dedup();
    assert_eq!(extracted.len(), 150);
    // The one file the pause abandoned is extracted twice.
    assert_eq!(n, 151);
    assert_eq!(store.evictions(), evictions, "nothing was replaced");
    let p = h.indexer.inner().project(&h.key()).expect("open");
    assert_eq!(crate::index::lock(&p.state).batch.full, None);
}

/// A file stored `TimedOut` by the batch is extracted again with the
/// interactive limits when it is opened, once per session.
#[test]
fn schedule_opened_timed_out_file_is_redone_once() {
    use crate::extract::Extraction;

    let h = Harness::new();
    let big = ts("big");
    h.git.add("main", &[("big.ts", &big)]);
    h.git.commit("main");
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    h.indexer
        .set_extractor(move |lang, bytes, deadline, cancel| {
            if c.fetch_add(1, Ordering::AcqRel) == 0 {
                // The background deadline ran out.
                Extraction {
                    status: ExtractStatus::TimedOut,
                    defs: Vec::new(),
                    refs: Vec::new(),
                }
            } else {
                crate::extract::extract(lang, bytes, deadline, cancel)
            }
        });
    h.git.add_worktree("feature");
    h.git.add("feature", &[("big.ts", &big)]);
    h.open();
    assert_eq!(calls.load(Ordering::Acquire), 1);
    assert!(h.hinted("big.ts"), "a TimedOut unit is stored");
    let store = h.indexer.store(&h.key()).expect("store");
    let (raw, stamp) = (store.evictions(), h.indexer.stamp(&h.key()));
    let (builds, tracked) = (h.stats().cursor_builds, h.git.count("tracked"));
    h.current("big.ts");
    h.settle();
    assert_eq!(calls.load(Ordering::Acquire), 2, "redone when opened");
    // The replace bumped the store's eviction generation; the hints, the
    // cursor and tier 5 do not count it.
    assert_eq!(store.evictions(), raw + 1);
    assert_eq!(h.indexer.stamp(&h.key()), stamp);
    assert_eq!(h.stats().cursor_builds, builds);
    assert_eq!(h.git.count("tracked"), tracked, "tier 5 not walked again");
    assert!(h.hinted("big.ts"));
    let lang = crate::index::lock(&h.indexer.inner().langs)
        .table
        .get("ts")
        .cloned()
        .expect("ts");
    let found = store
        .exists(&crate::store::Version::of(&lang), &[h.git.oid(&big)])
        .expect("exists");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].status, ExtractStatus::Done);
    // Opening it again checks nothing more.
    h.set_focus(Focus::Current(None));
    h.settle();
    h.current("big.ts");
    h.settle();
    assert_eq!(calls.load(Ordering::Acquire), 2);
    let p = h.indexer.inner().project(&h.key()).expect("open");
    assert_eq!(crate::index::lock(&p.state).batch.full, None);
}

/// An opened file that could not be read is not settled for the session:
/// opened again once it is readable, tier 1 extracts it.
#[test]
fn schedule_opened_file_read_failure_is_tried_again() {
    use crate::extract::Extraction;

    let h = Harness::new();
    let big = ts("big");
    h.git.add("main", &[("big.ts", &big)]);
    h.git.commit("main");
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    h.indexer
        .set_extractor(move |lang, bytes, deadline, cancel| {
            if c.fetch_add(1, Ordering::AcqRel) == 0 {
                Extraction {
                    status: ExtractStatus::TimedOut,
                    defs: Vec::new(),
                    refs: Vec::new(),
                }
            } else {
                crate::extract::extract(lang, bytes, deadline, cancel)
            }
        });
    h.open();
    assert_eq!(calls.load(Ordering::Acquire), 1);
    // Gone from the disk (no event yet): tier 1 cannot read it.
    h.git.delete("main", "big.ts");
    h.current("big.ts");
    h.settle();
    assert_eq!(calls.load(Ordering::Acquire), 1);
    // Back: opening it again extracts it.
    h.git.write("main", "big.ts", &big);
    h.set_focus(Focus::Current(None));
    h.settle();
    h.current("big.ts");
    h.settle();
    assert_eq!(calls.load(Ordering::Acquire), 2);
    let p = h.indexer.inner().project(&h.key()).expect("open");
    let lang = crate::index::lock(&h.indexer.inner().langs)
        .table
        .get("ts")
        .cloned()
        .expect("ts");
    let st = crate::index::lock(&p.state);
    assert!(
        st.focused
            .satisfied
            .contains(&(h.git.oid(&big), crate::store::Version::of(&lang)))
    );
}

/// Opening a file the batch is extracting: tier 1 does not wait on the
/// batch (ADR 0029); it cancels that extraction and extracts it itself.
#[test]
fn schedule_opened_file_takes_over_a_batch_extraction() {
    let current = |slow: &[u8], h: &Harness| {
        Focus::Current(Some(TextRef {
            worktree: Some(h.git.worktree_key("main")),
            path: "slow.ts".into(),
            oid: h.git.oid(slow),
            origin: TextOrigin::Worktree,
        }))
    };
    takes_over_a_batch_extraction(current);
}

/// The same for tier 2 (the review set): it does not wait on a batch that
/// a pause may hold for long either.
#[test]
fn schedule_review_file_takes_over_a_batch_extraction() {
    takes_over_a_batch_extraction(|_, _| {
        let paths: Vec<lectern_services::project::RelPath> = vec!["slow.ts".into()];
        Focus::Review(paths.into())
    });
}

fn takes_over_a_batch_extraction(focus: impl Fn(&[u8], &Harness) -> Focus) {
    use crate::extract::Extraction;

    let h = Harness::new();
    let slow = ts("slow");
    h.git.add("main", &[("slow.ts", &slow)]);
    h.git.commit("main");
    let gate = Arc::new(Gate::default());
    let g = gate.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    h.indexer
        .set_extractor(move |lang, bytes, deadline, cancel| {
            if c.fetch_add(1, Ordering::AcqRel) == 0 {
                // The batch's extraction runs until something cancels it.
                g.entered.store(true, Ordering::Release);
                let start = Instant::now();
                while !cancel.load(Ordering::Acquire) {
                    assert!(start.elapsed() < Duration::from_secs(20), "never cancelled");
                    std::thread::yield_now();
                }
                return Extraction {
                    status: ExtractStatus::Cancelled,
                    defs: Vec::new(),
                    refs: Vec::new(),
                };
            }
            crate::extract::extract(lang, bytes, deadline, cancel)
        });
    h.publish("main", Vec::new());
    let executor = h.executor.clone();
    std::thread::scope(|s| {
        let worker = s.spawn(move || {
            executor.run_until_idle();
        });
        gate.wait_entered();
        h.indexer.set_focus(
            &h.key(),
            Some(&h.git.worktree_key("main")),
            focus(&slow, &h),
        );
        // Tier 1 or 2 runs here while the batch task is still in its
        // extraction.
        h.executor.run_until_idle();
        worker.join().expect("the executor thread");
    });
    h.settle();
    assert_eq!(calls.load(Ordering::Acquire), 2);
    assert!(h.stored(h.git.oid(&slow), "ts"));
    assert!(h.hinted("slow.ts"));
    assert_eq!(h.coverage().focus, TierState::Done);
}

/// An index change in one other worktree walks only that worktree again,
/// and its blobs already confirmed this session are not checked or touched
/// again.
#[test]
fn schedule_tier5_index_change_rewalks_only_that_worktree() {
    let h = Harness::new();
    h.git.add("main", &[("main.ts", &ts("main"))]);
    h.git.commit("main");
    for name in ["feature", "other"] {
        h.git.add_worktree(name);
        h.git.add(
            name,
            &[("main.ts", &ts("main")), (&format!("{name}.ts"), &ts(name))],
        );
    }
    h.open();
    assert_eq!(h.coverage().other_worktrees, TierState::Done);
    let tracked = h.git.count("tracked");
    let checks = h.stats().exists_checks;
    let extractions = h.extractions();
    h.emit_git(&[("worktrees/feature/index", super::support::CHANGED)]);
    h.settle();
    assert_eq!(h.git.count("tracked"), tracked + 1, "only `feature`");
    assert_eq!(h.stats().exists_checks, checks, "confirmed blobs skipped");
    assert_eq!(h.extractions(), extractions);
    assert_eq!(h.coverage().other_worktrees, TierState::Done);
}

/// Until the first focus, the project opened last runs tiers 3–5; after
/// it, opening another project does not take them over.
#[test]
fn schedule_last_opened_project_runs_the_batch_before_any_focus() {
    let h = Harness::new();
    h.git.add("main", &[("a.ts", &ts("a"))]);
    h.git.commit("main");
    h.open();
    let batch_project = || crate::index::lock(&h.indexer.inner().batch_project).clone();
    assert_eq!(batch_project(), Some(h.key()));
    let open_folder = |name: &str| {
        let opened = open_folder(&h, name);
        h.settle();
        opened
    };
    let (_one, folder) = open_folder("one");
    assert_eq!(batch_project(), Some(folder));
    h.current("a.ts");
    h.settle();
    assert_eq!(batch_project(), Some(h.key()));
    let (_two, _) = open_folder("two");
    assert_eq!(batch_project(), Some(h.key()));
}

/// Opens a folder project with one file (no settling).
fn open_folder(
    h: &Harness,
    name: &str,
) -> (tempfile::TempDir, lectern_services::project::ProjectKey) {
    use lectern_services::project::{
        ActiveTree, FileKind, FileList, ProjectKey, ProjectSnapshot, Scan,
    };

    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().canonicalize().expect("canonical");
    let mut files = FileList::builder();
    files.push("a.ts", FileKind::File);
    let key = ProjectKey::Folder {
        root: Arc::from(root.as_path()),
    };
    let event = h.projects.set(ProjectSnapshot {
        key: key.clone(),
        generation: 1,
        name: name.into(),
        worktrees: Arc::from(Vec::new()),
        active: Some(ActiveTree {
            root: Arc::from(root.as_path()),
            worktree: None,
            opened_at: "".into(),
            files: Scan::Ready(Arc::new(files.build())),
            packages: Scan::Ready(Arc::from(Vec::new())),
        }),
    });
    h.indexer.project_event(&event);
    (dir, key)
}

/// One kick fills the batch pool: with three batch threads, three tasks
/// run the walk (`kick_batch`'s liveness rule; the harness checks it after
/// every settle and focus).
#[test]
fn schedule_one_kick_fills_the_batch_pool() {
    let h = Harness::with(super::support::Options {
        batch_tasks: 3,
        ..Default::default()
    });
    fill(&h, 200);
    h.open();
    assert_eq!(h.extractions(), 200);
    assert_eq!(h.indexer.inner().batch.lock().unwrap().peak, 3);
    let c = h.coverage();
    assert_eq!([c.subproject, c.worktree], [TierState::Done; 2]);
}

/// Closing the project that runs tiers 3–5 hands them to the one opened
/// before, and that project's walk starts (it was waiting its turn).
#[test]
fn schedule_closing_the_batch_project_starts_the_next_one() {
    let h = Harness::new();
    fill(&h, 20);
    // The git project opens, then a folder project, before any focus: the
    // folder runs tiers 3–5 and the git project waits.
    h.publish("main", Vec::new());
    let (_dir, folder) = open_folder(&h, "folder");
    h.settle();
    assert_eq!(h.extractions(), 0, "the git project waits its turn");
    h.indexer
        .project_event(&lectern_services::project::ProjectEvent::Closed { project: folder });
    h.assert_live("after closing the folder");
    h.settle();
    assert_eq!(h.extractions(), 20);
    let c = h.coverage();
    assert_eq!([c.subproject, c.worktree], [TierState::Done; 2]);
}

/// Closing a project while tier 2 has a long queue: its tasks stop before
/// the next file, nothing more is read, and the kernel is quiescent.
#[test]
fn schedule_close_during_a_review_queue_stops_reading() {
    let h = Harness::new();
    let files = fill(&h, 40);
    let kernel = h.host.kernel();
    // The batch tiers wait; tiers 1 and 2 do not.
    kernel.pause();
    h.open();
    assert_eq!(h.extractions(), 0);
    let inner = h.indexer.inner().clone();
    let key = h.key();
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    h.set_hook(move |_, _| {
        if c.fetch_add(1, Ordering::AcqRel) == 0 {
            inner.project_event(&lectern_services::project::ProjectEvent::Closed {
                project: key.clone(),
            });
        }
    });
    let review: Vec<lectern_services::project::RelPath> = files[..30]
        .iter()
        .map(|(p, _, _)| p.as_str().into())
        .collect();
    h.set_focus(Focus::Review(review.into()));
    h.settle();
    assert_eq!(
        calls.load(Ordering::Acquire),
        1,
        "no file read after the close"
    );
    assert!(h.quiescent(), "the tier 2 tasks released their guards");
    kernel.resume();
    h.settle();
    assert_eq!(calls.load(Ordering::Acquire), 1);
}

/// Tier 5's set of blobs confirmed stored this session stays small: 100k
/// blobs fit in 1.5 MB, and it tells blobs and languages apart.
#[test]
fn schedule_tier5_confirmed_set_is_compact() {
    let mut set = crate::index::state::Confirmed::default();
    let ts = crate::lang_name("ts");
    let oid = |i: u64| {
        let mut b = [0u8; 20];
        b[..8].copy_from_slice(&i.to_be_bytes());
        BlobId::from_bytes(&b).expect("an OID")
    };
    for i in 0..100_000 {
        set.insert(&oid(i), &ts);
    }
    assert!(set.contains(&oid(7), &ts));
    assert!(!set.contains(&oid(100_001), &ts));
    assert!(!set.contains(&oid(7), &crate::lang_name("js")));
    assert!(
        set.heap_bytes() <= 3 << 19,
        "{} bytes for 100k blobs",
        set.heap_bytes()
    );
}
