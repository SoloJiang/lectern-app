//! `account_*`: a query's completeness is one accounting of every file that
//! could take part (「完整性的核算」): indexed, searched, or not, whatever
//! tier it is in. Files outside the search's scope, hinted files whose unit
//! the full namespace evicted, and files over the read limit are never
//! taken as searched.

use lectern_services::code_intel::{
    Completeness, DefinitionRef, QueryKind, QueryResults, QueryState,
};

use super::goto::{indexed, unindexed};
use super::support::{CHANGED, Harness, packages};
use crate::index::lock;
use crate::query::{Account, Reason, Step};
use crate::store::Version;

const RUN: &str = "export function run() { return item(); }\n";
const ITEM: &str = "export function item() { return 2; }\n";
const CALL: &str = "export function more() { return item() * 2; }\n";
/// Another definition (other content: not the same unit as `ITEM`).
const ITEM3: &str = "export function item() { return 3; }\n";

fn done(r: &QueryResults) -> Completeness {
    match &r.state {
        QueryState::Done(c) => *c,
        other => panic!("not done: {other:?}"),
    }
}

fn paths(r: &QueryResults) -> Vec<String> {
    let mut out: Vec<String> = r
        .candidates
        .iter()
        .map(|c| c.definition.location.path.to_string())
        .chain(
            r.callers
                .iter()
                .chain(r.same_name.iter())
                .map(|x| x.location.path.to_string()),
        )
        .collect();
    out.sort();
    out.dedup();
    out
}

fn named(h: &Harness, near: &str) -> std::sync::Arc<QueryResults> {
    h.ask(QueryKind::Named {
        name: "item".into(),
        near: Some(near.into()),
    })
}

/// The definition at the first `item` of `path`.
fn defined(h: &Harness, path: &str) -> DefinitionRef {
    h.goto(path, "item", 0)
        .symbol
        .as_ref()
        .and_then(|s| s.defines.clone())
        .expect("a definition")
}

/// #2: the search covers the current sub-project; the files of the other
/// sub-projects that are neither indexed nor searched are reported as
/// `outside`, for a jump, find references and `Named` alike, never taken
/// as complete.
#[test]
fn account_outside_the_scope_is_partial() {
    let h = Harness::new();
    h.git.add(
        "main",
        &[("pa/a.ts", RUN.as_bytes()), ("pa/b.ts", ITEM.as_bytes())],
    );
    h.git.commit("main");
    h.open_with("main", packages(&["pa", "pb"]));
    // Another sub-project's files, not indexed: a definition and a call.
    h.pause();
    h.git.write("main", "pb/c.ts", ITEM3.as_bytes());
    h.git.write("main", "pb/d.ts", CALL.as_bytes());
    h.emit(&[("pb/c.ts", CHANGED), ("pb/d.ts", CHANGED)]);
    h.settle();
    let partial = Completeness::Partial {
        unsearched: 0,
        outside: 2,
    };
    // Classified by the walk: outside the scope `pa`, not indexed.
    let snap = h.indexer.inner().snap(&h.key()).expect("open");
    let all = |_: &crate::extract::Lang| true;
    assert_eq!(
        snap.unsearched_files(&Account::default(), "pa", &all),
        [
            ("pb/c.ts".to_owned(), Reason::Outside),
            ("pb/d.ts".to_owned(), Reason::Outside)
        ]
    );

    let r = h.goto("pa/a.ts", "item", 0);
    assert_eq!(paths(&r), ["pa/b.ts"]);
    assert_eq!(done(&r), partial, "jump");

    let r = named(&h, "pa/a.ts");
    assert_eq!(paths(&r), ["pa/b.ts"]);
    assert_eq!(done(&r), partial, "Named");

    let of = defined(&h, "pa/b.ts");
    let r = h.ask(QueryKind::References { of });
    assert_eq!(paths(&r), ["pa/a.ts"]);
    assert_eq!(done(&r), partial, "references");

    // Without sub-projects everything is the scope: searched, complete.
    h.set_packages(&[]);
    let r = h.goto("pa/a.ts", "item", 0);
    assert_eq!(paths(&r), ["pa/b.ts", "pb/c.ts"]);
    assert_eq!(done(&r), Completeness::Complete);
}

/// The namespace at its quota (`Batch::full`): the hints' stamp frozen at
/// the generation the batch tiers walk with.
fn fill(h: &Harness) {
    h.indexer.set_namespace_full(&h.key());
}

/// Removes `path`'s unit from the store, as the quota evicts it.
fn evict(h: &Harness, path: &str) {
    let project = h.indexer.inner().project(&h.key()).expect("open");
    let eff = h.map().effective(path).expect("in the map");
    let found = project
        .store
        .exists(&Version::of(&eff.lang), &[eff.oid])
        .unwrap_or_default();
    assert!(!found.is_empty(), "{path} is stored");
    let mut write = project.store.write();
    for f in found {
        write.remove_unit(f.unit);
    }
    let _ = write.commit();
}

/// #3: once the namespace is full a matching hint no longer proves its
/// unit. An evicted file whose hint still matches is checked against the
/// store, found missing, and searched (or, by `Named`, which searches
/// nothing, reported): never left out of a `Complete` result.
#[test]
fn account_full_namespace_checks_the_hinted_files() {
    let h = indexed(&[("a.ts", RUN), ("c.ts", ITEM), ("e.ts", CALL)]);
    h.pause();
    let of = defined(&h, "c.ts");
    fill(&h);
    evict(&h, "c.ts");
    evict(&h, "e.ts");
    assert!(
        h.hinted("c.ts") && h.hinted("e.ts"),
        "the frozen stamp keeps them"
    );
    // The accounting's walk checks them against the store.
    let snap = h.indexer.inner().snap(&h.key()).expect("open");
    let mut account = Account::default();
    let all = |_: &crate::extract::Lang| true;
    let _ = snap.walk(&mut account, "", &all);
    assert_eq!(
        snap.unsearched_files(&account, "", &all),
        [
            ("c.ts".to_owned(), Reason::Evicted),
            ("e.ts".to_owned(), Reason::Evicted)
        ]
    );

    // `Named` first: the queries after it store what they search. It
    // accounts for every language's files: both evicted ones.
    let r = named(&h, "a.ts");
    assert!(r.candidates.is_empty());
    assert_eq!(
        done(&r),
        Completeness::Partial {
            unsearched: 2,
            outside: 0
        }
    );

    let r = h.ask(QueryKind::References { of });
    assert_eq!(paths(&r), ["a.ts", "e.ts"], "e.ts is searched");
    assert_eq!(done(&r), Completeness::Complete);

    let r = h.goto("a.ts", "item", 0);
    assert_eq!(paths(&r), ["c.ts"], "c.ts is searched");
    assert_eq!(done(&r), Completeness::Complete);
}

/// #3: once the namespace is full the batch layer's own writes evict for
/// the quota; the view's eviction generation is the frozen one, so a query
/// racing them is not ended `Stale` (its accounting checks the store).
#[test]
fn account_full_namespace_eviction_during_a_query_is_not_stale() {
    for step in [Step::Lookup, Step::Hints] {
        let h = indexed(&[("a.ts", RUN), ("c.ts", ITEM)]);
        h.pause();
        fill(&h);
        let key = h.key();
        *lock(&h.indexer.inner().step_hook) = Some((
            step,
            Box::new(move |inner, _| {
                let project = inner.project(&key).expect("open");
                let st = lock(&project.state);
                let eff = st.map.effective("c.ts").expect("c.ts");
                drop(st);
                let found = project
                    .store
                    .exists(&Version::of(&eff.lang), &[eff.oid])
                    .unwrap_or_default();
                let mut write = project.store.write();
                for f in found {
                    write.remove_unit(f.unit);
                }
                let _ = write.commit();
            }),
        ));
        let r = h.goto("a.ts", "item", 0);
        assert!(lock(&h.indexer.inner().step_hook).is_none(), "{step:?} ran");
        assert_eq!(paths(&r), ["c.ts"], "{step:?}");
        assert_eq!(done(&r), Completeness::Complete, "{step:?}");
    }
}

/// A TypeScript file of about `size` bytes: `text`, then comments.
fn large(text: &str, size: usize) -> String {
    let mut s = text.to_owned();
    let line = "// padding padding padding padding padding padding padding\n";
    while s.len() < size {
        s.push_str(line);
    }
    s
}

/// #4: an indexed file is read under the limit its unit was extracted
/// under: tier 1 indexed a 2 MiB file (the interactive limit), so find
/// references reads it and lists its reference. A file over the limit it
/// is read under is not searched: the result is not `Complete`.
#[test]
fn account_files_are_read_under_their_units_limit() {
    let big = large(CALL, 2 << 20);
    let h = indexed(&[("a.ts", RUN), ("c.ts", ITEM), ("e.ts", &big)]);
    // Tier 1 redoes the background's `TooLarge` with the interactive limit.
    h.current("e.ts");
    h.settle();
    let of = defined(&h, "c.ts");
    let r = h.ask(QueryKind::References { of });
    assert_eq!(paths(&r), ["a.ts", "e.ts"]);
    assert_eq!(done(&r), Completeness::Complete);

    // A tracked 2 MiB file not indexed yet and not in tier 1: over the
    // background limit it is searched under, so not searched.
    let other = large("export function third() { return item() - 1; }\n", 2 << 20);
    let h = unindexed(
        &[
            ("a.ts", RUN),
            ("c.ts", ITEM),
            ("e.ts", &big),
            ("f.ts", &other),
        ],
        &[],
    );
    h.current("e.ts");
    h.settle();
    assert!(h.hinted("e.ts") && !h.hinted("f.ts"));
    let of = defined(&h, "c.ts");
    let r = h.ask(QueryKind::References { of });
    assert_eq!(paths(&r), ["a.ts", "e.ts"]);
    assert_eq!(
        done(&r),
        Completeness::Partial {
            unsearched: 1,
            outside: 0
        }
    );
}
