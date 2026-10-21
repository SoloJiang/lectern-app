//! `yield_*`: tiers 4 and 5 yield to interaction (「交互让路」, ADR 0033),
//! with the kernel's switch, its keepalive slots on a `ManualClock`
//! (`Kernel::set_clock`) and the yield hook (`Inner::yield_hook`) that runs
//! at every yield check of an extraction.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lectern_kernel::testing::ManualClock;
use lectern_kernel::{BackgroundState, Kernel};
use lectern_services::code_intel::{Focus, TextOrigin, TextRef, TierState};
use lectern_services::project::BlobId;

use crate::extract::{Deadline, ExtractStatus, Extraction, Lang, extract};
use crate::testing::{java_pack, synthetic};

use super::support::{Harness, Options, packages, ts};

/// Past a slot's end and the interval to the next slot's start.
const PAST_SLOT: Duration = Duration::from_millis(600);

/// A harness whose kernel's slots run on a manual clock.
fn harness(batch_tasks: usize) -> (Harness, ManualClock, Arc<Kernel>) {
    let h = Harness::with(Options {
        batch_tasks,
        ..Options::default()
    });
    let clock = ManualClock::new();
    let kernel = h.host.kernel();
    kernel.set_clock(Arc::new(clock.clone()));
    (h, clock, kernel)
}

/// Runs at every yield check of an extraction in a yielding tier.
fn on_yield_check(h: &Harness, hook: impl FnMut() + Send + 'static) {
    *h.indexer.inner().yield_hook.lock().unwrap() = Some(Box::new(hook));
}

/// Files under `app/` (tier 3 once a file there is current) and `lib/`
/// (tier 4), committed; returns their OIDs.
fn files(h: &Harness, app: &[(&str, Vec<u8>)], lib: &[(&str, Vec<u8>)]) -> Vec<BlobId> {
    let mut oids = Vec::new();
    for (dir, set) in [("app", app), ("lib", lib)] {
        for (name, source) in set {
            let path = format!("{dir}/{name}");
            h.git.add("main", &[(&path, source)]);
            oids.push(h.git.oid(source));
        }
    }
    h.git.commit("main");
    oids
}

/// Opens with `app` and `lib` sub-projects and no current text: every file
/// is tier 4.
fn open_far(h: &Harness) {
    h.publish("main", packages(&["app", "lib"]));
    h.settle();
}

/// Makes `path` the current text: its sub-project is tier 3.
fn focus(h: &Harness, path: &str, source: &[u8]) {
    let wt = h.git.worktree_key("main");
    h.indexer.set_focus(
        &h.key(),
        Some(&wt),
        Focus::Current(Some(TextRef {
            worktree: Some(wt.clone()),
            path: path.into(),
            oid: h.git.oid(source),
            origin: TextOrigin::Worktree,
        })),
    );
}

fn stored(h: &Harness, source: &[u8]) -> bool {
    h.stored(h.git.oid(source), "ts")
}

/// Settles, moving the slots' clock on, until `done` (at most `rounds`).
fn settle_until(h: &Harness, clock: &ManualClock, rounds: usize, done: impl Fn() -> bool) {
    for _ in 0..rounds {
        h.settle();
        if done() {
            return;
        }
        clock.advance(PAST_SLOT);
    }
    h.settle();
    assert!(done(), "not done after {rounds} rounds");
}

/// The paths queued in tier `t`'s queue of the open project.
fn queued(h: &Harness, t: usize) -> Vec<String> {
    let project = h.indexer.inner().project(&h.key()).expect("open");
    let st = project.state.lock().unwrap();
    st.batch.resume[t]
        .iter()
        .map(|item| item.path.to_string())
        .collect()
}

fn big_ts(seed: usize, len: usize) -> Vec<u8> {
    synthetic::source("ts", seed, len)
}

#[test]
fn yield_tier3_does_not_yield() {
    let (h, _clock, kernel) = harness(1);
    let app: Vec<(String, Vec<u8>)> = (0..6)
        .map(|i| (format!("a{i}.ts"), ts(&format!("a{i}"))))
        .collect();
    let lib: Vec<(String, Vec<u8>)> = (0..6)
        .map(|i| (format!("l{i}.ts"), ts(&format!("l{i}"))))
        .collect();
    let app_ref: Vec<(&str, Vec<u8>)> = app.iter().map(|(p, s)| (p.as_str(), s.clone())).collect();
    let lib_ref: Vec<(&str, Vec<u8>)> = lib.iter().map(|(p, s)| (p.as_str(), s.clone())).collect();
    files(&h, &app_ref, &lib_ref);
    kernel.set_yield(true);
    h.publish("main", packages(&["app", "lib"]));
    focus(&h, "app/a0.ts", &app[0].1);
    h.settle();
    // Tier 3 ran as usual; tier 4 had one slot (its existence check) and
    // waits for the next, which the clock never brings.
    for (_, source) in &app {
        assert!(stored(&h, source));
    }
    for (_, source) in &lib {
        assert!(!stored(&h, source));
    }
    let c = h.coverage();
    assert_eq!(c.subproject, TierState::Done);
    assert_eq!(c.worktree, TierState::Pending);
    kernel.set_yield(false);
    h.settle();
    for (_, source) in &lib {
        assert!(stored(&h, source), "tier 4 goes on once the switch is off");
    }
    assert_eq!(h.coverage().worktree, TierState::Done);
}

#[test]
fn yield_tier3_work_does_not_wait_for_a_slot() {
    let (h, _clock, kernel) = harness(1);
    let a: Vec<Vec<u8>> = (0..4).map(|i| ts(&format!("a{i}"))).collect();
    let l: Vec<Vec<u8>> = (0..4).map(|i| ts(&format!("l{i}"))).collect();
    let names = ["x0.ts", "x1.ts", "x2.ts", "x3.ts"];
    let app: Vec<(&str, Vec<u8>)> = names.iter().copied().zip(a.iter().cloned()).collect();
    let lib: Vec<(&str, Vec<u8>)> = names.iter().copied().zip(l.iter().cloned()).collect();
    files(&h, &app, &lib);
    kernel.set_yield(true);
    open_far(&h);
    // Everything is tier 4: one slot, then the batch task waits.
    assert!(a.iter().chain(&l).all(|s| !stored(&h, s)));
    // A current text makes `app/` tier 3: it runs at once, no slot needed.
    focus(&h, "app/x0.ts", &a[0]);
    h.settle();
    assert!(a.iter().all(|s| stored(&h, s)), "tier 3 doesn't wait");
    assert!(l.iter().all(|s| !stored(&h, s)), "tier 4 still waits");
}

/// Wraps the extractor: records every extraction's status and, for `Done`,
/// its result, by OID.
#[derive(Default)]
struct Results {
    statuses: Mutex<Vec<(BlobId, ExtractStatus)>>,
    done: Mutex<HashMap<BlobId, Vec<Extraction>>>,
}

fn record(h: &Harness) -> Arc<Results> {
    let results = Arc::new(Results::default());
    let r = results.clone();
    let hash = h.git.hash();
    h.indexer.set_extractor(
        move |lang: &Lang, bytes: &[u8], deadline: &Deadline, cancel: &Arc<AtomicBool>| {
            let oid = crate::git_access::hash_blob(hash, bytes);
            let extraction = extract(lang, bytes, deadline, cancel);
            r.statuses.lock().unwrap().push((oid, extraction.status));
            if extraction.status == ExtractStatus::Done {
                r.done
                    .lock()
                    .unwrap()
                    .entry(oid)
                    .or_default()
                    .push(extraction.clone());
            }
            extraction
        },
    );
    results
}

impl Results {
    fn parks(&self, oid: BlobId) -> usize {
        self.statuses
            .lock()
            .unwrap()
            .iter()
            .filter(|(o, s)| *o == oid && *s == ExtractStatus::Parked)
            .count()
    }

    fn done(&self, oid: BlobId) -> Vec<Extraction> {
        self.done
            .lock()
            .unwrap()
            .get(&oid)
            .cloned()
            .unwrap_or_default()
    }
}

const CORE: [(&str, &str); 7] = [
    ("ts", "ts"),
    ("tsx", "tsx"),
    ("js", "js"),
    ("rs", "rust"),
    ("java", "java"),
    ("py", "python"),
    ("go", "go"),
];

/// `source` extracted in one go.
fn whole(h: &Harness, lang: &str, source: &[u8]) -> Extraction {
    let lang = crate::extract::LanguageTable::core()
        .get(lang)
        .cloned()
        .expect("a core language");
    let whole = extract(
        &lang,
        source,
        &Deadline::background(&h.indexer.inner().watchdog),
        &Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(whole.status, ExtractStatus::Done);
    whole
}

#[test]
fn yield_resumed_extraction_is_identical() {
    for (ext, lang) in CORE {
        let (h, clock, kernel) = harness(1);
        let source = synthetic::source(ext, 7, 120 << 10);
        assert!(source.len() as u64 <= lectern_kernel::SLOT_MAX_BYTES);
        let name = format!("big.{ext}");
        files(&h, &[], &[(name.as_str(), source.clone())]);
        let results = record(&h);
        // Every 40th check ends the slot: the extraction stops there, in
        // the parse and in the walk, and goes on in the next slot.
        let checks = Arc::new(AtomicUsize::new(0));
        let (c, k) = (checks.clone(), clock.clone());
        on_yield_check(&h, move || {
            if c.fetch_add(1, Ordering::AcqRel) % 40 == 39 {
                k.advance(PAST_SLOT);
            }
        });
        kernel.set_yield(true);
        let oid = h.git.oid(&source);
        open_far(&h);
        settle_until(&h, &clock, 200, || !results.done(oid).is_empty());
        assert!(results.parks(oid) >= 2, "{ext}: stopped and resumed");
        assert_eq!(
            results.done(oid),
            [whole(&h, lang, &source)],
            "{ext}: the same unit"
        );
    }
    // Up to 1 MiB (the background limit): stopped when the switch turns on,
    // it waits for the switch to turn off and goes on in a batch.
    for (ext, lang) in CORE {
        let (h, _clock, kernel) = harness(1);
        let max = crate::extract::BACKGROUND_MAX_LEN;
        let source = synthetic::source(ext, 11, max - (4 << 10));
        assert!(source.len() <= max);
        let name = format!("huge.{ext}");
        files(&h, &[], &[(name.as_str(), source.clone())]);
        let results = record(&h);
        let checks = Arc::new(AtomicUsize::new(0));
        let (c, k) = (checks.clone(), kernel.clone());
        on_yield_check(&h, move || {
            if c.fetch_add(1, Ordering::AcqRel) % 200 == 199 {
                k.set_yield(true);
            }
        });
        let oid = h.git.oid(&source);
        open_far(&h);
        for _ in 0..10_000 {
            if !results.done(oid).is_empty() {
                break;
            }
            kernel.set_yield(false);
            h.settle();
        }
        assert!(results.parks(oid) >= 2, "{ext}: stopped and resumed");
        assert_eq!(
            results.done(oid),
            [whole(&h, lang, &source)],
            "{ext}: the same unit"
        );
    }
}

#[test]
fn yield_language_pack_is_given_up_and_redone() {
    let (h, clock, kernel) = harness(1);
    const SOURCE: &[u8] = b"class Packed { void run() { helper(); } }\n";
    h.git.add("main", &[("lib/Packed.jpack", SOURCE)]);
    h.git.commit("main");
    h.grammars
        .install("jpack", &["jpack"], java_pack(1, Arc::new(())));
    let results = record(&h);
    // The pack's extraction is registered with the slot already over: it
    // can't keep its state, so it is given up.
    let k = clock.clone();
    let once = Arc::new(AtomicBool::new(false));
    let o = once.clone();
    on_yield_check(&h, move || {
        if !o.swap(true, Ordering::AcqRel) {
            k.advance(PAST_SLOT);
        }
    });
    kernel.set_yield(true);
    let oid = h.git.oid(SOURCE);
    open_far(&h);
    settle_until(&h, &clock, 20, || !results.done(oid).is_empty());
    assert!(results.parks(oid) >= 1, "given up once");
    assert_eq!(results.done(oid).len(), 1, "then done again from scratch");
}

#[test]
fn yield_parked_time_is_not_deadline_time() {
    let (h, _clock, kernel) = harness(1);
    // Over 256 KiB: once stopped it waits for the switch to turn off.
    let source = big_ts(3, 300 << 10);
    files(&h, &[], &[("big.ts", source.clone())]);
    let results = record(&h);
    let checks = Arc::new(AtomicUsize::new(0));
    let (c, k) = (checks.clone(), kernel.clone());
    on_yield_check(&h, move || {
        if c.fetch_add(1, Ordering::AcqRel) == 5 {
            k.set_yield(true);
        }
    });
    let oid = h.git.oid(&source);
    open_far(&h);
    assert_eq!(results.parks(oid), 1, "stopped in the parse");
    assert!(!stored(&h, &source));
    // A second parked: the deadline (250 ms) would be long gone.
    h.clock.advance(Duration::from_secs(1));
    kernel.set_yield(false);
    h.settle();
    let done = results.done(oid);
    assert_eq!(done.len(), 1);
    assert!(stored(&h, &source));
}

#[test]
fn yield_large_file_waits_for_the_switch() {
    let (h, clock, kernel) = harness(1);
    let big = big_ts(5, 300 << 10);
    let small: Vec<Vec<u8>> = (0..3).map(|i| ts(&format!("s{i}"))).collect();
    let names = ["b0.ts", "b1.ts", "b2.ts"];
    let mut lib: Vec<(&str, Vec<u8>)> = names.iter().copied().zip(small.iter().cloned()).collect();
    lib.push(("z_big.ts", big.clone()));
    files(&h, &[], &lib);
    kernel.set_yield(true);
    open_far(&h);
    // Slots go by: the small files are done in them, one each; the large
    // one, next in the queue, never enters one.
    for _ in 0..10 {
        clock.advance(PAST_SLOT);
        h.settle();
    }
    assert!(small.iter().all(|s| stored(&h, s)));
    assert!(!stored(&h, &big), "over 256 KiB: no slot");
    kernel.set_yield(false);
    h.settle();
    assert!(stored(&h, &big), "done once the switch is off");
}

#[test]
fn yield_pause_drops_the_parked_state() {
    let (h, _clock, kernel) = harness(1);
    let source = big_ts(4, 300 << 10);
    files(&h, &[], &[("big.ts", source.clone())]);
    let results = record(&h);
    let checks = Arc::new(AtomicUsize::new(0));
    let (c, k) = (checks.clone(), kernel.clone());
    on_yield_check(&h, move || {
        if c.fetch_add(1, Ordering::AcqRel) == 5 {
            k.set_yield(true);
        }
    });
    let oid = h.git.oid(&source);
    open_far(&h);
    assert_eq!(results.parks(oid), 1);
    assert!(queued(&h, 1).is_empty(), "parked, out of the queue");
    // The pause wins: the saved state goes and the file is queued again.
    h.pause();
    h.settle();
    assert_eq!(queued(&h, 1), ["lib/big.ts"]);
    assert_eq!(kernel.background().state, BackgroundState::Paused);
    h.resume();
    kernel.set_yield(false);
    h.settle();
    assert_eq!(results.done(oid).len(), 1);
    assert!(stored(&h, &source));
}

#[test]
fn yield_pause_reaches_all_paused() {
    let (h, clock, kernel) = harness(2);
    let a: Vec<Vec<u8>> = (0..4).map(|i| ts(&format!("a{i}"))).collect();
    let l: Vec<Vec<u8>> = (0..40).map(|i| ts(&format!("l{i}"))).collect();
    let an: Vec<String> = (0..4).map(|i| format!("x{i}.ts")).collect();
    let ln: Vec<String> = (0..40).map(|i| format!("y{i}.ts")).collect();
    let app: Vec<(&str, Vec<u8>)> = an
        .iter()
        .map(String::as_str)
        .zip(a.iter().cloned())
        .collect();
    let lib: Vec<(&str, Vec<u8>)> = ln
        .iter()
        .map(String::as_str)
        .zip(l.iter().cloned())
        .collect();
    files(&h, &app, &lib);
    // Both batch tasks wait for a slot of tier 4.
    kernel.set_yield(true);
    open_far(&h);
    clock.advance(PAST_SLOT);
    h.settle();
    assert_eq!(kernel.background().count, 2, "one guard per task");
    // Tier 3 comes, and the pause in the middle of its first batch.
    let k = kernel.clone();
    let paused = Arc::new(AtomicBool::new(false));
    let p = paused.clone();
    h.set_hook(move |_, _| {
        if !p.swap(true, Ordering::AcqRel) {
            k.pause();
        }
    });
    focus(&h, "app/x0.ts", &a[0]);
    h.settle();
    let status = kernel.background();
    assert_eq!(status.state, BackgroundState::Paused, "{status:?}");
    assert_eq!(status.count, 2);
    h.resume();
    kernel.set_yield(false);
    h.settle();
    assert!(a.iter().chain(&l).all(|s| stored(&h, s)));
}

#[test]
fn yield_large_parked_item_gives_way_to_tier3() {
    let (h, _clock, kernel) = harness(1);
    let parked = big_ts(8, 300 << 10);
    let tier3 = big_ts(9, 300 << 10);
    let current = ts("current");
    // `a/` comes first in the walk: its large file is the first extracted
    // while everything is tier 4; `z/` becomes tier 3 later.
    h.git.add(
        "main",
        &[
            ("a/big.ts", &parked),
            ("z/current.ts", &current),
            ("z/huge.ts", &tier3),
        ],
    );
    h.git.commit("main");
    let results = Arc::new(Results::default());
    // The first far extraction (a/big.ts) stops early and keeps the large
    // slot.
    let once = Arc::new(AtomicBool::new(false));
    let (o, k) = (once.clone(), kernel.clone());
    on_yield_check(&h, move || {
        if !o.swap(true, Ordering::AcqRel) {
            k.set_yield(true);
        }
    });
    // While tier 3 extracts its large file, the parked one is back in its
    // queue: its tree is gone.
    let saw = Arc::new(Mutex::new(None));
    let s = saw.clone();
    let inner = Arc::downgrade(h.indexer.inner());
    let key = h.key();
    let tier3_oid = h.git.oid(&tier3);
    let hash = h.git.hash();
    let r = results.clone();
    h.indexer.set_extractor(
        move |lang: &Lang, bytes: &[u8], deadline: &Deadline, cancel: &Arc<AtomicBool>| {
            let oid = crate::git_access::hash_blob(hash, bytes);
            if oid == tier3_oid
                && let Some(inner) = inner.upgrade()
                && let Some(project) = inner.project(&key)
            {
                let st = project.state.lock().unwrap();
                let queue: Vec<String> = st.batch.resume[1]
                    .iter()
                    .map(|i| i.path.to_string())
                    .collect();
                *s.lock().unwrap() = Some(queue);
            }
            let extraction = extract(lang, bytes, deadline, cancel);
            r.statuses.lock().unwrap().push((oid, extraction.status));
            extraction
        },
    );
    h.publish("main", packages(&["a", "z"]));
    h.settle();
    let parked_oid = h.git.oid(&parked);
    assert_eq!(results.parks(parked_oid), 1, "parked with the large slot");
    assert!(!stored(&h, &tier3));
    // A current text in `z/`: its large file is tier 3 now.
    focus(&h, "z/current.ts", &current);
    h.settle();
    assert!(stored(&h, &tier3), "tier 3 didn't wait for the switch");
    assert_eq!(
        saw.lock().unwrap().clone(),
        Some(vec!["a/big.ts".to_owned()]),
        "the parked file gave the slot up"
    );
    assert!(!stored(&h, &parked));
    kernel.set_yield(false);
    h.settle();
    assert!(stored(&h, &parked));
}

#[test]
fn yield_switch_toggled_100_times_writes_each_file_once() {
    let (h, clock, kernel) = harness(2);
    let sources: Vec<Vec<u8>> = (0..30)
        .map(|i| synthetic::source("ts", 100 + i, 24 << 10))
        .collect();
    let names: Vec<String> = (0..30).map(|i| format!("f{i:02}.ts")).collect();
    let lib: Vec<(&str, Vec<u8>)> = names
        .iter()
        .map(String::as_str)
        .zip(sources.iter().cloned())
        .collect();
    files(&h, &[], &lib);
    let results = record(&h);
    let toggles = Arc::new(AtomicUsize::new(0));
    let checks = Arc::new(AtomicUsize::new(0));
    let (t, c, k, cl) = (
        toggles.clone(),
        checks.clone(),
        kernel.clone(),
        clock.clone(),
    );
    on_yield_check(&h, move || {
        if c.fetch_add(1, Ordering::AcqRel) % 7 == 6 && t.load(Ordering::Acquire) < 100 {
            let n = t.fetch_add(1, Ordering::AcqRel);
            let on = n % 2 == 0;
            if on {
                cl.advance(PAST_SLOT);
            }
            k.set_yield(on);
        }
    });
    open_far(&h);
    settle_until(&h, &clock, 400, || sources.iter().all(|s| stored(&h, s)));
    kernel.set_yield(false);
    h.settle();
    assert!(toggles.load(Ordering::Acquire) > 10, "toggled");
    for source in &sources {
        let oid = h.git.oid(source);
        assert_eq!(results.done(oid).len(), 1, "extracted to the end once");
        assert!(stored(&h, source));
    }
}
