//! `escalate_*` and `head_dirty_*`: semantic navigation (「语义导航的注册」):
//! when a query asks the providers, whom it asks (a dead token, an
//! unregistered provider, `head_only` and HEAD), `Same` and `Ready`, and the
//! arbitration between SCIP-like and jdtls-like providers: the result
//! independent of reply order, stale IDs dropped, cancel on close only,
//! unregistering while requests are open.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lectern_kernel::ActivationToken;
use lectern_services::code_intel::{
    LangName, NavigationProvider, Query, QueryId, QueryKind, QueryResults, SemanticError,
    SemanticLocation, SemanticReply, SemanticRequest, SemanticRequestId, SemanticState,
};

use super::goto::indexed;
use super::support::{CHANGED, Harness};

/// A provider that records its calls and holds every reply until the test
/// sends it.
struct Fake {
    id: &'static str,
    head_only: bool,
    calls: Mutex<Vec<(&'static str, SemanticRequest)>>,
    pending: Mutex<Vec<(SemanticRequestId, SemanticReply)>>,
    cancelled: Mutex<Vec<SemanticRequestId>>,
}

impl Fake {
    fn new(id: &'static str, head_only: bool) -> Arc<Fake> {
        Arc::new(Fake {
            id,
            head_only,
            calls: Mutex::default(),
            pending: Mutex::default(),
            cancelled: Mutex::default(),
        })
    }

    fn calls(&self) -> Vec<(&'static str, SemanticRequest)> {
        self.calls.lock().unwrap().clone()
    }

    fn ids(&self) -> Vec<SemanticRequestId> {
        self.pending
            .lock()
            .unwrap()
            .iter()
            .map(|(i, _)| *i)
            .collect()
    }

    /// Answers the request `id` with `locations` (path, line, column).
    fn answer(&self, id: SemanticRequestId, locations: &[(&str, u32, u32)]) {
        let reply = {
            let mut pending = self.pending.lock().unwrap();
            let at = pending.iter().position(|(i, _)| *i == id).expect("open");
            pending.remove(at).1
        };
        reply.send(Ok(locations
            .iter()
            .map(|(p, l, c)| SemanticLocation {
                path: (*p).into(),
                line: *l,
                column: *c,
                end_line: *l,
            })
            .collect()));
    }

    fn answer_all(&self, locations: &[(&str, u32, u32)]) {
        for id in self.ids() {
            self.answer(id, locations);
        }
    }
}

impl NavigationProvider for Fake {
    fn id(&self) -> &str {
        self.id
    }

    fn handles(&self, lang: &LangName) -> bool {
        &*lang.0 == "java"
    }

    fn head_only(&self) -> bool {
        self.head_only
    }

    fn definition(&self, request: SemanticRequest, reply: SemanticReply) {
        self.calls
            .lock()
            .unwrap()
            .push(("definition", request.clone()));
        self.pending.lock().unwrap().push((request.id, reply));
    }

    fn references(&self, request: SemanticRequest, reply: SemanticReply) {
        self.calls
            .lock()
            .unwrap()
            .push(("references", request.clone()));
        self.pending.lock().unwrap().push((request.id, reply));
    }

    fn cancel(&self, request: SemanticRequestId) {
        self.cancelled.lock().unwrap().push(request);
    }
}

const A: &str = "class A {\n  int run(B b) {\n    return b.size();\n  }\n}\n";
const B: &str = "class B {\n  int size() { return 1; }\n}\n";
const C: &str = "class C {\n  int size() { return 2; }\n}\n";

fn java() -> Harness {
    indexed(&[("src/A.java", A), ("src/B.java", B), ("src/C.java", C)])
}

fn token(h: &Harness) -> ActivationToken {
    h.host.handle.token()
}

/// Asks the jump from `b.size()` without running the replies.
fn ask(h: &Harness) -> QueryId {
    let at = h.text("src/A.java");
    let position = h.pos("src/A.java", "size", 0);
    let id = h.indexer.query(Query {
        project: h.key(),
        kind: QueryKind::Definition { at, position },
    });
    h.settle();
    id
}

fn results(h: &Harness, id: QueryId) -> Arc<QueryResults> {
    h.settle();
    h.indexer.results(id).expect("kept")
}

/// `B.size` is at line 1, column 6.
const B_SIZE: (&str, u32, u32) = ("src/B.java", 1, 6);
const C_SIZE: (&str, u32, u32) = ("src/C.java", 1, 6);

#[test]
fn escalate_java_with_several_or_one_name_only_candidates() {
    let h = java();
    let p = Fake::new("jdtls", false);
    let _reg = h.indexer.register_provider(p.clone(), token(&h));
    let id = ask(&h);
    assert_eq!(results(&h, id).candidates.len(), 2);
    assert_eq!(results(&h, id).semantic, SemanticState::Pending);
    assert_eq!(p.calls().len(), 1);
    let (call, request) = &p.calls()[0];
    assert_eq!(*call, "definition");
    assert_eq!(&*request.at.path, "src/A.java");
    // One NameOnly candidate escalates too.
    let h1 = indexed(&[("src/A.java", A), ("src/B.java", B)]);
    let p1 = Fake::new("jdtls", false);
    let _reg1 = h1.indexer.register_provider(p1.clone(), token(&h1));
    ask(&h1);
    assert_eq!(p1.calls().len(), 1);
    // A local binding (one Bound candidate) does not.
    let h2 = indexed(&[(
        "src/A.java",
        "class A {\n  int run() {\n    int count = 1;\n    return count;\n  }\n}\n",
    )]);
    let p2 = Fake::new("jdtls", false);
    let _reg2 = h2.indexer.register_provider(p2.clone(), token(&h2));
    let at = h2.text("src/A.java");
    let position = h2.pos("src/A.java", "count", 1);
    let r = h2.ask(QueryKind::Definition { at, position });
    assert_eq!(r.jump, Some(0));
    assert!(p2.calls().is_empty());
    assert_eq!(r.semantic, SemanticState::NotRequested);
    // Other languages never escalate.
    let ts = indexed(&[
        ("a.ts", "export function run(b: B) { return b.size(); }\n"),
        ("b.ts", "export class B { size() { return 1; } }\n"),
    ]);
    let pts = Fake::new("jdtls", false);
    let _regts = ts.indexer.register_provider(pts.clone(), token(&ts));
    ts.goto("a.ts", "size", 0);
    assert!(pts.calls().is_empty());
}

#[test]
fn escalate_same_or_ready() {
    let h = java();
    let p = Fake::new("jdtls", false);
    let _reg = h.indexer.register_provider(p.clone(), token(&h));
    let id = ask(&h);
    p.answer_all(&[B_SIZE, C_SIZE]);
    assert_eq!(results(&h, id).semantic, SemanticState::Same);
    let id = ask(&h);
    p.answer_all(&[B_SIZE]);
    match &results(&h, id).semantic {
        SemanticState::Ready(locations) => {
            let at: Vec<(&str, u32)> = locations.iter().map(|l| (&*l.path, l.line)).collect();
            assert_eq!(at, [("src/B.java", 1)]);
            let oid = h.map().effective("src/B.java").unwrap().oid;
            assert_eq!(locations[0].oid, oid, "the OID from the query's map");
        }
        other => panic!("{other:?}"),
    }
    // A path the map does not have is dropped.
    let id = ask(&h);
    p.answer_all(&[B_SIZE, C_SIZE, ("src/Gone.java", 3, 1)]);
    assert_eq!(results(&h, id).semantic, SemanticState::Same);
    // Every provider failing: `Failed`.
    let id = ask(&h);
    for (rid, reply) in std::mem::take(&mut *p.pending.lock().unwrap()) {
        let _ = rid;
        reply.send(Err(SemanticError::Unavailable));
    }
    assert_eq!(results(&h, id).semantic, SemanticState::Failed);
}

/// A query's semantic part keeps the map it started with only while a
/// request is open (the map holds the language table's leases): the last
/// answer, or the last provider gone, drops it; the merge stays.
#[test]
fn escalate_answered_query_releases_its_map() {
    let h = java();
    let p = Fake::new("jdtls", false);
    let reg = h.indexer.register_provider(p.clone(), token(&h));
    let id = ask(&h);
    assert_eq!(h.indexer.inner().semantic_map_held(id), Some(true), "open");
    p.answer_all(&[B_SIZE]);
    assert!(matches!(results(&h, id).semantic, SemanticState::Ready(_)));
    assert_eq!(h.indexer.inner().semantic_map_held(id), Some(false));
    // The provider going away with a request open drops it too.
    let id = ask(&h);
    assert_eq!(h.indexer.inner().semantic_map_held(id), Some(true));
    drop(reg);
    h.settle();
    assert_eq!(h.indexer.inner().semantic_map_held(id), Some(false));
    assert_eq!(results(&h, id).semantic, SemanticState::Failed);
}

/// A dead token is never called, and the registration goes; a dropped
/// registration is not called either.
#[test]
fn escalate_dead_or_unregistered_providers_are_not_called() {
    let h = java();
    let other = crate::testing::host();
    let dead = other.handle.token();
    other.deactivate();
    let p = Fake::new("jdtls", false);
    let _reg = h.indexer.register_provider(p.clone(), dead);
    let id = ask(&h);
    assert!(p.calls().is_empty());
    assert_eq!(results(&h, id).semantic, SemanticState::NotRequested);
    assert_eq!(
        h.indexer.inner().providers(),
        0,
        "a dead provider is unregistered"
    );
    let q = Fake::new("jdtls", false);
    let reg = h.indexer.register_provider(q.clone(), token(&h));
    drop(reg);
    ask(&h);
    assert!(q.calls().is_empty());
}

/// `head_only` providers are asked only for text equal to HEAD, and their
/// locations in files that differ from HEAD are dropped.
#[test]
fn escalate_head_only_only_when_head_clean() {
    let h = java();
    let scip = Fake::new("scip", true);
    let _reg = h.indexer.register_provider(scip.clone(), token(&h));
    assert!(h.indexer.head_clean(&h.key(), "src/A.java"));
    let id = ask(&h);
    assert_eq!(scip.calls().len(), 1);
    assert!(scip.calls()[0].1.head_clean);
    // C.java changed in the worktree since HEAD: SCIP's location there goes.
    h.git.write(
        "main",
        "src/C.java",
        &[C.as_bytes(), b"// edited\n"].concat(),
    );
    h.emit(&[("src/C.java", CHANGED)]);
    h.settle();
    scip.answer_all(&[B_SIZE, C_SIZE]);
    match &results(&h, id).semantic {
        SemanticState::Ready(l) => {
            assert_eq!(
                l.iter().map(|l| &*l.path).collect::<Vec<_>>(),
                ["src/B.java"]
            );
        }
        other => panic!("{other:?}"),
    }
    // The text itself changed: SCIP is not asked.
    h.git.write(
        "main",
        "src/A.java",
        &[A.as_bytes(), b"// edited\n"].concat(),
    );
    h.emit(&[("src/A.java", CHANGED)]);
    h.settle();
    let before = scip.calls().len();
    let id = ask(&h);
    assert_eq!(scip.calls().len(), before);
    assert_eq!(results(&h, id).semantic, SemanticState::NotRequested);
}

/// A change only staged is not HEAD (`head_clean` false) until committed;
/// after an index change, until the comparison ran, nothing is clean.
#[test]
fn head_dirty_staged_index_and_commit() {
    let h = java();
    let scip = Fake::new("scip", true);
    let _reg = h.indexer.register_provider(scip.clone(), token(&h));
    h.git.add(
        "main",
        &[("src/A.java", &[A.as_bytes(), b"// staged\n"].concat())],
    );
    h.refresh_files();
    assert!(!h.indexer.head_clean(&h.key(), "src/A.java"));
    ask(&h);
    assert!(scip.calls().is_empty(), "staged, not committed");
    h.git.commit("main");
    h.emit_git(&[("HEAD", CHANGED)]);
    h.settle();
    assert!(h.indexer.head_clean(&h.key(), "src/A.java"));
    ask(&h);
    assert_eq!(scip.calls().len(), 1, "committed");
    // An index change delivered, the comparison not run yet: not clean.
    h.git.add(
        "main",
        &[("src/B.java", &[B.as_bytes(), b"// staged\n"].concat())],
    );
    h.emit_git(&[("index", CHANGED)]);
    h.driver.run_until_idle();
    h.driver.advance(Duration::from_secs(5));
    h.driver.run_until_idle();
    assert!(!h.indexer.head_clean(&h.key(), "src/A.java"));
    h.settle();
    assert!(h.indexer.head_clean(&h.key(), "src/A.java"));
}

/// SCIP and jdtls both handle Java: whatever order they reply in, the
/// merge is the same: HEAD-equal files from SCIP, changed and untracked
/// ones from jdtls; a reply to a stale ID is dropped; closing cancels what
/// is unanswered.
#[test]
fn escalate_arbitration_is_independent_of_reply_order() {
    let mut merged = Vec::new();
    for scip_first in [true, false] {
        let h = java();
        // C.java changed in the worktree; D.java untracked.
        h.git.write(
            "main",
            "src/C.java",
            &[C.as_bytes(), b"// edited\n"].concat(),
        );
        h.git.write(
            "main",
            "src/D.java",
            b"class D {\n  int size() { return 4; }\n}\n",
        );
        h.emit(&[("src/C.java", CHANGED), ("src/D.java", CHANGED)]);
        h.settle();
        let scip = Fake::new("scip", true);
        let jdtls = Fake::new("jdtls", false);
        // Registered in either order too.
        let (_r1, _r2) = if scip_first {
            (
                h.indexer.register_provider(scip.clone(), token(&h)),
                h.indexer.register_provider(jdtls.clone(), token(&h)),
            )
        } else {
            (
                h.indexer.register_provider(jdtls.clone(), token(&h)),
                h.indexer.register_provider(scip.clone(), token(&h)),
            )
        };
        let id = ask(&h);
        // SCIP's index is of HEAD: its C.java and D.java answers are not
        // its to give; jdtls's B.java is not its to give (B equals HEAD).
        let from_scip = [B_SIZE, ("src/C.java", 1, 6), ("src/D.java", 0, 0)];
        let from_jdtls = [("src/B.java", 1, 7), ("src/D.java", 1, 6)];
        if scip_first {
            scip.answer_all(&from_scip);
            jdtls.answer_all(&from_jdtls);
        } else {
            jdtls.answer_all(&from_jdtls);
            scip.answer_all(&from_scip);
        }
        let r = results(&h, id);
        let SemanticState::Ready(l) = &r.semantic else {
            panic!("{:?}", r.semantic);
        };
        let got: Vec<(String, u32, u32)> = l
            .iter()
            .map(|l| (l.path.to_string(), l.line, l.column))
            .collect();
        merged.push(got);
    }
    assert_eq!(merged[0], merged[1]);
    assert_eq!(
        merged[0],
        [
            ("src/B.java".to_owned(), 1, 6),
            ("src/D.java".to_owned(), 1, 6),
        ]
    );
}

#[test]
fn escalate_arbitration_stale_ids_and_cancel_on_close() {
    let h = java();
    let jdtls = Fake::new("jdtls", false);
    let _reg = h.indexer.register_provider(jdtls.clone(), token(&h));
    let first = ask(&h);
    let first_ids = jdtls.ids();
    h.indexer.close(first);
    assert_eq!(
        *jdtls.cancelled.lock().unwrap(),
        first_ids,
        "closing cancels"
    );
    // A late reply to the closed query's ID is dropped.
    jdtls.answer_all(&[B_SIZE]);
    h.settle();
    assert!(h.indexer.results(first).is_none());
    let second = ask(&h);
    assert_eq!(results(&h, second).semantic, SemanticState::Pending);
}

/// SCIP replies first and every known candidate equals HEAD: jdtls is not
/// cancelled, and its later location in an untracked file joins the merge.
#[test]
fn escalate_arbitration_jdtls_is_not_cancelled_by_scip() {
    let h = java();
    h.git.write(
        "main",
        "src/D.java",
        b"class D {\n  int size() { return 4; }\n}\n",
    );
    h.emit(&[("src/D.java", CHANGED)]);
    h.settle();
    let scip = Fake::new("scip", true);
    let jdtls = Fake::new("jdtls", false);
    let _r1 = h.indexer.register_provider(scip.clone(), token(&h));
    let _r2 = h.indexer.register_provider(jdtls.clone(), token(&h));
    let id = ask(&h);
    assert_eq!(results(&h, id).candidates.len(), 3);
    scip.answer_all(&[B_SIZE, C_SIZE]);
    let SemanticState::Ready(l) = results(&h, id).semantic.clone() else {
        panic!("not ready");
    };
    let paths: Vec<&str> = l.iter().map(|l| &*l.path).collect();
    assert_eq!(paths, ["src/B.java", "src/C.java"]);
    assert!(jdtls.cancelled.lock().unwrap().is_empty());
    assert_eq!(jdtls.ids().len(), 1, "still open");
    // jdtls's untracked D.java joins: now the candidates' positions.
    jdtls.answer_all(&[("src/D.java", 1, 6)]);
    assert_eq!(results(&h, id).semantic, SemanticState::Same);
}

/// A provider unregisters while its request is open: a query only it was
/// asked for fails; one SCIP answered merges what is left; a reply after
/// that is dropped.
#[test]
fn escalate_unregister_while_requests_are_open() {
    let h = java();
    let jdtls = Fake::new("jdtls", false);
    let reg = h.indexer.register_provider(jdtls.clone(), token(&h));
    let alone = ask(&h);
    let scip = Fake::new("scip", true);
    let _r = h.indexer.register_provider(scip.clone(), token(&h));
    let both = ask(&h);
    scip.answer_all(&[B_SIZE]);
    assert!(matches!(
        results(&h, both).semantic,
        SemanticState::Ready(_)
    ));
    drop(reg);
    assert_eq!(results(&h, alone).semantic, SemanticState::Failed);
    match results(&h, both).semantic.clone() {
        SemanticState::Ready(l) => assert_eq!(l.len(), 1),
        other => panic!("{other:?}"),
    }
    // jdtls answers anyway: too late.
    jdtls.answer_all(&[("src/C.java", 1, 6)]);
    assert_eq!(results(&h, alone).semantic, SemanticState::Failed);
    match results(&h, both).semantic.clone() {
        SemanticState::Ready(l) => assert_eq!(l.len(), 1, "the late reply is dropped"),
        other => panic!("{other:?}"),
    }
}

/// A Java `References` asks the providers' `references`.
#[test]
fn escalate_java_references() {
    let h = java();
    let jdtls = Fake::new("jdtls", false);
    let _reg = h.indexer.register_provider(jdtls.clone(), token(&h));
    let r = h.goto("src/B.java", "size", 0);
    let of = r
        .symbol
        .as_ref()
        .and_then(|s| s.defines.clone())
        .expect("defines");
    let before = jdtls.calls().len();
    let id = h.indexer.query(Query {
        project: h.key(),
        kind: QueryKind::References { of },
    });
    h.settle();
    let calls = jdtls.calls();
    assert_eq!(calls.len(), before + 1);
    assert_eq!(calls[before].0, "references");
    jdtls.answer_all(&[]);
    let r = results(&h, id);
    // No callers, an empty reply: the same.
    assert_eq!(r.semantic, SemanticState::Same);
}
