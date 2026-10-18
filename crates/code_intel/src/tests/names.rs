//! `names_*`: the ⌘K name table (「名字表」): deduplicated, NFC, only the
//! current worktree's map; built once at a time and handed out in
//! `CodeIntelEvent::Names`; rebuilt while held, at most once per interval;
//! not after its holders let go.

use std::sync::Arc;
use std::time::Duration;

use lectern_services::code_intel::{CodeIntelEvent, NameTable, Names};

use crate::names::REBUILD_MIN;

use super::support::{CHANGED, Harness};

/// The tables in the `Names` events so far.
fn tables(h: &Harness) -> Vec<Arc<NameTable>> {
    h.events()
        .into_iter()
        .filter_map(|e| match e {
            CodeIntelEvent::Names { table, generation, .. } => {
                assert_eq!(generation, table.generation());
                Some(table)
            }
            _ => None,
        })
        .collect()
}

fn names_of(t: &NameTable) -> Vec<&str> {
    t.iter().collect()
}

/// Waits until the watchdog has run its due entries (it polls the manual
/// clock), then runs what they submitted.
fn fire_due(h: &Harness) {
    let watchdog = h.indexer.inner().watchdog.clone();
    let start = std::time::Instant::now();
    while watchdog.scheduled() > 0 {
        assert!(start.elapsed() < Duration::from_secs(20), "the due entry never ran");
        std::thread::sleep(Duration::from_millis(2));
    }
    h.settle();
}

#[test]
fn names_dedup_nfc_current_worktree_only() {
    let h = Harness::new();
    // `café` composed in one file, decomposed in another.
    h.git.add(
        "main",
        &[
            ("a.py", "def caf\u{e9}():\n    return 1\n\nclass Run:\n    pass\n".as_bytes()),
            ("b.py", "def cafe\u{301}():\n    return 2\n\ndef Run():\n    return 3\n".as_bytes()),
        ],
    );
    h.git.commit("main");
    h.git.add_worktree("other");
    h.git.add("other", &[("c.py", b"def only_there():\n    return 4\n")]);
    h.open();
    assert!(
        h.stored(h.git.oid(b"def only_there():\n    return 4\n"), "python"),
        "tier 5 stored the other worktree's file"
    );
    assert!(matches!(h.indexer.names(&h.key()), Names::Building));
    h.settle();
    let table = tables(&h).pop().expect("a Names event");
    assert_eq!(names_of(&table), ["Run", "caf\u{e9}"]);
    let run = table.iter().position(|n| n == "Run").expect("Run");
    let both = crate::def_kind_bit(crate::DefKind::Class) | crate::def_kind_bit(crate::DefKind::Function);
    assert_eq!(table.kinds(run), both);
}

#[test]
fn names_ready_after_build() {
    let h = Harness::new();
    h.git.add("main", &[("a.ts", b"export function one() {}\n")]);
    h.git.commit("main");
    h.open();
    assert!(matches!(h.indexer.names(&h.key()), Names::Building));
    // Building: another call does not start another build.
    assert!(matches!(h.indexer.names(&h.key()), Names::Building));
    assert_eq!(h.indexer.inner().name_builds(&h.key()), 1);
    h.settle();
    let held = tables(&h).pop().expect("a Names event");
    match h.indexer.names(&h.key()) {
        Names::Ready(t) => assert!(Arc::ptr_eq(&t, &held), "the same Arc"),
        other => panic!("{other:?}"),
    }
    assert_eq!(h.indexer.inner().name_builds(&h.key()), 1);
    // Every holder lets go: the next call builds again.
    h.clear_events();
    drop(held);
    assert!(matches!(h.indexer.names(&h.key()), Names::Building));
    h.settle();
    assert_eq!(h.indexer.inner().name_builds(&h.key()), 2);
}

/// Held and behind: `names` keeps returning the old table while the new
/// one waits for the interval; the watchdog's due entry builds it, one
/// build for several changes; after the holders let go, no more.
#[test]
fn names_refresh_while_held() {
    let h = Harness::new();
    h.git.add("main", &[("a.ts", b"export function one() {}\n")]);
    h.git.commit("main");
    h.open();
    h.indexer.names(&h.key());
    h.settle();
    let first = tables(&h).pop().expect("the first table");
    h.clear_events();
    // Two batches written within the interval.
    for (i, name) in ["two", "three"].iter().enumerate() {
        let path = format!("n{i}.ts");
        h.git
            .write("main", &path, format!("export function {name}() {{}}\n").as_bytes());
        h.emit(&[(&path, CHANGED)]);
        h.settle();
    }
    match h.indexer.names(&h.key()) {
        Names::Ready(t) => assert!(Arc::ptr_eq(&t, &first), "the old table meanwhile"),
        other => panic!("{other:?}"),
    }
    assert_eq!(h.indexer.inner().name_builds(&h.key()), 1, "not before the interval");
    h.clock.advance(REBUILD_MIN + Duration::from_millis(1));
    fire_due(&h);
    let fresh = tables(&h);
    assert_eq!(fresh.len(), 1, "one rebuild for both changes");
    assert_eq!(names_of(&fresh[0]), ["one", "three", "two"]);
    assert!(fresh[0].generation() > first.generation());
    assert_eq!(h.indexer.inner().name_builds(&h.key()), 2);
    // The holder takes the new table and lets the old one go.
    drop(first);
    h.clear_events();
    let held = fresh.into_iter().next().unwrap();
    drop(held);
    h.git.write("main", "n9.ts", b"export function four() {}\n");
    h.emit(&[("n9.ts", CHANGED)]);
    h.settle();
    h.clock.advance(REBUILD_MIN * 3);
    std::thread::sleep(Duration::from_millis(50));
    h.settle();
    assert!(tables(&h).is_empty(), "nobody holds it: no rebuild");
    assert_eq!(h.indexer.inner().name_builds(&h.key()), 2);
}

#[test]
fn names_unavailable_without_a_project_or_an_index() {
    let h = Harness::new();
    assert!(matches!(h.indexer.names(&h.key()), Names::Unavailable));
}
