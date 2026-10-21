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

/// The paths set aside in tier `t` of the open project, for the switch to
/// turn off.
fn aside(h: &Harness, t: usize) -> Vec<String> {
    let project = h.indexer.inner().project(&h.key()).expect("open");
    let st = project.state.lock().unwrap();
    st.batch.aside[t]
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
    langs: Mutex<HashMap<BlobId, Lang>>,
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
            r.langs.lock().unwrap().insert(oid, lang.clone());
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

    /// The OIDs extracted to the end, in order.
    fn done_order(&self) -> Vec<BlobId> {
        self.statuses
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, s)| *s == ExtractStatus::Done)
            .map(|(o, _)| *o)
            .collect()
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
    // The pack's file comes first in tier 4's walk, small files after it.
    let small: Vec<Vec<u8>> = (0..3).map(|i| ts(&format!("q{i}"))).collect();
    h.git.add("main", &[("lib/Packed.jpack", SOURCE)]);
    for (i, s) in small.iter().enumerate() {
        h.git.add("main", &[(format!("lib/q{i}.ts").as_str(), s)]);
    }
    h.git.commit("main");
    h.grammars
        .install("jpack", &["jpack"], java_pack(1, Arc::new(())));
    let results = record(&h);
    // The pack's extraction outlasts its slot (the slot's end comes at its
    // first check): it can't keep its state, so it is set aside.
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
    // The rest of the tier goes on in slots meanwhile.
    settle_until(&h, &clock, 20, || small.iter().all(|s| stored(&h, s)));
    for _ in 0..5 {
        clock.advance(PAST_SLOT);
        h.settle();
    }
    assert_eq!(results.parks(oid), 1, "given up once, not tried again");
    assert!(results.done(oid).is_empty());
    assert_eq!(aside(&h, 1), ["lib/Packed.jpack"]);
    // Redone from scratch once the interaction is over.
    kernel.set_yield(false);
    h.settle();
    assert!(aside(&h, 1).is_empty());
    let lang = results.langs.lock().unwrap()[&oid].clone();
    let whole = extract(
        &lang,
        SOURCE,
        &Deadline::background(&h.indexer.inner().watchdog),
        &Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(whole.status, ExtractStatus::Done);
    assert_eq!(results.done(oid), [whole], "the same unit");
    assert!(stored_pack(&h, oid, &lang));
}

/// Whether `oid` is stored for the pack language `lang`.
fn stored_pack(h: &Harness, oid: BlobId, lang: &Lang) -> bool {
    let store = h.indexer.store(&h.key()).expect("an open project");
    let version = crate::store::Version::of(lang);
    store
        .exists(&version, &[oid])
        .is_some_and(|found| !found.is_empty())
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

/// `count` CRLF checkouts under `lib/` (tier 4), committed with LF; their
/// LF sources.
fn crlf_lib(h: &Harness, count: usize) -> Vec<Vec<u8>> {
    let sources: Vec<Vec<u8>> = (0..count).map(|i| ts(&format!("l{i}"))).collect();
    for (i, source) in sources.iter().enumerate() {
        let path = format!("lib/l{i}.ts");
        h.git.eol(&path);
        let crlf = String::from_utf8(source.clone())
            .expect("UTF-8")
            .replace('\n', "\r\n");
        h.git.add("main", &[(&path, crlf.as_bytes())]);
    }
    h.git.commit("main");
    sources
}

/// A keepalive slot does one file: a CRLF checkout read in it is converted
/// on its own at read time, inside the slot and before its extraction
/// (「规范内容 OID 的批量核对」), never after it; stored under its index OID.
#[test]
fn yield_slot_converts_its_one_file() {
    let (h, clock, kernel) = harness(1);
    // First in the queue, over 256 KiB: set aside unconverted.
    let big = big_ts(7, 300 << 10);
    h.git.eol("lib/a_big.ts");
    let big_crlf = String::from_utf8(big.clone())
        .expect("UTF-8")
        .replace('\n', "\r\n");
    h.git.add("main", &[("lib/a_big.ts", big_crlf.as_bytes())]);
    let sources = crlf_lib(&h, 3);
    let converted = Arc::new(Mutex::new(Vec::new()));
    let seen = converted.clone();
    let git = h.git.clone();
    // At each extraction's start: how many files were converted by then.
    h.set_hook(move |_, _| seen.lock().unwrap().push(git.count("canonical_oid")));
    kernel.set_yield(true);
    open_far(&h);
    settle_until(&h, &clock, 20, || sources.iter().all(|s| stored(&h, s)));
    assert!(h.git.canonical_batches().is_empty(), "no batch in a slot");
    // The small files only: the large one was not converted in a slot.
    assert_eq!(h.git.count("canonical_oid"), sources.len());
    assert_eq!(aside(&h, 1), ["lib/a_big.ts"]);
    assert!(!stored(&h, &big));
    // The n-th extraction started after the n-th conversion.
    let converted = converted.lock().unwrap().clone();
    assert_eq!(converted, (1..=sources.len()).collect::<Vec<_>>());
}

/// The switch comes on during a batch: the batch makes no index parse
/// after it. Its files read with another OID than queued are set aside
/// (their extraction dropped), done and converted in one batch once the
/// switch is off; the batch's other files are committed as usual.
#[test]
fn yield_switch_on_sets_unconverted_files_aside() {
    let (h, _clock, kernel) = harness(1);
    let sources = crlf_lib(&h, 6);
    let plain: Vec<Vec<u8>> = (0..2).map(|i| ts(&format!("p{i}"))).collect();
    for (i, source) in plain.iter().enumerate() {
        h.git.add("main", &[(&format!("lib/p{i}.ts"), source)]);
    }
    h.git.commit("main");
    let started = Arc::new(AtomicUsize::new(0));
    let on = Arc::new(Mutex::new(None));
    let (k, n, at) = (kernel.clone(), started.clone(), on.clone());
    let git = h.git.clone();
    h.set_hook(move |_, _| {
        // The fourth extraction of the batch: the switch comes on.
        if n.fetch_add(1, Ordering::SeqCst) == 3 {
            *at.lock().unwrap() = Some(git.canonical_batches().len());
            k.set_yield(true);
        }
    });
    open_far(&h);
    let batches_at_switch = on.lock().unwrap().expect("the switch came on");
    assert_eq!(
        h.git.canonical_batches().len(),
        batches_at_switch,
        "no batch conversion once the switch is on"
    );
    assert!(!aside(&h, 1).is_empty(), "CRLF files set aside");
    assert!(
        sources.iter().any(|s| !stored(&h, s)),
        "set aside, not committed"
    );
    kernel.set_yield(false);
    h.settle();
    for source in sources.iter().chain(&plain) {
        assert!(stored(&h, source), "done once the switch is off");
    }
    assert!(aside(&h, 1).is_empty());
    assert!(h.git.canonical_batches().len() > batches_at_switch);
}

#[test]
fn yield_large_file_waits_for_the_switch() {
    let (h, clock, kernel) = harness(1);
    let big = [big_ts(5, 300 << 10), big_ts(6, 300 << 10)];
    let small: Vec<Vec<u8>> = (0..3).map(|i| ts(&format!("s{i}"))).collect();
    // The large files come first in the queue, the small ones after them.
    let mut lib: Vec<(&str, Vec<u8>)> =
        vec![("a_big0.ts", big[0].clone()), ("a_big1.ts", big[1].clone())];
    let names = ["b0.ts", "b1.ts", "b2.ts"];
    lib.extend(names.iter().copied().zip(small.iter().cloned()));
    files(&h, &[], &lib);
    let results = record(&h);
    kernel.set_yield(true);
    open_far(&h);
    // Slots go by: the large files never enter one and are set aside; the
    // small files behind them are done, one a slot.
    for _ in 0..10 {
        clock.advance(PAST_SLOT);
        h.settle();
    }
    assert!(small.iter().all(|s| stored(&h, s)), "the tier went on");
    assert!(big.iter().all(|b| !stored(&h, b)), "over 256 KiB: no slot");
    assert_eq!(aside(&h, 1), ["lib/a_big0.ts", "lib/a_big1.ts"]);
    let before = results.done_order().len();
    kernel.set_yield(false);
    h.settle();
    assert!(
        big.iter().all(|b| stored(&h, b)),
        "done once the switch is off"
    );
    assert!(aside(&h, 1).is_empty());
    let oids: Vec<BlobId> = big.iter().map(|b| h.git.oid(b)).collect();
    assert_eq!(results.done_order()[before..], oids, "in their order");
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
    // One batch task (the task's own parked item gives way) and two (the
    // tier 3 work takes the slot over from the other task's parked item,
    // without waiting for that task to run: on the manual executor both
    // tasks share one thread).
    for tasks in [1, 2] {
        let (h, clock, kernel) = harness(tasks);
        let parked = big_ts(8, 300 << 10);
        let tier3 = big_ts(9, 300 << 10);
        let current = ts("current");
        // `z/` is tier 3 (its current text) and done first; `a/big.ts`, the
        // only tier 4 file, stops early with the large slot and waits for
        // the switch to turn off, its state kept.
        h.git
            .add("main", &[("a/big.ts", &parked), ("z/current.ts", &current)]);
        h.git.commit("main");
        let results = Arc::new(Results::default());
        let once = Arc::new(AtomicBool::new(false));
        let (o, k) = (once.clone(), kernel.clone());
        on_yield_check(&h, move || {
            if !o.swap(true, Ordering::AcqRel) {
                k.set_yield(true);
            }
        });
        // While tier 3 extracts its large file, no parked state is left
        // with the large slot: the parked tree is gone.
        let saw = Arc::new(Mutex::new(None));
        let s = saw.clone();
        let inner = Arc::downgrade(h.indexer.inner());
        let tier3_oid = h.git.oid(&tier3);
        let hash = h.git.hash();
        let r = results.clone();
        h.indexer.set_extractor(
            move |lang: &Lang, bytes: &[u8], deadline: &Deadline, cancel: &Arc<AtomicBool>| {
                let oid = crate::git_access::hash_blob(hash, bytes);
                if oid == tier3_oid
                    && let Some(inner) = inner.upgrade()
                {
                    *s.lock().unwrap() = Some(inner.large.parked());
                }
                let extraction = extract(lang, bytes, deadline, cancel);
                r.statuses.lock().unwrap().push((oid, extraction.status));
                extraction
            },
        );
        h.publish("main", packages(&["a", "z"]));
        focus(&h, "z/current.ts", &current);
        h.settle();
        let parked_oid = h.git.oid(&parked);
        assert!(stored(&h, &current));
        assert_eq!(results.parks(parked_oid), 1, "{tasks}: parked");
        assert!(h.indexer.inner().large.parked(), "{tasks}: with the slot");
        // A large file in `z/`: tier 3.
        h.git.add("main", &[("z/huge.ts", &tier3)]);
        h.git.commit("main");
        h.refresh_files();
        assert!(stored(&h, &tier3), "{tasks}: tier 3 didn't wait");
        assert_eq!(*saw.lock().unwrap(), Some(false), "{tasks}: state gone");
        assert!(!h.indexer.inner().large.busy(), "{tasks}: slot free");
        // The parked file is back in tier 4; the next slot sets it aside
        // for the switch.
        assert!(!stored(&h, &parked));
        clock.advance(PAST_SLOT);
        h.settle();
        assert_eq!(aside(&h, 1), ["a/big.ts"], "{tasks}");
        kernel.set_yield(false);
        h.settle();
        assert!(stored(&h, &parked), "{tasks}");
    }
}

/// The large slot changes hands between two batch tasks without the
/// holder running: its parked state is dropped by the non-yielding
/// acquirer, its turn revoked (so its drop doesn't free the new turn).
#[test]
fn yield_large_slot_taken_over_from_a_parked_task() {
    use crate::index::batch::LargeSlot;
    use std::sync::mpsc;

    let h = Harness::new();
    let watchdog = h.indexer.inner().watchdog.clone();
    let source = big_ts(14, 300 << 10);
    let lang = crate::extract::LanguageTable::core()
        .get("ts")
        .cloned()
        .expect("ts");
    let slot = Arc::new(LargeSlot::default());
    let resume: crate::extract::ResumeSlot = Arc::default();
    let (parked_tx, parked_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    std::thread::scope(|s| {
        // The first task: extracts with the slot, stops to yield, and then
        // doesn't run until told.
        let (slot, resume, watchdog, lang, source) = (&slot, &resume, &watchdog, &lang, &source);
        let holder = s.spawn(move || {
            let turn = slot.acquire(|| false, false, || {}).expect("free");
            let calls = AtomicUsize::new(0);
            let probe = Arc::new(move || calls.fetch_add(1, Ordering::AcqRel) >= 5);
            let deadline = Deadline::background(watchdog).yielding(probe, resume.clone());
            let e = extract(lang, source, &deadline, &Arc::new(AtomicBool::new(false)));
            assert_eq!(e.status, ExtractStatus::Parked);
            turn.park(resume);
            parked_tx.send(()).unwrap();
            go_rx.recv().unwrap();
            assert!(turn.revoked(), "taken over");
            assert!(!turn.unpark(), "its state is gone");
            // Dropping a revoked turn frees nothing.
            drop(turn);
        });
        parked_rx.recv().unwrap();
        assert!(resume.lock().unwrap().is_some(), "the state is kept");
        // The second task (tier 3) takes the slot at once.
        let woke = AtomicBool::new(false);
        let turn = slot
            .acquire(|| false, true, || woke.store(true, Ordering::Release))
            .expect("taken over");
        assert!(resume.lock().unwrap().is_none(), "the parked tree went");
        assert!(woke.load(Ordering::Acquire), "the waiting tasks are woken");
        go_tx.send(()).unwrap();
        holder.join().unwrap();
        assert!(slot.busy(), "still the second task's");
        // A holder that isn't parked keeps the slot, even from tier 3.
        let tries = AtomicUsize::new(0);
        let stop = || tries.fetch_add(1, Ordering::AcqRel) >= 2;
        assert!(slot.acquire(stop, true, || {}).is_none());
        drop(turn);
        assert!(!slot.busy());
        assert!(slot.acquire(|| false, false, || {}).is_some());
    });
}

/// A parked item of the project that ran tiers 3–5 is given up when
/// another project takes them over: that project's tier 3 work doesn't wait
/// behind the yielding boundary, and the large slot is free again.
#[test]
fn yield_parked_item_of_another_project_is_given_up() {
    use lectern_services::project::{
        ActiveTree, FileKind, FileList, ProjectKey, ProjectSnapshot, Scan,
    };

    let (h, _clock, kernel) = harness(1);
    let parked = big_ts(12, 300 << 10);
    files(&h, &[], &[("big.ts", parked.clone())]);
    let results = record(&h);
    // A folder project opens first, with the batch tiers paused; the git
    // project, opened last, runs them.
    h.pause();
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().canonicalize().expect("canonical");
    let huge = big_ts(13, 300 << 10);
    std::fs::write(root.join("huge.ts"), &huge).expect("write");
    let folder = ProjectKey::Folder {
        root: Arc::from(root.as_path()),
    };
    let mut list = FileList::builder();
    list.push("huge.ts", FileKind::File);
    let event = h.projects.set(ProjectSnapshot {
        key: folder.clone(),
        generation: 1,
        name: "folder".into(),
        worktrees: Arc::from(Vec::new()),
        active: Some(ActiveTree {
            root: Arc::from(root.as_path()),
            worktree: None,
            opened_at: "".into(),
            files: Scan::Ready(Arc::new(list.build())),
            packages: Scan::Ready(Arc::from(Vec::new())),
        }),
    });
    h.indexer.project_event(&event);
    h.settle();
    let checks = Arc::new(AtomicUsize::new(0));
    let (c, k) = (checks.clone(), kernel.clone());
    on_yield_check(&h, move || {
        if c.fetch_add(1, Ordering::AcqRel) == 5 {
            k.set_yield(true);
        }
    });
    open_far(&h);
    h.resume();
    h.settle();
    let parked_oid = h.git.oid(&parked);
    assert_eq!(results.parks(parked_oid), 1, "parked with the large slot");
    assert!(h.indexer.inner().large.busy());
    // The folder's large file becomes its current text: the folder runs
    // tiers 3–5 now, the file tier 3.
    let oid = h
        .indexer
        .map(&folder)
        .and_then(|m| m.effective("huge.ts").map(|e| e.oid))
        .expect("read");
    h.indexer.set_focus(
        &folder,
        None,
        Focus::Current(Some(TextRef {
            worktree: None,
            path: "huge.ts".into(),
            oid,
            origin: TextOrigin::Worktree,
        })),
    );
    h.settle();
    let store = h.indexer.store(&folder).expect("open");
    let version = crate::store::Version::of(&results.langs.lock().unwrap()[&parked_oid]);
    assert!(
        store
            .exists(&version, &[oid])
            .is_some_and(|f| !f.is_empty()),
        "the folder's tier 3 didn't wait for the switch"
    );
    assert!(!h.indexer.inner().large.busy(), "the slot is free");
    // The git project's file is back in its queue, its state gone.
    assert_eq!(queued(&h, 1), ["lib/big.ts"]);
    assert!(!stored(&h, &parked));
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
