//! `search_*`: the time-boxed search (「限时搜索」): only whole-word hits are
//! parsed; one second after the query it stops with `Partial` counts; what
//! it extracted is written; a file being extracted in the background is
//! not searched and keeps the result `Partial`.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use lectern_services::code_intel::{CandidateSource, Completeness, QueryState};

use crate::Clock;
use crate::extract::{Deadline, ExtractStatus, Extraction, Lang, extract};
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
        assert_eq!(
            whole_word(text.as_bytes(), name),
            *hit,
            "{text:?} / {name:?}"
        );
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
    let mut files: Vec<(String, String)> = vec![(
        "app/main.ts".into(),
        "export function run() { return helper(); }\n".into(),
    )];
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
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
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
    assert!(
        !h.hinted("d.ts"),
        "searched, not hit: not indexed by the search"
    );
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

/// The search's per-file deadline is the background limit cut at the
/// query's end. An extraction the cut stops is the query running out of
/// time, not the file timing out: it is not stored, the file counts as
/// unsearched, and the batch tiers index it afterwards, so a later jump
/// finds it.
#[test]
fn search_cut_extraction_is_not_stored() {
    let mut files: Vec<(String, String)> = vec![(
        "main.ts".into(),
        "export function run() { return helper(); }\n".into(),
    )];
    for i in 0..5 {
        files.push((
            format!("f{i}.ts"),
            format!("export function helper() {{ return {i}; }}\n"),
        ));
    }
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let h = unindexed(&refs, &[]);
    // Every extraction takes 200 ms, and runs out of time when that
    // reaches its deadline: f0–f3 take the first 800 ms, f4 gets the
    // 200 ms left of the query's second and runs out.
    let clock = h.clock.clone();
    h.indexer.set_extractor(
        move |lang: &Lang, bytes: &[u8], deadline: &Deadline, cancel: &Arc<AtomicBool>| {
            clock.advance(Duration::from_millis(200));
            if clock.now() >= deadline.instant() {
                return Extraction::empty(ExtractStatus::TimedOut);
            }
            extract(lang, bytes, deadline, cancel)
        },
    );
    let paths = |r: &lectern_services::code_intel::QueryResults| -> Vec<String> {
        let mut p: Vec<String> = Harness::found(r).into_iter().map(|(p, _, _)| p).collect();
        p.sort();
        p
    };
    let r = h.goto("main.ts", "helper", 0);
    assert_eq!(paths(&r), ["f0.ts", "f1.ts", "f2.ts", "f3.ts"]);
    assert_eq!(
        r.state,
        QueryState::Done(Completeness::Partial {
            unsearched: 1,
            outside: 0
        }),
        "the cut file is not reported as searched"
    );
    let f4 = h.git.oid(b"export function helper() { return 4; }\n");
    assert!(!h.stored(f4, "ts"), "a cut result is not stored");
    assert!(!h.hinted("f4.ts"));
    // The batch tiers index it with their own deadline.
    h.resume();
    h.settle();
    assert!(h.stored(f4, "ts"));
    let r = h.goto("main.ts", "helper", 0);
    assert_eq!(paths(&r), ["f0.ts", "f1.ts", "f2.ts", "f3.ts", "f4.ts"]);
    assert_eq!(r.state, QueryState::Done(Completeness::Complete));
}

/// One file's deadline in a query: the background limit, or the query's
/// end when that comes first; what may be stored under each.
#[test]
fn search_file_deadline_table() {
    use crate::extract::BACKGROUND_DEADLINE;
    use crate::query::FileDeadline;
    let now = std::time::Instant::now();
    let full = now + BACKGROUND_DEADLINE;
    let near = now + Duration::from_millis(100);
    let far = now + Duration::from_secs(1);
    type Case = (
        &'static str,
        Option<std::time::Instant>,
        std::time::Instant,
        bool,
    );
    let cases: [Case; 4] = [
        ("no end", None, full, true),
        ("an end after the limit", Some(far), full, true),
        ("an end at the limit", Some(full), full, true),
        ("an end before the limit", Some(near), near, false),
    ];
    for (what, until, at, timeout_settles) in cases {
        let d = FileDeadline::new(now, until);
        assert_eq!(d.at, at, "{what}");
        assert_eq!(
            d.settles(ExtractStatus::TimedOut),
            timeout_settles,
            "{what}"
        );
        assert!(!d.settles(ExtractStatus::Cancelled), "{what}");
        for status in [
            ExtractStatus::Done,
            ExtractStatus::TooLarge,
            ExtractStatus::Binary,
            ExtractStatus::Unsupported,
        ] {
            assert!(d.settles(status), "{what} {status:?}");
        }
    }
}

/// The ranking distance: the path segments left on both sides after the
/// common directory prefix.
#[test]
fn search_distance_table() {
    use crate::query::distance;
    let cases: &[(&str, &str, usize)] = &[
        ("a/b/c.ts", "a/b/c.ts", 0),
        ("a/b/c.ts", "a/b/d.ts", 2),
        ("a/b/c.ts", "a/d.ts", 3),
        ("c.ts", "d.ts", 2),
        ("a/b/c.ts", "x/y.ts", 5),
        ("a/b", "a/b/c", 1),
        ("", "a", 2),
    ];
    for (a, b, d) in cases {
        assert_eq!(distance(a, b), *d, "{a} / {b}");
        assert_eq!(distance(b, a), *d, "{b} / {a}");
    }
}
