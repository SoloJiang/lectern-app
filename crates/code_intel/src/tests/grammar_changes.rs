//! `grammar_*`: language packs installed, updated, rolled back and removed
//! while a project is open (「语言包的变化」), with `testing::FakeGrammars`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use lectern_services::code_intel::TierState;

use crate::extract::{ExtractStatus, Lang};
use crate::index::lock;
use crate::site::ParsedFile;
use crate::store::Version;
use crate::testing::java_pack;
use crate::{GrammarChange, GrammarChangeKind, lang_name};

use super::support::{Gate, Harness, ts};

const TRACKED: &[u8] = b"class Tracked { void run() { helper(); } }\n";
const UNTRACKED: &[u8] = b"class Untracked { int size() { return 1; } }\n";

/// A repository with one tracked and one untracked `.jpack` file and a
/// TypeScript file, opened and indexed.
fn repo() -> Harness {
    let h = Harness::new();
    h.git.add(
        "main",
        &[("src/Tracked.jpack", TRACKED), ("src/a.ts", &ts("a"))],
    );
    h.git.commit("main");
    h.git.write("main", "src/Untracked.jpack", UNTRACKED);
    h.open();
    h
}

fn pack(h: &Harness) -> Option<Lang> {
    lock(&h.indexer.inner().langs).table.get("jpack").cloned()
}

fn pack_paths(h: &Harness) -> usize {
    let map = h.map();
    [TRACKED, UNTRACKED]
        .iter()
        .map(|c| map.paths(&h.git.oid(c), "jpack").len())
        .sum()
}

fn stored_under(h: &Harness, sha: u8, content: &[u8]) -> bool {
    let mut grammar = [0u8; 8];
    grammar.copy_from_slice(&[sha; 8]);
    let version = Version::new(
        lang_name("jpack"),
        u64::from_be_bytes(grammar),
        pack(h).map_or(0, |l| l.query_version()),
    );
    let store = h.indexer.store(&h.key()).expect("store");
    store
        .exists(&version, &[h.git.oid(content)])
        .is_some_and(|f| !f.is_empty())
}

fn install(h: &Harness, sha: u8) {
    h.grammars
        .install("jpack", &["jpack"], java_pack(sha, Arc::new(())));
}

#[test]
fn grammar_installed_brings_tracked_and_untracked_files_in() {
    let h = repo();
    assert_eq!(pack_paths(&h), 0);
    let before = h.extractions();
    install(&h, 1);
    h.settle();
    assert_eq!(
        pack_paths(&h),
        2,
        "tracked and untracked files enter the map"
    );
    assert_eq!(h.extractions(), before + 2);
    assert!(stored_under(&h, 1, TRACKED));
    assert!(stored_under(&h, 1, UNTRACKED));
    assert!(h.hinted("src/Tracked.jpack"));
    assert!(h.hinted("src/Untracked.jpack"));
}

#[test]
fn grammar_updated_extracts_again_and_drops_in_flight_results() {
    let h = repo();
    // The update arrives while the first extraction under version 1 runs.
    let inner = h.indexer.inner().clone();
    let grammars = h.grammars.clone();
    let once = Arc::new(AtomicBool::new(false));
    let o = once.clone();
    h.set_hook(move |lang, _| {
        if &*lang.name().0 == "jpack" && !o.swap(true, Ordering::AcqRel) {
            grammars.install("jpack", &["jpack"], java_pack(2, Arc::new(())));
            inner.clone().grammar_round();
        }
    });
    install(&h, 1);
    h.settle();
    assert!(
        !stored_under(&h, 1, TRACKED),
        "no result of version 1 is written"
    );
    assert!(
        !stored_under(&h, 1, UNTRACKED),
        "no result of version 1 is written"
    );
    let cancelled = h
        .seen
        .results
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, s)| *s == ExtractStatus::Cancelled)
        .count();
    assert_eq!(cancelled, 1, "the version 1 extraction in flight gave up");
    assert!(stored_under(&h, 2, TRACKED));
    assert!(stored_under(&h, 2, UNTRACKED));
    assert!(h.hinted("src/Tracked.jpack"));
    // A later update: the version 2 units no longer match.
    let before = h.extractions();
    install(&h, 3);
    h.settle();
    assert_eq!(h.extractions(), before + 2);
    assert!(stored_under(&h, 3, TRACKED));
}

#[test]
fn grammar_rolled_back_matches_the_old_units_again() {
    let h = repo();
    install(&h, 1);
    h.settle();
    install(&h, 2);
    h.settle();
    let before = h.extractions();
    // Back to version 1 (ADR 0018's rollback): its units match again.
    install(&h, 1);
    h.settle();
    assert_eq!(h.extractions(), before, "nothing extracted again");
    assert!(h.hinted("src/Tracked.jpack"));
    assert!(h.hinted("src/Untracked.jpack"));
}

#[test]
fn grammar_removed_leaves_the_map_and_cancels_in_flight() {
    let h = repo();
    let inner = h.indexer.inner().clone();
    let grammars = h.grammars.clone();
    let armed = Arc::new(AtomicBool::new(false));
    let a = armed.clone();
    h.set_hook(move |lang, _| {
        if &*lang.name().0 == "jpack" && a.swap(false, Ordering::AcqRel) {
            grammars.remove("jpack");
            inner.clone().grammar_round();
        }
    });
    install(&h, 1);
    h.settle();
    // An edit is extracted when the pack goes away.
    let edited: &[u8] = b"class Tracked { void edited() {} }\n";
    armed.store(true, Ordering::Release);
    h.git.write("main", "src/Tracked.jpack", edited);
    h.emit(&[("src/Tracked.jpack", super::support::CHANGED)]);
    h.settle();
    assert_eq!(h.status(h.git.oid(edited)), Some(ExtractStatus::Cancelled));
    assert!(pack(&h).is_none());
    assert_eq!(pack_paths(&h), 0, "the pack's files leave the map");
    assert!(h.map().effective("src/Tracked.jpack").is_none());
    let c = h.coverage();
    assert_eq!([c.subproject, c.worktree], [TierState::Done; 2]);
}

#[test]
fn grammar_merged_notifications_are_one_round() {
    let h = repo();
    let rounds = h.stats().grammar_rounds;
    // Three changes before anything runs: one round sees the last state.
    install(&h, 1);
    install(&h, 2);
    install(&h, 3);
    h.settle();
    assert_eq!(h.stats().grammar_rounds, rounds + 1);
    assert!(stored_under(&h, 3, TRACKED));
    assert!(!stored_under(&h, 1, TRACKED));
    assert!(!stored_under(&h, 2, TRACKED));
    // The same result as one change at a time.
    let one = repo();
    install(&one, 3);
    one.settle();
    assert_eq!(pack_paths(&one), pack_paths(&h));
    assert_eq!(
        pack(&one).map(|l| l.grammar_version()),
        pack(&h).map(|l| l.grammar_version())
    );
    // A notification no newer than the last handled is dropped.
    let rounds = h.stats().grammar_rounds;
    h.indexer.grammar_changed(GrammarChange {
        generation: 1,
        lang: lang_name("jpack"),
        kind: GrammarChangeKind::Updated,
    });
    h.settle();
    assert_eq!(h.stats().grammar_rounds, rounds);
}

/// Counts the drops of a pack's "module".
struct Module(Arc<AtomicUsize>);

impl Drop for Module {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}

#[test]
fn grammar_lease_unloads_after_the_last_user() {
    let h = repo();
    let unloads = Arc::new(AtomicUsize::new(0));
    h.grammars.install(
        "jpack",
        &["jpack"],
        java_pack(1, Arc::new(Module(unloads.clone()))),
    );
    h.settle();
    // A query's parsed file holds the pack.
    let lang = pack(&h).expect("installed");
    let clock = crate::SystemClock;
    let Ok(parsed) = ParsedFile::parse(
        &lang,
        Arc::from(TRACKED),
        &clock,
        Instant::now() + std::time::Duration::from_secs(10),
        &AtomicBool::new(false),
    ) else {
        panic!("the pack parses the file");
    };
    drop(lang);
    // And an extraction in flight, waiting in the hook.
    let gate = Arc::new(Gate::default());
    let g = gate.clone();
    h.set_hook(move |lang, _| {
        if &*lang.name().0 == "jpack" {
            g.entered.store(true, Ordering::Release);
            while !g.open.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
        }
    });
    let edited: &[u8] = b"class Tracked { void again() {} }\n";
    h.git.write("main", "src/Tracked.jpack", edited);
    h.emit(&[("src/Tracked.jpack", super::support::CHANGED)]);
    h.driver.run_until_idle();
    h.driver.advance(std::time::Duration::from_secs(5));
    let executor = h.executor.clone();
    let inner = h.indexer.inner().clone();
    std::thread::scope(|s| {
        let worker = s.spawn(move || {
            executor.run_until_idle();
        });
        gate.wait_entered();
        // Removed while both are in use: not unloaded.
        h.grammars.remove("jpack");
        inner.clone().grammar_round();
        assert_eq!(unloads.load(Ordering::Acquire), 0);
        gate.open.store(true, Ordering::Release);
        worker.join().expect("the executor thread");
    });
    h.settle();
    assert_eq!(
        unloads.load(Ordering::Acquire),
        0,
        "the parsed file still holds it"
    );
    drop(parsed);
    assert_eq!(unloads.load(Ordering::Acquire), 1, "unloaded exactly once");
}
