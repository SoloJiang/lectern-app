//! `read_verify_*`: a query answers about worktree text only when the bytes
//! read are provably the `TextRef`'s content (「跳转到定义」 step 1).

use lectern_services::code_intel::{QueryError, QueryKind, QueryState, TextOrigin, TextRef};

use super::support::{CHANGED, Harness};

const TEXT: &str = "export function run() {\n  const limit = 1;\n  return limit;\n}\n";

fn query(h: &Harness, path: &str, oid: lectern_services::project::BlobId) -> QueryState {
    let at = TextRef {
        worktree: Some(h.wt()),
        path: path.into(),
        oid,
        origin: TextOrigin::Worktree,
    };
    let position = h.pos(path, "limit", 1);
    h.ask(QueryKind::Definition { at, position }).state.clone()
}

/// Every way worktree bytes relate to the `TextRef`'s OID, one row each.
#[test]
fn read_verify_answers_only_for_the_content() {
    struct Case {
        what: &'static str,
        setup: fn(&Harness),
        /// The OID the caller asks with.
        asked: fn(&Harness) -> lectern_services::project::BlobId,
        answers: bool,
    }
    let cases = [
        Case {
            what: "the raw bytes are the content",
            setup: |_| {},
            asked: |h| h.git.oid(TEXT.as_bytes()),
            answers: true,
        },
        Case {
            what: "a CRLF checkout: the canonical OID is the content",
            setup: |h| {
                h.git.eol("a.ts");
                h.git
                    .write("main", "a.ts", TEXT.replace('\n', "\r\n").as_bytes());
                h.emit(&[("a.ts", CHANGED)]);
                h.settle();
            },
            asked: |h| h.git.oid(TEXT.as_bytes()),
            answers: true,
        },
        Case {
            what: "changed since: the map's old OID is no longer the content",
            setup: |h| {
                h.git
                    .write("main", "a.ts", &[TEXT.as_bytes(), b"// edited\n"].concat());
                // No event yet: the map still has the old OID.
            },
            asked: |h| h.map().effective("a.ts").expect("in the map").oid,
            answers: false,
        },
        Case {
            what: "an external filter: no canonical OID",
            setup: |h| {
                h.git.filter("a.ts");
                h.git
                    .write("main", "a.ts", &[TEXT.as_bytes(), b"// pointer\n"].concat());
            },
            asked: |h| h.git.oid(TEXT.as_bytes()),
            answers: false,
        },
        Case {
            what: "gone from the worktree",
            setup: |h| h.git.delete("main", "a.ts"),
            asked: |h| h.git.oid(TEXT.as_bytes()),
            answers: false,
        },
    ];
    for case in cases {
        let h = Harness::new();
        h.git.add("main", &[("a.ts", TEXT.as_bytes())]);
        h.git.commit("main");
        h.open();
        (case.setup)(&h);
        let oid = (case.asked)(&h);
        // The position is read from the original text: the same line in
        // every row that answers.
        let state = if std::fs::metadata(h.git.root("main").join("a.ts")).is_ok() {
            query(&h, "a.ts", oid)
        } else {
            let at = TextRef {
                worktree: Some(h.wt()),
                path: "a.ts".into(),
                oid,
                origin: TextOrigin::Worktree,
            };
            let position = super::support::pos_in(TEXT.as_bytes(), "limit", 1);
            h.ask(QueryKind::Definition { at, position }).state.clone()
        };
        if case.answers {
            assert!(
                matches!(state, QueryState::Done(_)),
                "{}: {state:?}",
                case.what
            );
        } else {
            assert_eq!(
                state,
                QueryState::Failed(QueryError::Stale),
                "{}",
                case.what
            );
        }
    }
}

const RUN: &str = "export function run() { return item(); }\n";
const ITEM: &str = "export function item() { return 2; }\n";
const EDITED_ITEM: &str = "export function item() { return 3; }\n";
const USE: &str = "export function other() { return item() + 1; }\n";
const EDITED_USE: &str = "export function other() { return item() * item(); }\n";

/// A search candidate edited after the query took its snapshot (between
/// its lookup and the search's read): its new bytes give nothing, under no
/// OID; the file counts as unsearched, and nothing is stored for it.
#[test]
fn read_verify_search_skips_a_file_changed_after_the_snapshot() {
    let h = super::goto::unindexed(&[("a.ts", RUN), ("c.ts", ITEM)], &[]);
    let git = h.git.clone();
    *crate::index::lock(&h.indexer.inner().step_hook) = Some((
        crate::query::Step::Lookup,
        Box::new(move |_, _| {
            git.write("main", "c.ts", EDITED_ITEM.as_bytes());
        }),
    ));
    let r = h.goto("a.ts", "item", 0);
    assert!(r.candidates.is_empty(), "{:?}", r.candidates);
    assert_eq!(
        r.state,
        QueryState::Done(lectern_services::code_intel::Completeness::Partial {
            unsearched: 1,
            outside: 0
        })
    );
    let edited = h.git.oid(EDITED_ITEM.as_bytes());
    assert!(
        !h.stored(edited, "ts"),
        "nothing extracted from the new bytes"
    );
    assert!(!h.stored(h.git.oid(ITEM.as_bytes()), "ts"));
}

/// An indexed file edited after the snapshot (on disk, before its event):
/// find references reads none of its new bytes and stays `Partial`; the
/// file's index entry is of other content.
#[test]
fn read_verify_references_skip_an_indexed_file_changed_after_the_snapshot() {
    let h = super::goto::indexed(&[("a.ts", ITEM), ("c.ts", USE)]);
    let r = h.goto("a.ts", "item", 0);
    let of = r
        .symbol
        .as_ref()
        .and_then(|s| s.defines.clone())
        .expect("a definition");
    // Before the edit: c.ts's reference, complete.
    let r = h.ask(QueryKind::References { of: of.clone() });
    let paths = |r: &lectern_services::code_intel::QueryResults| -> Vec<String> {
        r.callers
            .iter()
            .chain(r.same_name.iter())
            .map(|x| x.location.path.to_string())
            .collect()
    };
    assert_eq!(paths(&r), ["c.ts"]);
    assert_eq!(
        r.state,
        QueryState::Done(lectern_services::code_intel::Completeness::Complete)
    );
    h.git.write("main", "c.ts", EDITED_USE.as_bytes());
    let r = h.ask(QueryKind::References { of });
    assert!(paths(&r).is_empty(), "{:?}", r.same_name);
    assert_eq!(
        r.state,
        QueryState::Done(lectern_services::code_intel::Completeness::Partial {
            unsearched: 1,
            outside: 0
        })
    );
}
