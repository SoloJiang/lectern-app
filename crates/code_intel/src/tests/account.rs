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
/// A parse that never ends in time (`Inner::parse_takes`).
const FOREVER: std::time::Duration = std::time::Duration::from_secs(3600);

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
    let _ = snap.walk_all(&mut account, "");
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

/// B1: a file read under tier 1's limits is parsed under tier 1's deadline
/// too (one rule, `Snap::file_limits`). Find references parses a `TooLarge`
/// unit's 1.5 MiB file (the search reads it up to 4 MiB) and a 2 MiB file
/// tier 1 indexed (from `ci_ref`); each parse takes 400 ms, past the
/// background's 250 ms and within tier 1's 1 s: both are searched, their
/// references found, the result complete.
#[test]
fn account_files_are_parsed_under_their_units_limit() {
    let api = large(CALL, 3 << 19);
    let big = large("export function third() { return item() - 1; }\n", 2 << 20);
    let h = indexed(&[
        ("a.ts", RUN),
        ("c.ts", ITEM),
        ("api.ts", &api),
        ("e.ts", &big),
    ]);
    h.current("e.ts");
    h.settle();
    assert!(h.hinted("api.ts") && h.hinted("e.ts"));
    assert_eq!(reasons(&h), [("api.ts".to_owned(), Reason::TooLarge)]);
    let of = defined(&h, "c.ts");
    let slow = std::time::Duration::from_millis(400);
    for path in ["api.ts", "e.ts"] {
        *lock(&h.indexer.inner().parse_takes) = Some((path.into(), slow));
        let r = h.ask(QueryKind::References { of: of.clone() });
        assert_eq!(paths(&r), ["a.ts", "api.ts", "e.ts"], "{path} parsed");
        assert_eq!(done(&r), Completeness::Complete, "{path}");
    }
    // Past tier 1's deadline too: not searched, never complete.
    *lock(&h.indexer.inner().parse_takes) = Some(("e.ts".into(), FOREVER));
    let r = h.ask(QueryKind::References { of });
    assert_eq!(paths(&r), ["a.ts", "api.ts"]);
    assert_eq!(done(&r), partial(1, 0));
}

/// The accounting's reasons of every file, for every language, before any
/// query read something.
fn reasons(h: &Harness) -> Vec<(String, Reason)> {
    let snap = h.indexer.inner().snap(&h.key()).expect("open");
    snap.unsearched_files(&Account::default(), "", &|_| true)
}

fn partial(unsearched: u32, outside: u32) -> Completeness {
    Completeness::Partial {
        unsearched,
        outside,
    }
}

/// F1: a tracked file over the background limit, never opened, is stored
/// `TooLarge` and its hint matches, but its unit holds nothing: it is not
/// indexed. `Named` (which searches nothing) reports it; a jump and find
/// references search it, under the interactive limit.
#[test]
fn account_too_large_units_are_searched() {
    let generated = large(ITEM, 3 << 19);
    let far = large("export function far() { return item() + 9; }\n", 3 << 19);
    let h = indexed(&[
        ("a.ts", RUN),
        ("c.ts", ITEM3),
        ("gen.ts", &generated),
        ("far.ts", &far),
    ]);
    assert!(h.hinted("gen.ts") && h.hinted("far.ts"));
    assert_eq!(
        reasons(&h),
        [
            ("far.ts".to_owned(), Reason::TooLarge),
            ("gen.ts".to_owned(), Reason::TooLarge)
        ]
    );
    let r = named(&h, "a.ts");
    assert_eq!(paths(&r), ["c.ts"]);
    assert_eq!(done(&r), partial(2, 0));

    let r = h.goto("a.ts", "item", 0);
    assert_eq!(paths(&r), ["c.ts", "gen.ts"], "gen.ts is searched");
    assert_eq!(done(&r), Completeness::Complete);

    // The search's `Done` extractions replaced the `TooLarge` units (a done
    // result outranks an unfinished unit whatever the tier writing it): no
    // longer unfinished, `Named` lists gen.ts, the next jump answers from
    // the index without extracting anything again.
    assert_eq!(reasons(&h), []);
    let r = named(&h, "a.ts");
    assert_eq!(paths(&r), ["c.ts", "gen.ts"]);
    assert_eq!(done(&r), Completeness::Complete);
    let before = h.extractions();
    let r = h.goto("a.ts", "item", 0);
    assert_eq!(paths(&r), ["c.ts", "gen.ts"]);
    assert_eq!(done(&r), Completeness::Complete);
    assert_eq!(h.extractions(), before, "nothing extracted again");

    let of = defined(&h, "c.ts");
    let r = h.ask(QueryKind::References { of });
    assert_eq!(paths(&r), ["a.ts", "far.ts"], "far.ts is searched");
    assert_eq!(done(&r), Completeness::Complete);
}

/// F1: a unit stored `TimedOut` (here every extraction of the file times
/// out) is not indexed either: never complete without its file.
#[test]
fn account_timed_out_units_are_not_indexed() {
    use crate::extract::{ExtractStatus, Extraction, extract};
    let h = Harness::new();
    h.indexer.set_extractor(|lang, bytes, deadline, cancel| {
        if memchr::memmem::find(bytes, b"SLOW").is_some() {
            Extraction::empty(ExtractStatus::TimedOut)
        } else {
            extract(lang, bytes, deadline, cancel)
        }
    });
    let slow = "// SLOW\nexport function item() { return 7; }\n";
    for (path, text) in [("a.ts", RUN), ("slow.ts", slow)] {
        h.git.add("main", &[(path, text.as_bytes())]);
    }
    h.git.commit("main");
    h.open();
    assert!(h.hinted("slow.ts"));
    assert_eq!(reasons(&h), [("slow.ts".to_owned(), Reason::TimedOut)]);
    assert_eq!(done(&named(&h, "a.ts")), partial(1, 0));
    // The search extracts it again, and it times out again.
    let r = h.goto("a.ts", "item", 0);
    assert!(r.candidates.is_empty());
    assert_eq!(done(&r), partial(1, 0));
}

/// F1, decided: a binary file has no definitions or references whatever the
/// limit, so its `Binary` unit counts as indexed.
#[test]
fn account_binary_units_count_as_indexed() {
    let binary = "export function item() { return 1; }\n\0\0\0\0";
    let h = indexed(&[("a.ts", RUN), ("c.ts", ITEM), ("bin.ts", binary)]);
    assert!(h.hinted("bin.ts"));
    assert_eq!(reasons(&h), []);
    let r = h.goto("a.ts", "item", 0);
    assert_eq!(paths(&r), ["c.ts"]);
    assert_eq!(done(&r), Completeness::Complete);
}

/// F1: a worktree file the overlay could not read (over 1 MiB, modified or
/// untracked: `Unindexable`) is not searched; a filter's pointer file
/// (`Excluded`) is no file of a query's.
#[test]
fn account_unindexable_worktree_files_are_unsearched() {
    use crate::map::WorktreeState;
    let h = indexed(&[
        ("a.ts", RUN),
        ("c.ts", ITEM),
        ("big.ts", CALL),
        ("lfs.ts", ITEM3),
    ]);
    h.pause();
    h.git
        .write("main", "big.ts", large(CALL, 2 << 20).as_bytes());
    h.git
        .write("main", "new.ts", large(ITEM3, 2 << 20).as_bytes());
    h.git.filter("lfs.ts");
    h.git.write(
        "main",
        "lfs.ts",
        b"version https://git-lfs.github.com/spec/v1\n",
    );
    h.emit(&[
        ("big.ts", CHANGED),
        ("new.ts", CHANGED),
        ("lfs.ts", CHANGED),
    ]);
    h.settle();
    let map = h.map();
    assert_eq!(map.overlay.get("big.ts"), Some(WorktreeState::Unindexable));
    assert_eq!(map.overlay.get("new.ts"), Some(WorktreeState::Unindexable));
    assert_eq!(map.overlay.get("lfs.ts"), Some(WorktreeState::Excluded));
    assert_eq!(
        reasons(&h),
        [
            ("big.ts".to_owned(), Reason::Unindexable),
            ("new.ts".to_owned(), Reason::Unindexable)
        ]
    );
    let r = h.goto("a.ts", "item", 0);
    assert_eq!(paths(&r), ["c.ts"]);
    assert_eq!(done(&r), partial(2, 0));
}

/// F2: references of a local definition are complete only when its own
/// file was searched; the own file is parsed with tier 1's limits.
#[test]
fn account_local_references_need_their_file_searched() {
    let text = "export function run() { const local = 1; return local + local; }\n";
    let h = indexed(&[("a.ts", text)]);
    let of = h
        .goto("a.ts", "local", 0)
        .symbol
        .as_ref()
        .and_then(|s| s.defines.clone())
        .expect("a definition");
    assert!(of.local);
    let r = h.ask(QueryKind::References { of: of.clone() });
    assert!(!paths(&r).is_empty());
    assert_eq!(done(&r), Completeness::Complete);
    *lock(&h.indexer.inner().parse_takes) = Some(("a.ts".into(), FOREVER));
    let r = h.ask(QueryKind::References { of });
    assert!(r.callers.is_empty() && r.same_name.is_empty());
    assert_eq!(done(&r), partial(1, 0));
}

/// F2: the definition's own file read by OID (an object: the worktree's
/// file changed since) that could not be searched counts, though the map
/// has another content at its path.
#[test]
fn account_definition_read_as_an_object_counts_when_unsearched() {
    use lectern_services::code_intel::TextOrigin;
    let h = indexed(&[("a.ts", RUN), ("c.ts", ITEM)]);
    let mut of = defined(&h, "c.ts");
    h.git.write("main", "c.ts", ITEM3.as_bytes());
    h.emit(&[("c.ts", CHANGED)]);
    h.settle();
    assert_ne!(h.text("c.ts").oid, of.location.oid);
    of.location.origin = TextOrigin::Object;
    of.location.worktree = None;
    let r = h.ask(QueryKind::References { of: of.clone() });
    assert_eq!(done(&r), Completeness::Complete);
    *lock(&h.indexer.inner().parse_takes) = Some(("c.ts".into(), FOREVER));
    let r = h.ask(QueryKind::References { of });
    assert_eq!(done(&r), partial(1, 0));
}

/// Sub-projects `pa` (the jump) and `pb` (six more files), all indexed,
/// the namespace full and the background paused.
fn full_two_packages() -> Harness {
    let h = Harness::new();
    h.git.add(
        "main",
        &[("pa/a.ts", RUN.as_bytes()), ("pa/b.ts", ITEM.as_bytes())],
    );
    for i in 0..6 {
        let text = format!("export function f{i}() {{ return {i}; }}\n");
        h.git
            .add("main", &[(&format!("pb/f{i}.ts"), text.as_bytes())]);
    }
    h.git.commit("main");
    h.open_with("main", packages(&["pa", "pb"]));
    h.pause();
    fill(&h);
    h
}

/// F3: closing a query during the full-mode check outside its scope stops
/// the check before its next statement (a statement is at most 500 OIDs, a
/// few milliseconds).
#[test]
fn account_full_check_stops_with_the_query() {
    use lectern_services::code_intel::Query;
    let h = full_two_packages();
    let inner = h.indexer.inner().clone();
    let id = h.indexer.query(Query {
        project: h.key(),
        kind: QueryKind::Definition {
            at: h.text("pa/a.ts"),
            position: h.pos("pa/a.ts", "item", 0),
        },
    });
    *lock(&inner.step_hook) = Some((Step::Verify, Box::new(move |inner, _| inner.close(id))));
    let before = inner
        .verify_checks
        .load(std::sync::atomic::Ordering::Acquire);
    h.settle();
    assert!(lock(&inner.step_hook).is_none(), "the check started");
    assert_eq!(
        inner
            .verify_checks
            .load(std::sync::atomic::Ordering::Acquire)
            - before,
        1,
        "no statement after the close"
    );
    assert!(h.indexer.results(id).is_none());
}

/// A4: `Named`'s candidates go out before its accounting's walk, whose
/// full-mode check runs up to the time box: at the walk's first statement
/// they are published, the query still running.
#[test]
fn account_named_candidates_go_out_before_the_walk() {
    let h = full_two_packages();
    let seen: std::sync::Arc<std::sync::Mutex<Option<std::sync::Arc<QueryResults>>>> =
        Default::default();
    let s = seen.clone();
    *lock(&h.indexer.inner().step_hook) = Some((
        Step::Verify,
        Box::new(move |inner, _| {
            let q = lock(&inner.queries);
            *s.lock().unwrap() = q.entries.back().map(|e| e.results.clone());
        }),
    ));
    let r = named(&h, "pa/a.ts");
    assert_eq!(paths(&r), ["pa/b.ts"]);
    assert_eq!(done(&r), Completeness::Complete);
    let before = seen
        .lock()
        .unwrap()
        .clone()
        .expect("the walk checked the store");
    assert_eq!(before.state, QueryState::Running);
    assert_eq!(paths(&before), ["pa/b.ts"]);
}

/// A hook that lets the query's time box pass at its `n`th full-mode
/// statement (`Step::Verify`), counting from 1.
fn time_box_at(n: usize, clock: crate::testing::ManualClock) -> crate::query::HookFn {
    Box::new(move |inner, _| {
        if n <= 1 {
            clock.advance(std::time::Duration::from_secs(2));
        } else {
            *lock(&inner.step_hook) = Some((Step::Verify, time_box_at(n - 1, clock)));
        }
    })
}

/// F3, A4: every full-mode check stops at the query's time box, the
/// scope's as well as the one outside it; what it did not check counts
/// (`Unchecked`: unsearched in the scope, `outside` outside it), never as
/// indexed. The statements: the scope's hinted pa/b.ts, the walk's pa/a.ts,
/// then pb's six files two at a time. Without the cut, complete.
#[test]
fn account_full_check_is_time_boxed() {
    let h = full_two_packages();
    // At the scope's statement: pa/b.ts is unchecked, and so is pb.
    *lock(&h.indexer.inner().step_hook) = Some((Step::Verify, time_box_at(1, h.clock.clone())));
    let r = h.goto("pa/a.ts", "item", 0);
    assert_eq!(paths(&r), ["pa/b.ts"], "the index's candidate");
    assert_eq!(done(&r), partial(1, 6));
    // At the first statement outside the scope.
    *lock(&h.indexer.inner().step_hook) = Some((Step::Verify, time_box_at(3, h.clock.clone())));
    let r = h.goto("pa/a.ts", "item", 0);
    assert_eq!(paths(&r), ["pa/b.ts"]);
    assert_eq!(done(&r), partial(0, 6));
    let r = h.goto("pa/a.ts", "item", 0);
    assert_eq!(done(&r), Completeness::Complete);
}

/// A4: the full-mode check of the search's scope (the root: no sub-project,
/// so every file is in it) stops at the query's time box too: the seven
/// hinted files besides the current text are unchecked and unsearched; a
/// query closed during it does no statement after the close.
#[test]
fn account_full_scope_check_stops_with_the_query() {
    use lectern_services::code_intel::Query;
    let mut files = vec![
        ("a.ts".to_owned(), RUN.to_owned()),
        ("b.ts".to_owned(), ITEM.to_owned()),
    ];
    for i in 0..6 {
        files.push((
            format!("f{i}.ts"),
            format!("export function f{i}() {{ return {i}; }}\n"),
        ));
    }
    let pairs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let h = indexed(&pairs);
    h.pause();
    fill(&h);
    *lock(&h.indexer.inner().step_hook) = Some((Step::Verify, time_box_at(1, h.clock.clone())));
    let r = h.goto("a.ts", "item", 0);
    assert_eq!(paths(&r), ["b.ts"], "the index's candidate");
    assert_eq!(done(&r), partial(7, 0));
    let inner = h.indexer.inner().clone();
    let id = h.indexer.query(Query {
        project: h.key(),
        kind: QueryKind::Definition {
            at: h.text("a.ts"),
            position: h.pos("a.ts", "item", 0),
        },
    });
    *lock(&inner.step_hook) = Some((Step::Verify, Box::new(move |inner, _| inner.close(id))));
    let before = inner
        .verify_checks
        .load(std::sync::atomic::Ordering::Acquire);
    h.settle();
    assert!(lock(&inner.step_hook).is_none(), "the check started");
    assert_eq!(
        inner
            .verify_checks
            .load(std::sync::atomic::Ordering::Acquire)
            - before,
        1,
        "no statement after the close"
    );
    assert!(h.indexer.results(id).is_none());
}

/// The walk is cached per project until its key changes: the same query
/// again on an unchanged index reuses it; a unit done again (tier 1 opens a
/// `TooLarge` file) changes the record, counted as an installation, so the
/// next query walks again and sees it indexed.
#[test]
fn account_walks_are_cached_until_the_index_changes() {
    let big = large(ITEM, 3 << 19);
    let h = indexed(&[("a.ts", RUN), ("c.ts", ITEM3), ("big.ts", &big)]);
    h.indexer.clear_walks(&h.key());
    assert_eq!(done(&named(&h, "a.ts")), partial(1, 0));
    assert_eq!(h.indexer.walks(&h.key()), 1);
    assert_eq!(done(&named(&h, "a.ts")), partial(1, 0));
    assert_eq!(h.indexer.walks(&h.key()), 1, "the same walk");
    h.current("big.ts");
    h.settle();
    let r = named(&h, "a.ts");
    assert_eq!(paths(&r), ["big.ts", "c.ts"]);
    assert_eq!(done(&r), Completeness::Complete);
    assert_eq!(h.indexer.walks(&h.key()), 2, "walked again");
}
