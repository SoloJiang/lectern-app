//! `cut_*`: a query's cut (「输入代数与派生状态」). A query looks up the
//! index, then reads hints to leave the indexed files out of its search
//! and its counts. A batch that commits in between installs hints for
//! files the lookup did not see; the query looks up again for them, so a
//! valid candidate is never left out of both. The batch is committed by a
//! test hook between the lookup and the first hint read, on the query's
//! own thread, through the same commit the batch tiers use.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use lectern_services::code_intel::{
    CandidateSource, Completeness, DefinitionRef, Query, QueryKind, QueryResults, QueryState,
};

use crate::extract::{Deadline, extract};
use crate::index::{Inner, Scanned, lock};
use crate::query::{Snap, Step, ViewCheck};

use super::goto::unindexed;
use super::support::Harness;

const RUN: &str = "export function run() { return item(); }\n";
const ITEM: &str = "export function item() { return 2; }\n";
const USE: &str = "export function other() { return item() + 1; }\n";

/// Commits `path`'s extraction as of the snapshot, through the batch
/// tiers' commit: one transaction, then its hints.
fn commit(inner: &Inner, snap: &Snap, path: &str, text: &[u8]) {
    let eff = snap.map.effective(path).expect("in the map");
    let lang = snap
        .table
        .get(&eff.lang.name().0)
        .cloned()
        .expect("a language");
    let deadline = Deadline::background(&inner.watchdog);
    let extraction = extract(&lang, text, &deadline, &Arc::new(AtomicBool::new(false)));
    inner.commit_scanned(
        &snap.project,
        snap.session,
        vec![Scanned {
            path: eff.path,
            oid: eff.oid,
            lang,
            extraction,
        }],
    );
}

/// The next query's batch commits `path` between its lookup and its first
/// hint read.
fn commit_after_lookup(h: &Harness, path: &'static str, text: &'static str) {
    *lock(&h.indexer.inner().step_hook) = Some((
        Step::Lookup,
        Box::new(move |inner, snap| {
            commit(inner, snap, path, text.as_bytes());
        }),
    ));
}

fn ran(h: &Harness) {
    assert!(lock(&h.indexer.inner().step_hook).is_none(), "the hook ran");
    assert!(h.hinted("c.ts"), "the batch installed c.ts's hint");
}

fn paths(r: &QueryResults) -> Vec<String> {
    Harness::found(r).into_iter().map(|(p, _, _)| p).collect()
}

/// A jump: the lookup found nothing, the batch indexed c.ts, the search
/// leaves c.ts out; the second lookup finds it.
#[test]
fn cut_jump_finds_a_file_indexed_after_the_lookup() {
    let h = unindexed(&[("a.ts", RUN), ("c.ts", ITEM)], &[]);
    commit_after_lookup(&h, "c.ts", ITEM);
    let r = h.goto("a.ts", "item", 0);
    ran(&h);
    assert_eq!(paths(&r), ["c.ts"]);
    assert_eq!(r.candidates[0].source, CandidateSource::Index);
    assert_eq!(r.state, QueryState::Done(Completeness::Complete));
}

/// A prefetch's step 3 looked up before the batch: the query that reuses
/// it looks up again, so the prefetch counts only for the hints it saw.
#[test]
fn cut_prefetch_reused_after_a_batch_finds_its_file() {
    let h = unindexed(&[("a.ts", RUN), ("c.ts", ITEM)], &[]);
    let q = Query {
        project: h.key(),
        kind: QueryKind::Definition {
            at: h.text("a.ts"),
            position: h.pos("a.ts", "item", 0),
        },
    };
    h.indexer.prefetch(q.clone());
    h.settle();
    assert!(h.indexer.inner().prefetched(&q));
    let inner = h.indexer.inner();
    let snap = inner.snap(&h.key()).expect("open");
    commit(inner, &snap, "c.ts", ITEM.as_bytes());
    assert!(h.hinted("c.ts"));
    let before = h.stats().prepares;
    let id = h.indexer.query(q);
    h.settle();
    assert_eq!(h.stats().prepares, before, "the prefetch is reused");
    let r = h.indexer.results(id).expect("kept");
    assert_eq!(paths(&r), ["c.ts"]);
    assert_eq!(r.candidates[0].source, CandidateSource::Index);
    assert_eq!(r.state, QueryState::Done(Completeness::Complete));
}

/// References: the `ci_ref` lookup found no file, the batch indexed c.ts,
/// the search leaves it out; the second lookup reads it.
#[test]
fn cut_references_find_a_file_indexed_after_the_lookup() {
    let h = unindexed(&[("a.ts", ITEM), ("c.ts", USE)], &[]);
    let r = h.goto("a.ts", "item", 0);
    let of: DefinitionRef = r
        .symbol
        .as_ref()
        .and_then(|s| s.defines.clone())
        .expect("a definition");
    commit_after_lookup(&h, "c.ts", USE);
    let r = h.ask(QueryKind::References { of });
    ran(&h);
    let found: Vec<String> = r
        .callers
        .iter()
        .chain(r.same_name.iter())
        .map(|x| x.location.path.to_string())
        .collect();
    assert_eq!(found, ["c.ts"]);
    assert_eq!(r.state, QueryState::Done(Completeness::Complete));
}

/// `Named`: its counts take c.ts as indexed; the second lookup lists it.
#[test]
fn cut_named_finds_a_file_indexed_after_the_lookup() {
    let h = unindexed(&[("a.ts", RUN), ("c.ts", ITEM)], &[]);
    commit_after_lookup(&h, "c.ts", ITEM);
    let r = h.ask(QueryKind::Named {
        name: "item".into(),
        near: None,
    });
    ran(&h);
    assert_eq!(paths(&r), ["c.ts"]);
    // a.ts is still not indexed.
    assert_eq!(
        r.state,
        QueryState::Done(Completeness::Partial {
            unsearched: 1,
            outside: 0
        })
    );
}

/// Hints that change nothing (an existence hit stamping a hint that
/// already matches) are no installation: the count stays.
#[test]
fn cut_unchanged_hint_is_no_installation() {
    let h = unindexed(&[("a.ts", RUN), ("c.ts", ITEM)], &[]);
    let inner = h.indexer.inner();
    let snap = inner.snap(&h.key()).expect("open");
    commit(inner, &snap, "c.ts", ITEM.as_bytes());
    let after = lock(&snap.project.state).hinted;
    assert!(after > snap.view.hinted);
    assert_eq!(inner.check_view(&snap, &snap.view), ViewCheck::Grew(after));
    let eff = snap.map.effective("c.ts").expect("c.ts");
    {
        let mut st = lock(&snap.project.state);
        let stamp = st.stamp(snap.project.evictions());
        st.hint_paths(
            &eff.oid,
            &eff.lang.name().0,
            stamp,
            crate::extract::ExtractStatus::Done,
        );
    }
    let view = crate::query::View {
        hinted: after,
        ..snap.view
    };
    assert_eq!(inner.check_view(&snap, &view), ViewCheck::Same);
}
