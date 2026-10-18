//! `prefetch_*` and `focus_*`: a prefetch is reused by the same query
//! within two seconds while the map, the evictions and the language packs
//! stay; not otherwise. A focus of another worktree does not reach tiers 1
//! and 2.

use std::time::Duration;

use lectern_services::code_intel::{Focus, Query, QueryKind, TextOrigin, TextRef};

use crate::query::PREFETCH_FOR;
use crate::store::Version;

use super::goto::indexed;
use super::support::{CHANGED, Harness};

fn jump(h: &Harness) -> Query {
    Query {
        project: h.key(),
        kind: QueryKind::Definition {
            at: h.text("a.ts"),
            position: h.pos("a.ts", "item", 0),
        },
    }
}

fn repo() -> Harness {
    indexed(&[
        ("a.ts", "export function run() { return item(); }\n"),
        ("b.ts", "export function item() { return 1; }\n"),
    ])
}

/// Whether the query reused the prefetch: it ran no steps 1–3 of its own.
fn reused(h: &Harness, query: Query) -> bool {
    let before = h.stats().prepares;
    let id = h.indexer.query(query);
    h.settle();
    let r = h.indexer.results(id).expect("kept");
    assert_eq!(r.candidates.len(), 1, "the same answer either way");
    h.stats().prepares == before
}

#[test]
fn prefetch_is_reused_within_two_seconds() {
    let h = repo();
    let q = jump(&h);
    h.indexer.prefetch(q.clone());
    h.settle();
    assert!(h.indexer.inner().prefetched(&q));
    h.clock.advance(PREFETCH_FOR - Duration::from_millis(1));
    assert!(reused(&h, q.clone()));
    // Used once.
    assert!(!reused(&h, q));
}

/// Each input the prefetch depends on, one row: after it changes the query
/// runs its own steps.
#[test]
fn prefetch_is_not_reused_after_its_inputs_change() {
    type Case = (&'static str, fn(&Harness));
    let cases: [Case; 3] = [
        ("two seconds later", |h| {
            h.clock.advance(PREFETCH_FOR + Duration::from_millis(1))
        }),
        ("a new map", |h| {
            h.git.write("main", "c.ts", b"export const c = 1;\n");
            h.emit(&[("c.ts", CHANGED)]);
            h.settle();
        }),
        ("evictions", |h| {
            let store = h.indexer.store(&h.key()).expect("store");
            let ts = crate::extract::LanguageTable::core()
                .get("ts")
                .cloned()
                .unwrap();
            let mut w = store.write();
            w.remove(
                h.git.oid(b"export function item() { return 1; }\n"),
                &Version::of(&ts),
            );
            assert!(matches!(w.commit(), lectern_cache::Written::Committed));
        }),
    ];
    for (what, change) in cases {
        let h = repo();
        let q = jump(&h);
        h.indexer.prefetch(q.clone());
        h.settle();
        change(&h);
        // The background re-indexes what was evicted: pause it so the
        // answer comes from the search, still one candidate.
        h.pause();
        let q = jump(&h);
        assert!(!reused(&h, q), "{what}");
    }
}

/// A focus set for a worktree that is not the current one does not reach
/// tiers 1 and 2; after switching to it, it does.
#[test]
fn focus_of_another_worktree_waits_for_it() {
    let h = Harness::new();
    h.git
        .add("main", &[("a.ts", b"export function one() {}\n")]);
    h.git.commit("main");
    let other = h.git.add_worktree("other");
    let _ = other;
    h.git
        .add("other", &[("o.ts", b"export function two() {}\n")]);
    h.pause();
    h.open();
    let wt = h.git.worktree_key("other");
    let oid = h.git.oid(b"export function two() {}\n");
    h.indexer.set_focus(
        &h.key(),
        Some(&wt),
        Focus::Current(Some(TextRef {
            worktree: Some(wt.clone()),
            path: "o.ts".into(),
            oid,
            origin: TextOrigin::Worktree,
        })),
    );
    h.settle();
    assert!(!h.extracted().contains(&oid), "not the current worktree");
    h.publish("other", Vec::new());
    h.settle();
    assert!(
        h.extracted().contains(&oid),
        "current now: tier 1 extracts it"
    );
}

/// An unused prefetch goes [`PREFETCH_FOR`] after it was made, with no
/// query or prefetch after it: a due entry on the watchdog's table expires
/// it. One whose inputs changed goes at the next prefetch.
#[test]
fn prefetch_expires_unused() {
    let h = repo();
    let q = jump(&h);
    h.indexer.prefetch(q.clone());
    h.settle();
    assert_eq!(h.indexer.inner().prefetch_count(), 1);
    assert_eq!(h.indexer.inner().watchdog.scheduled(), 1, "its expiry");
    h.clock.advance(PREFETCH_FOR);
    h.settle();
    assert_eq!(h.indexer.inner().prefetch_count(), 1, "still valid");
    h.advance_and_fire(Duration::from_millis(2));
    assert_eq!(h.indexer.inner().prefetch_count(), 0);
    assert_eq!(h.indexer.inner().watchdog.scheduled(), 0, "nothing left");
    // A new map makes it unusable: the next prefetch drops it.
    h.indexer.prefetch(q.clone());
    h.settle();
    h.git.write("main", "c.ts", b"export const c = 1;\n");
    h.emit(&[("c.ts", CHANGED)]);
    h.settle();
    let other = Query {
        project: h.key(),
        kind: QueryKind::Definition {
            at: h.text("b.ts"),
            position: h.pos("b.ts", "item", 0),
        },
    };
    h.indexer.prefetch(other);
    h.settle();
    assert_eq!(h.indexer.inner().prefetch_count(), 1, "only the new one");
}
