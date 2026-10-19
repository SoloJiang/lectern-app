//! `view_*`: a query's results are consistent with one view (「查询的视图」).
//! The interleaving matrix: for each query kind, at each step where the
//! test hook runs (and, for a reused prefetch, between the prefetch and the
//! query), each background change, each case on its own repository. The
//! result is the old view's, the new view's, or not `Complete`: never a
//! silent omission, never a mix. Every candidate it lists is one of the old
//! or the new view's.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use lectern_services::code_intel::{
    Completeness, DefinitionRef, Query, QueryKind, QueryResults, QueryState,
};
use lectern_services::project::ProjectEvent;

use crate::extract::{Deadline, extract};
use crate::index::{Inner, Scanned, lock};
use crate::query::{HookFn, Snap, Step};
use crate::store::Version;

use super::support::Harness;

const RUN: &str = "export function run() { return item(); }\n";
/// Indexed before the query.
const ITEM: &str = "export function item() { return 2; }\n";
/// Not indexed: the search finds it.
const ITEM2: &str = "export function item() { return 4; }\n";
const ITEM2_EDITED: &str = "export function item() { return 5; }\n";
/// References: indexed, and not indexed.
const USE: &str = "export function other() { return item() + 1; }\n";
const USE2: &str = "export function more() { return item() * 2; }\n";
/// Only in the other worktree.
const ITEM3: &str = "export function item() { return 6; }\n";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Definition,
    /// A `Definition` reusing a prefetch made just before it.
    Prefetch,
    References,
    Named,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Change {
    /// A batch commits a file not indexed yet.
    Commit,
    /// The cache evicts an indexed file's unit (the store at quota).
    Evict,
    /// A file's content changes and the reconcile installs a new map.
    Map,
    /// A file's content changes on disk, before its event.
    Edit,
    /// The current worktree changes.
    Switch,
    /// The project closes.
    Close,
    /// A language pack is installed: the language table changes.
    Grammar,
    /// The pack k.jpack is in is upgraded: the table has the new version
    /// before the reconcile rebuilds the map.
    Upgrade,
}

const CHANGES: [Change; 8] = [
    Change::Commit,
    Change::Evict,
    Change::Map,
    Change::Edit,
    Change::Switch,
    Change::Close,
    Change::Grammar,
    Change::Upgrade,
];

/// Where a change happens: at a step of the query; just before the query
/// is asked, so it arrives while the reconcile the change requested has
/// not run (no ready snapshot after a switch, the old one otherwise); or
/// (a reused prefetch) between the prefetch and the query.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum At {
    Step(Step),
    Before,
    Between,
}

/// A method `item` in the pack's language: `Named` lists it.
const K_PACK: &str = "class K { void item() { } }\n";

const STEPS: [Step; 5] = [
    Step::Snapshot,
    Step::Lookup,
    Step::Hints,
    Step::Read,
    Step::Publish,
];

/// Commits `path`'s extraction as of the snapshot, through the batch
/// tiers' commit.
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

/// The repository: `main` with a.ts (the jump's text), c.ts and e.ts
/// indexed, d.ts and f.ts not (for `Named`, everything indexed); a
/// `feature` worktree with one more definition.
fn fixture(kind: Kind) -> Harness {
    let h = Harness::new();
    h.grammars.install(
        "jpack",
        &["jpack"],
        crate::testing::java_pack(1, Arc::new(())),
    );
    h.settle();
    let files: [(&str, &str); 6] = [
        ("a.ts", RUN),
        ("c.ts", ITEM),
        ("d.ts", ITEM2),
        ("e.ts", USE),
        ("f.ts", USE2),
        ("k.jpack", K_PACK),
    ];
    for (path, text) in files {
        h.git.add("main", &[(path, text.as_bytes())]);
    }
    h.git.commit("main");
    h.git.add_worktree("feature");
    for (path, text) in files {
        h.git.add("feature", &[(path, text.as_bytes())]);
    }
    h.git.add("feature", &[("g.ts", ITEM3.as_bytes())]);
    h.git.commit("feature");
    if kind == Kind::Named {
        h.open();
        return h;
    }
    h.pause();
    h.open();
    let inner = h.indexer.inner();
    let snap = inner.snap(&h.key()).expect("open");
    commit(inner, &snap, "c.ts", ITEM.as_bytes());
    commit(inner, &snap, "e.ts", USE.as_bytes());
    commit(inner, &snap, "k.jpack", K_PACK.as_bytes());
    h.settle();
    h
}

/// The change, as the hook runs it (on the query's thread).
fn change(h: &Harness, c: Change) -> HookFn {
    let git = h.git.clone();
    let projects = h.projects.clone();
    let grammars = h.grammars.clone();
    let key = h.key();
    match c {
        Change::Commit => Box::new(move |inner, snap| {
            commit(inner, snap, "d.ts", ITEM2.as_bytes());
        }),
        Change::Evict => Box::new(move |_, snap| {
            let eff = snap.map.effective("c.ts").expect("c.ts");
            let version = Version::of(&eff.lang);
            let found = snap
                .project
                .store
                .exists(&version, &[eff.oid])
                .unwrap_or_default();
            let mut write = snap.project.store.write();
            for f in found {
                write.remove_unit(f.unit);
            }
            let _ = write.commit();
        }),
        Change::Map => Box::new(move |inner, snap| {
            git.write("main", "d.ts", ITEM2_EDITED.as_bytes());
            lock(&snap.project.state).dirty.insert("d.ts".into());
            inner.arc().expect("live").reconcile_now(&key);
        }),
        Change::Edit => Box::new(move |_, _| {
            git.write("main", "d.ts", ITEM2_EDITED.as_bytes());
        }),
        Change::Switch => Box::new(move |inner, _| {
            let event = projects.set(git.snapshot(1_000, "feature", Vec::new()));
            inner.project_event(&event);
        }),
        Change::Close => Box::new(move |inner, _| {
            inner.project_event(&ProjectEvent::Closed { project: key });
        }),
        Change::Grammar => Box::new(move |inner, _| {
            grammars.install(
                "kpack",
                &["kpack"],
                crate::testing::java_pack(3, Arc::new(())),
            );
            inner.arc().expect("live").grammar_round();
        }),
        Change::Upgrade => Box::new(move |inner, _| {
            grammars.install(
                "jpack",
                &["jpack"],
                crate::testing::java_pack(2, Arc::new(())),
            );
            inner.arc().expect("live").grammar_round();
        }),
    }
}

/// What a result lists: (path, line, column, OID) of each candidate or
/// reference, and whether it is `Complete`.
#[derive(Clone, Debug, Default)]
struct Outcome {
    listed: BTreeSet<(String, u32, u32, String)>,
    complete: bool,
}

fn outcome(kind: Kind, r: &QueryResults) -> Outcome {
    let mut listed = BTreeSet::new();
    if kind == Kind::References {
        for x in r.callers.iter().chain(r.same_name.iter()) {
            let l = &x.location;
            listed.insert((l.path.to_string(), l.line, l.column, l.oid.to_string()));
        }
    } else {
        for c in r.candidates.iter() {
            let l = &c.definition.location;
            listed.insert((l.path.to_string(), l.line, l.column, l.oid.to_string()));
        }
    }
    Outcome {
        listed,
        complete: r.state == QueryState::Done(Completeness::Complete),
    }
}

/// The definition the references are asked of: c.ts's `item`.
fn defined(h: &Harness) -> DefinitionRef {
    h.goto("c.ts", "item", 0)
        .symbol
        .as_ref()
        .and_then(|s| s.defines.clone())
        .expect("a definition")
}

fn query(h: &Harness, kind: Kind, of: Option<&DefinitionRef>) -> Query {
    let kind = match kind {
        Kind::Definition | Kind::Prefetch => QueryKind::Definition {
            at: h.text("a.ts"),
            position: h.pos("a.ts", "item", 0),
        },
        Kind::References => QueryKind::References {
            of: of.expect("a definition").clone(),
        },
        Kind::Named => QueryKind::Named {
            name: "item".into(),
            near: Some("a.ts".into()),
        },
    };
    Query {
        project: h.key(),
        kind,
    }
}

/// Runs `q` to the end; None when its entry is gone (closed with its
/// session).
fn ask(h: &Harness, q: Query) -> Option<Arc<QueryResults>> {
    let id = h.indexer.query(q);
    h.settle();
    h.indexer.results(id)
}

/// The matrix for one kind; the number of cases and how many of them ran
/// their change (a hook at a step the kind never reaches does not).
fn matrix(kind: Kind) -> (usize, usize) {
    // The old view's result, on a repository nothing changes.
    let base = fixture(kind);
    let of = (kind == Kind::References).then(|| defined(&base));
    let old = outcome(
        kind,
        &ask(&base, query(&base, kind, of.as_ref())).expect("kept"),
    );
    assert!(old.complete, "{kind:?}: the old view's result is complete");
    assert!(!old.listed.is_empty(), "{kind:?}");
    let mut places: Vec<At> = STEPS.iter().map(|s| At::Step(*s)).collect();
    places.push(At::Before);
    if kind == Kind::Prefetch {
        places.push(At::Between);
    }
    let (mut cases, mut ran) = (0, 0);
    for at in places {
        for c in CHANGES {
            cases += 1;
            let what = format!("{kind:?} {at:?} {c:?}");
            let h = fixture(kind);
            let of = (kind == Kind::References).then(|| defined(&h));
            let q = query(&h, kind, of.as_ref());
            if kind == Kind::Prefetch {
                h.indexer.prefetch(q.clone());
                h.settle();
                assert!(h.indexer.inner().prefetched(&q), "{what}");
            }
            // The events recorded once the change is done.
            let mark = Arc::new(AtomicUsize::new(usize::MAX));
            let fired = match at {
                At::Step(step) => {
                    let (m, run) = (mark.clone(), change(&h, c));
                    let hook: HookFn = Box::new(move |inner, snap| {
                        run(inner, snap);
                        m.store(lock(&inner.host.events).len(), Ordering::Release);
                    });
                    *lock(&h.indexer.inner().step_hook) = Some((step, hook));
                    false
                }
                // Applied with nothing run after it: the reconcile it
                // requests is still queued when the query runs
                // (`Interactive` before `Background`).
                At::Before | At::Between => {
                    let inner = h.indexer.inner();
                    let snap = inner.snap(&h.key()).expect("open");
                    change(&h, c)(inner, &snap);
                    true
                }
            };
            let id = h.indexer.query(q.clone());
            h.settle();
            let r = h.indexer.results(id);
            let fired = fired || lock(&h.indexer.inner().step_hook).is_none();
            // Nothing of a session that ended is published after its end.
            if fired && matches!(at, At::Step(_)) && matches!(c, Change::Switch | Change::Close) {
                let from = mark.load(Ordering::Acquire);
                let after = h.events().into_iter().skip(from).any(|e| {
                    matches!(e, lectern_services::code_intel::CodeIntelEvent::Results { id: i, .. } if i == id)
                });
                assert!(!after, "{what}: published after its session ended");
            }
            *lock(&h.indexer.inner().step_hook) = None;
            if fired {
                ran += 1;
            }
            // The new view's result, asked again once everything settled.
            h.settle();
            let new = if c == Change::Close {
                // Not open: no result.
                Outcome::default()
            } else {
                ask(&h, query(&h, kind, of.as_ref()))
                    .map(|r| outcome(kind, &r))
                    .unwrap_or_default()
            };
            // A query running when its session ends is closed with it:
            // nothing of the old worktree is published after the switch.
            if fired && matches!(at, At::Step(_)) && matches!(c, Change::Switch | Change::Close) {
                assert!(
                    r.is_none(),
                    "{what}: still published after its session ended: {r:?}"
                );
            }
            let Some(r) = r else {
                // Closed with its session: nothing of it is left to read.
                continue;
            };
            assert!(
                matches!(r.state, QueryState::Done(_) | QueryState::Failed(_)),
                "{what}: never ends: {:?}",
                r.state
            );
            let got = outcome(kind, &r);
            if got.complete {
                assert!(
                    got.listed == old.listed || (new.complete && got.listed == new.listed),
                    "{what}: complete but neither view's:\n got {:?}\n old {:?}\n new {:?}",
                    got.listed,
                    old.listed,
                    new.listed
                );
            }
            for item in &got.listed {
                assert!(
                    old.listed.contains(item) || new.listed.contains(item),
                    "{what}: {item:?} is in neither view:\n old {:?}\n new {:?}",
                    old.listed,
                    new.listed
                );
            }
        }
    }
    eprintln!("{kind:?}: {cases} cases, {ran} ran their change");
    (cases, ran)
}

#[test]
fn view_matrix_definition() {
    let (cases, ran) = matrix(Kind::Definition);
    assert_eq!(cases, 48);
    assert_eq!(ran, cases, "every case ran its change");
}

#[test]
fn view_matrix_prefetch() {
    let (cases, ran) = matrix(Kind::Prefetch);
    assert_eq!(cases, 56);
    assert_eq!(ran, cases, "every case ran its change");
}

#[test]
fn view_matrix_references() {
    let (cases, ran) = matrix(Kind::References);
    assert_eq!(cases, 48);
    assert_eq!(ran, cases, "every case ran its change");
}

#[test]
fn view_matrix_named() {
    let (cases, ran) = matrix(Kind::Named);
    assert_eq!(cases, 48);
    // `Named` reads no file: its 8 `Read` cases never reach the step.
    assert_eq!(ran, cases - 8);
}

/// A folder project opening: the reconcile builds the base map (its file
/// list has no OIDs) before it reads every file into the overlay. In that
/// window no snapshot is ready: a query asked there waits, then answers
/// from the complete map.
#[test]
fn view_folder_project_opening_has_no_snapshot_before_its_overlay() {
    use lectern_services::project::{
        ActiveTree, FileKind, FileList, ProjectKey, ProjectSnapshot, Scan,
    };

    let h = Harness::new();
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().canonicalize().expect("canonical");
    std::fs::write(root.join("a.ts"), RUN).expect("write");
    std::fs::write(root.join("c.ts"), ITEM).expect("write");
    let key = ProjectKey::Folder {
        root: Arc::from(root.as_path()),
    };
    let mut files = FileList::builder();
    files.push("a.ts", FileKind::File);
    files.push("c.ts", FileKind::File);
    let in_window: Arc<std::sync::Mutex<Option<bool>>> = Arc::default();
    let seen = in_window.clone();
    let k = key.clone();
    *lock(&h.indexer.inner().after_base) = Some(Box::new(move |inner| {
        *seen.lock().unwrap() = Some(inner.snap(&k).is_some());
    }));
    let event = h.projects.set(ProjectSnapshot {
        key: key.clone(),
        generation: 1,
        name: "folder".into(),
        worktrees: Arc::from(Vec::new()),
        active: Some(ActiveTree {
            root: Arc::from(root.as_path()),
            worktree: None,
            opened_at: "".into(),
            files: Scan::Ready(Arc::new(files.build())),
            packages: Scan::Ready(Arc::from(Vec::new())),
        }),
    });
    h.indexer.project_event(&event);
    // Asked before anything ran: it waits for the snapshot.
    let id = h.indexer.query(Query {
        project: key.clone(),
        kind: QueryKind::Named {
            name: "item".into(),
            near: None,
        },
    });
    h.settle();
    assert_eq!(
        *in_window.lock().unwrap(),
        Some(false),
        "no snapshot between the base and the overlay"
    );
    // It started once the overlay was read: c.ts is a file of its map, not
    // indexed yet, so it is reported, never taken as complete.
    let r = h.indexer.results(id).expect("kept");
    let got = outcome(Kind::Named, &r);
    assert!(
        !got.complete || got.listed.iter().any(|(p, ..)| p == "c.ts"),
        "{:?} {:?}",
        r.state,
        got.listed
    );
    assert!(matches!(r.state, QueryState::Done(_)), "{:?}", r.state);
    // Once indexed: complete, with c.ts.
    let again = ask(
        &h,
        Query {
            project: key,
            kind: QueryKind::Named {
                name: "item".into(),
                near: None,
            },
        },
    )
    .expect("kept");
    let got = outcome(Kind::Named, &again);
    assert!(got.complete, "{:?}", again.state);
    assert!(got.listed.iter().any(|(p, ..)| p == "c.ts"));
}

/// A query asked right after the current worktree changed, about the new
/// worktree's text, before any reconcile ran: it answers from the new
/// worktree's map (g.ts is only there), or it is not `Complete`; never from
/// the old map. The same for every kind that reads the map.
#[test]
fn view_query_right_after_a_switch_answers_from_the_new_worktree() {
    for kind in [Kind::Definition, Kind::Named] {
        let h = fixture(kind);
        let event = h.projects.set(h.git.snapshot(1_000, "feature", Vec::new()));
        h.indexer.project_event(&event);
        let at = lectern_services::code_intel::TextRef {
            worktree: Some(h.git.worktree_key("feature")),
            path: "a.ts".into(),
            oid: h.git.oid(RUN.as_bytes()),
            origin: lectern_services::code_intel::TextOrigin::Worktree,
        };
        let q = Query {
            project: h.key(),
            kind: match kind {
                Kind::Named => QueryKind::Named {
                    name: "item".into(),
                    near: Some("a.ts".into()),
                },
                _ => QueryKind::Definition {
                    position: super::support::pos_in(RUN.as_bytes(), "item", 0),
                    at,
                },
            },
        };
        let r = ask(&h, q).expect("kept: asked after the switch");
        let got = outcome(kind, &r);
        if got.complete {
            assert!(
                got.listed.iter().any(|(p, ..)| p == "g.ts"),
                "{kind:?}: complete from the old map: {:?}",
                got.listed
            );
        }
        // Every OID it gives is the new worktree's.
        let feature = h.indexer.map(&h.key()).expect("open");
        for (path, _, _, oid) in &got.listed {
            let eff = feature.effective(path).expect("in the new map");
            assert_eq!(eff.oid.to_string(), *oid, "{kind:?} {path}");
        }
    }
}

/// Removing a unit is an eviction the view sees: the store's eviction
/// generation grows (the matrix's `Evict` relies on it).
#[test]
fn view_eviction_moves_the_generation() {
    let h = fixture(Kind::Definition);
    let inner = h.indexer.inner();
    let snap = inner.snap(&h.key()).expect("open");
    change(&h, Change::Evict)(inner, &snap);
    assert_eq!(
        inner.check_view(&snap, &snap.view),
        crate::query::ViewCheck::Evicted
    );
}
