//! `derived_*`: one group per input of「输入代数与派生状态」. Changing only
//! that input recomputes what depends on it and nothing else (the
//! indexer's counters are the counting hooks).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lectern_services::code_intel::{Focus, TextOrigin, TextRef};

use crate::index::StatCounts;
use crate::map::WorktreeState;
use crate::store::Version;

use super::support::{CHANGED, Harness, packages, ts};
use super::warm::fill;

/// Which derived states a change recomputed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Recomputed {
    base: bool,
    compare: bool,
    cursor: bool,
}

fn recomputed(before: StatCounts, after: StatCounts) -> Recomputed {
    Recomputed {
        base: after.base_builds > before.base_builds,
        compare: after.compares > before.compares,
        cursor: after.cursor_builds > before.cursor_builds,
    }
}

/// A repository with sub-projects `app` and `lib`, indexed.
fn indexed() -> Harness {
    let h = Harness::new();
    h.git.add(
        "main",
        &[
            ("app/a.ts", &ts("a")),
            ("app/b.ts", &ts("b")),
            ("lib/c.ts", &ts("c")),
        ],
    );
    h.git.commit("main");
    h.open_with("main", packages(&["app", "lib"]));
    h
}

#[test]
fn derived_files_rebuild_the_base_and_drop_equal_overlay_entries() {
    let h = indexed();
    h.git.write("main", "app/a.ts", &ts("a2"));
    h.emit(&[("app/a.ts", CHANGED)]);
    h.settle();
    assert!(matches!(
        h.map().overlay.get("app/a.ts"),
        Some(WorktreeState::Content(_))
    ));
    let before = h.stats();
    // Staging it gives a new file list whose entry equals the worktree.
    h.git.stage("main", "app/a.ts");
    h.refresh_files();
    assert_eq!(
        recomputed(before, h.stats()),
        Recomputed {
            base: true,
            compare: true,
            cursor: true
        }
    );
    assert!(
        h.map().overlay.is_empty(),
        "{:?}",
        h.map().overlay.entries()
    );
    assert_eq!(
        h.map().effective("app/a.ts").map(|e| e.oid),
        Some(h.git.oid(&ts("a2")))
    );
}

#[test]
fn derived_packages_only_rebuild_tier_3() {
    let h = indexed();
    let wt = h.git.worktree_key("main");
    h.indexer.set_focus(
        &h.key(),
        Some(&wt),
        Focus::Current(Some(TextRef {
            worktree: Some(wt.clone()),
            path: "lib/c.ts".into(),
            oid: h.git.oid(&ts("c")),
            origin: TextOrigin::Worktree,
        })),
    );
    h.settle();
    assert_eq!(h.indexer.cursor_scope(&h.key()).as_deref(), Some("lib"));
    let before = h.stats();
    let reread = before.rereads;
    // The sub-projects change, the file list does not.
    h.set_packages(&["app", "lib/inner", "lib"]);
    assert_eq!(
        recomputed(before, h.stats()),
        Recomputed {
            base: false,
            compare: false,
            cursor: true
        }
    );
    assert_eq!(h.stats().rereads, reread);
    assert_eq!(h.indexer.cursor_scope(&h.key()).as_deref(), Some("lib"));
    h.set_packages(&["app"]);
    // No sub-project contains the current text: tier 3 is empty.
    assert_eq!(h.indexer.cursor_scope(&h.key()).as_deref(), Some("empty"));
    h.set_packages(&[]);
    // No sub-projects: tier 3 is the whole worktree.
    assert_eq!(h.indexer.cursor_scope(&h.key()).as_deref(), Some("whole"));
    assert_eq!(h.stats().base_builds, before.base_builds);
}

#[test]
fn derived_head_only_compares_again_and_head_clean_follows() {
    let h = indexed();
    // A change staged, not committed: equal to the index, not to HEAD.
    h.git.add("main", &[("app/a.ts", &ts("a_staged"))]);
    h.refresh_files();
    assert!(!h.indexer.head_clean(&h.key(), "app/a.ts"));
    assert!(h.indexer.head_clean(&h.key(), "app/b.ts"));
    let before = h.stats();
    // The commit changes HEAD only: the index and the file list stay.
    h.git.commit("main");
    let during = Arc::new(AtomicBool::new(true));
    {
        let d = during.clone();
        let inner = h.indexer.inner().clone();
        let key = h.key();
        // Until the new comparison finishes, the path is not clean.
        h.git.before_compare(move || {
            d.store(inner.head_clean(&key, "app/a.ts"), Ordering::Release);
        });
    }
    h.emit_git(&[("HEAD", CHANGED)]);
    h.settle();
    assert!(
        !during.load(Ordering::Acquire),
        "not clean before the comparison ends"
    );
    assert_eq!(
        recomputed(before, h.stats()),
        Recomputed {
            base: false,
            compare: true,
            cursor: false
        }
    );
    assert!(h.indexer.head_clean(&h.key(), "app/a.ts"));
    assert!(h.indexer.head_clean(&h.key(), "app/b.ts"));
}

#[test]
fn derived_index_only_realigns_head_diff_and_overlay() {
    let h = indexed();
    h.git.write("main", "app/a.ts", &ts("a2"));
    h.emit(&[("app/a.ts", CHANGED)]);
    h.settle();
    let before = h.stats();
    // Only staged: the index checksum changes; the file list is the old one
    // until `project` publishes it.
    h.git.stage("main", "app/a.ts");
    h.emit_git(&[("index", CHANGED)]);
    h.settle();
    assert_eq!(
        recomputed(before, h.stats()),
        Recomputed {
            base: false,
            compare: true,
            cursor: false
        }
    );
    assert!(h.indexer.compared_current(&h.key()));
    // The HEAD difference lists the staged path with its HEAD OID.
    let diff = h.indexer.head_diff(&h.key()).expect("a diff");
    assert_eq!(diff.head_oid("app/a.ts"), Some(Some(h.git.oid(&ts("a")))));
    assert!(!h.indexer.head_clean(&h.key(), "app/a.ts"));
}

#[test]
fn derived_overlay_hint_follows_the_effective_oid() {
    let h = indexed();
    assert!(h.hinted("app/a.ts"));
    let kernel = h.host.kernel();
    // Paused: the reconcile runs, the batch tiers wait.
    kernel.pause();
    h.git.write("main", "app/a.ts", &ts("a2"));
    h.emit(&[("app/a.ts", CHANGED)]);
    h.settle();
    assert_eq!(
        h.map().effective("app/a.ts").map(|e| e.oid),
        Some(h.git.oid(&ts("a2")))
    );
    assert!(!h.hinted("app/a.ts"), "the new OID is not indexed yet");
    kernel.resume();
    h.settle();
    assert!(h.hinted("app/a.ts"));
    assert!(h.stored(h.git.oid(&ts("a2")), "ts"));
}

#[test]
fn derived_rules_drop_newly_ignored_untracked_files() {
    for capped in [false, true] {
        let h = indexed();
        h.git.write("main", "app/gen.ts", &ts("gen"));
        h.emit(&[("app/gen.ts", CHANGED)]);
        h.settle();
        assert!(h.map().overlay.get("app/gen.ts").is_some());
        h.git.ignore("app/gen.ts");
        if capped {
            h.git.cap_untracked(Some(0));
        }
        let before = h.stats();
        h.git.write("main", ".gitignore", b"app/gen.ts\n");
        h.emit(&[(".gitignore", CHANGED)]);
        h.settle();
        assert!(recomputed(before, h.stats()).compare);
        let in_map = h.map().paths(&h.git.oid(&ts("gen")), "ts");
        if capped {
            // An incomplete comparison removes nothing.
            assert_eq!(in_map.len(), 1);
        } else {
            assert!(in_map.is_empty(), "an ignored file leaves the map");
            assert!(h.map().overlay.get("app/gen.ts").is_none());
        }
    }
}

#[test]
fn derived_grammar_rebuilds_base_hints_and_cursor() {
    let h = indexed();
    let before = h.stats();
    h.grammars.install(
        "jpack",
        &["jpack"],
        crate::testing::java_pack(1, Arc::new(())),
    );
    h.settle();
    assert_eq!(
        recomputed(before, h.stats()),
        Recomputed {
            base: true,
            compare: true,
            cursor: true
        }
    );
}

#[test]
fn derived_eviction_rechecks_and_reextracts_only_what_went() {
    let h = Harness::new();
    let files = fill(&h, 21);
    h.open();
    assert_eq!(h.extractions(), 21);
    let store = h.indexer.store(&h.key()).expect("store");
    let generation = store.evictions();
    let table = crate::extract::LanguageTable::core();
    let gone: Vec<_> = files.iter().step_by(3).cloned().collect();
    let mut write = store.write();
    for (_, oid, lang) in &gone {
        write.remove(*oid, &Version::of(table.get(lang).expect("lang")));
    }
    assert!(matches!(write.commit(), lectern_cache::Written::Committed));
    assert_ne!(store.evictions(), generation);
    // Every hint stops matching.
    for (path, _, _) in &files {
        assert!(!h.hinted(path), "{path}");
    }
    let before = h.stats();
    h.indexer.demand(&h.key());
    h.settle();
    assert!(h.stats().cursor_builds > before.cursor_builds);
    let mut again: Vec<_> = h.extracted()[21..].to_vec();
    again.sort();
    let mut want: Vec<_> = gone.iter().map(|(_, o, _)| *o).collect();
    want.sort();
    assert_eq!(again, want, "only the evicted units are extracted again");
    for (path, _, _) in &files {
        assert!(h.hinted(path), "{path}");
    }
}

#[test]
fn derived_writes_bump_their_generation() {
    let h = Harness::new();
    h.git.add("main", &[("a.ts", &ts("a"))]);
    h.git.commit("main");
    h.open();
    let writes = h.indexer.gens(&h.key()).expect("gens").writes;
    assert!(writes >= 1);
    h.git.write("main", "b.ts", &ts("b"));
    h.emit(&[("b.ts", CHANGED)]);
    h.settle();
    assert_eq!(h.indexer.gens(&h.key()).expect("gens").writes, writes + 1);
}

#[test]
fn derived_stale_result_is_not_installed() {
    let h = indexed();
    let compares = h.stats().compares;
    let key = h.key();
    let inner = h.indexer.inner().clone();
    h.git
        .before_compare(move || inner.bump(&key, |g| g.rules += 1));
    h.git.write("main", ".gitignore", b"x\n");
    h.emit(&[(".gitignore", CHANGED)]);
    h.settle();
    // The first comparison saw the old rules generation: dropped, done
    // again.
    assert_eq!(h.stats().compares, compares + 2);
    assert!(h.indexer.compared_current(&h.key()));
}
