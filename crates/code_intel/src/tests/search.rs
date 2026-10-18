//! `search_*`: the time-boxed search (「限时搜索」): only whole-word hits are
//! parsed; one second after the query it stops with `Partial` counts; what
//! it extracted is written; a file being extracted in the background is
//! not searched and keeps the result `Partial`.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use lectern_services::code_intel::{CandidateSource, Completeness, QueryState};

use crate::extract::{Deadline, Lang, extract};
use crate::index::state::{Owner, Tier};
use crate::query::whole_word;

use super::goto::unindexed;
use super::support::Harness;

#[test]
fn search_whole_word_table() {
    let cases: &[(&str, &str, bool)] = &[
        ("item()", "item", true),
        ("x = item", "item", true),
        ("item", "item", true),
        ("itemCount()", "item", false),
        ("anItem item2", "item", false),
        ("$item _item item_ item$", "item", false),
        ("éitem itemé", "item", false),
        ("(item)", "item", true),
        ("a.item.b", "item", true),
        ("", "item", false),
        ("item", "", false),
    ];
    for (text, name, hit) in cases {
        assert_eq!(whole_word(text.as_bytes(), name), *hit, "{text:?} / {name:?}");
    }
}

/// Files that only contain the name inside longer identifiers, or not at
/// all, are read but never parsed.
#[test]
fn search_parses_only_whole_word_hits() {
    let h = unindexed(
        &[
            ("a.ts", "export function run() { return item(); }\n"),
            ("b.ts", "export function itemCount() { return 1; }\n"),
            ("c.ts", "export function item() { return 2; }\n"),
            ("d.ts", "export function other() { return 3; }\n"),
        ],
        &[],
    );
    let r = h.goto("a.ts", "item", 0);
    assert_eq!(
        Harness::found(&r)
            .into_iter()
            .map(|(p, _, _)| p)
            .collect::<Vec<_>>(),
        ["c.ts"]
    );
    assert_eq!(r.candidates[0].source, CandidateSource::Scanned);
    assert_eq!(r.state, QueryState::Done(Completeness::Complete));
    let c = h.git.oid(b"export function item() { return 2; }\n");
    assert_eq!(h.extracted(), [c], "only the hit is extracted");
}

/// One second after the query the search stops, between files: `Partial`
/// counts the sub-project's files left and the unindexed ones outside it.
#[test]
fn search_stops_at_one_second_with_partial_counts() {
    let mut files: Vec<(String, String)> =
        vec![("app/main.ts".into(), "export function run() { return helper(); }\n".into())];
    for i in 0..6 {
        files.push((
            format!("app/f{i}.ts"),
            format!("export function helper() {{ return {i}; }}\n"),
        ));
    }
    for i in 0..3 {
        files.push((
            format!("lib/l{i}.ts"),
            format!("export function helper() {{ return {}; }}\n", 10 + i),
        ));
    }
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    let h = unindexed(&refs, &["app", "lib"]);
    // Every extraction takes 400 ms of the query's clock.
    let clock = h.clock.clone();
    h.indexer.set_extractor(
        move |lang: &Lang, bytes: &[u8], deadline: &Deadline, cancel: &Arc<AtomicBool>| {
            let e = extract(lang, bytes, deadline, cancel);
            clock.advance(Duration::from_millis(400));
            e
        },
    );
    let r = h.goto("app/main.ts", "helper", 0);
    let paths: Vec<String> = Harness::found(&r).into_iter().map(|(p, _, _)| p).collect();
    assert_eq!(paths, ["app/f0.ts", "app/f1.ts", "app/f2.ts"]);
    assert_eq!(
        r.state,
        QueryState::Done(Completeness::Partial {
            unsearched: 3,
            outside: 3
        })
    );
}

/// A hit's extraction is written: stored, and its path's hint matches.
#[test]
fn search_writes_its_extractions() {
    let item = "export function item() { return 2; }\n";
    let h = unindexed(
        &[
            ("a.ts", "export function run() { return item(); }\n"),
            ("c.ts", item),
            ("d.ts", "export function other() { return 3; }\n"),
        ],
        &[],
    );
    let oid = h.git.oid(item.as_bytes());
    assert!(!h.stored(oid, "ts"));
    h.goto("a.ts", "item", 0);
    assert!(h.stored(oid, "ts"));
    assert!(h.hinted("c.ts"));
    assert!(!h.hinted("d.ts"), "searched, not hit: not indexed by the search");
    // The next query finds it in the index.
    let r = h.goto("a.ts", "item", 0);
    assert_eq!(r.candidates[0].source, CandidateSource::Index);
}

/// A file being extracted in the background is not searched: even with
/// every other file searched the result stays `Partial`.
#[test]
fn search_in_flight_partial() {
    let busy = "export function item() { return 9; }\n";
    let h = unindexed(
        &[
            ("a.ts", "export function run() { return item(); }\n"),
            ("b.ts", busy),
            ("c.ts", "export function item() { return 2; }\n"),
        ],
        &[],
    );
    let p = h.indexer.inner().project(&h.key()).expect("open");
    let lang = crate::extract::LanguageTable::core()
        .get("ts")
        .cloned()
        .expect("ts");
    p.state.lock().unwrap().in_flight.insert(
        (h.git.oid(busy.as_bytes()), lang.name().clone()),
        Owner {
            flag: Arc::new(AtomicBool::new(false)),
            tier: Tier::Subproject,
        },
    );
    let r = h.goto("a.ts", "item", 0);
    let paths: Vec<String> = Harness::found(&r).into_iter().map(|(p, _, _)| p).collect();
    assert_eq!(paths, ["c.ts"], "b.ts is not searched");
    match r.state {
        QueryState::Done(Completeness::Partial { unsearched, .. }) => assert!(unsearched >= 1),
        ref other => panic!("{other:?}"),
    }
}
