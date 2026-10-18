//! `incremental_*`: `FakeBackend` drives `WorktreeBatch`es; each event of
//! 「增量更新」's table bumps its input, and the kernel stays busy from the
//! backend's event until the overlay is updated.

use lectern_fs_watch::Flags;

use super::support::{CHANGED, DIR_CHANGED, Harness, REMOVED, ts};

fn indexed() -> Harness {
    let h = Harness::new();
    h.git.add(
        "main",
        &[
            ("src/a.ts", &ts("a")),
            ("src/b.ts", &ts("b")),
            ("docs/c.ts", &ts("c")),
        ],
    );
    h.git.commit("main");
    h.open();
    h
}

#[test]
fn incremental_edit_indexes_the_new_oid_and_forgets_the_old() {
    let h = indexed();
    let old = h.git.oid(&ts("a"));
    let new = h.git.oid(&ts("a_edited"));
    h.git.write("main", "src/a.ts", &ts("a_edited"));
    h.emit(&[("src/a.ts", CHANGED)]);
    h.settle();
    assert!(
        h.map().paths(&old, "ts").is_empty(),
        "the old OID is not returned"
    );
    assert_eq!(h.map().paths(&new, "ts").len(), 1);
    assert!(h.stored(new, "ts"));
    assert!(h.hinted("src/a.ts"));
    // The old unit stays for eviction: another worktree may use it.
    assert!(h.stored(old, "ts"));
}

#[test]
fn incremental_deletion_and_new_file() {
    let h = indexed();
    h.git.delete("main", "src/b.ts");
    h.git.write("main", "src/new.ts", &ts("new"));
    h.emit(&[
        ("src/b.ts", REMOVED),
        ("src/new.ts", super::support::CREATED),
    ]);
    h.settle();
    assert!(h.map().effective("src/b.ts").is_none());
    assert!(h.map().paths(&h.git.oid(&ts("b")), "ts").is_empty());
    assert!(h.hinted("src/new.ts"));
}

/// Each event and the generations it must bump.
#[test]
fn incremental_events_bump_their_inputs() {
    struct Case {
        name: &'static str,
        setup: fn(&Harness),
        events: &'static [(&'static str, Flags)],
        git: &'static [(&'static str, Flags)],
        compare: bool,
        head: bool,
        index: bool,
        rules: bool,
    }
    let cases = [
        Case {
            name: "a directory: compare to find files moved in",
            setup: |h| h.git.write("main", "src/moved/in.ts", &ts("moved")),
            events: &[("src", DIR_CHANGED)],
            git: &[],
            compare: true,
            head: false,
            index: false,
            rules: false,
        },
        Case {
            name: "rescan",
            setup: |_| {},
            events: &[("src", Flags(Flags::DIR.0 | Flags::RESCAN.0))],
            git: &[],
            compare: true,
            head: false,
            index: false,
            rules: true,
        },
        Case {
            name: "HEAD moved by a ref change",
            setup: |h| h.git.move_head("main"),
            events: &[],
            git: &[("refs/heads/main", CHANGED)],
            compare: true,
            head: true,
            index: false,
            rules: false,
        },
        Case {
            name: "the index",
            setup: |h| h.git.stage_content("main", "src/a.ts", &ts("a_staged")),
            events: &[],
            git: &[("index", CHANGED)],
            compare: true,
            head: false,
            index: true,
            rules: false,
        },
        Case {
            name: "a .gitignore",
            setup: |h| h.git.write("main", ".gitignore", b"*.log\n"),
            events: &[(".gitignore", CHANGED)],
            git: &[],
            compare: true,
            head: false,
            index: false,
            rules: true,
        },
    ];
    for case in cases {
        let h = indexed();
        let before = h.indexer.gens(&h.key()).expect("gens");
        let compares = h.stats().compares;
        (case.setup)(&h);
        h.emit(case.events);
        h.emit_git(case.git);
        h.settle();
        let after = h.indexer.gens(&h.key()).expect("gens");
        assert_eq!(
            h.stats().compares > compares,
            case.compare,
            "{}: compare",
            case.name
        );
        assert_eq!(after.head > before.head, case.head, "{}: head", case.name);
        assert_eq!(
            after.index > before.index,
            case.index,
            "{}: index",
            case.name
        );
        assert_eq!(
            after.rules > before.rules,
            case.rules,
            "{}: rules",
            case.name
        );
        assert_eq!(after.files, before.files, "{}: files", case.name);
    }
}

#[test]
fn incremental_directory_event_finds_moved_in_files() {
    let h = indexed();
    h.git.write("main", "src/moved/in.ts", &ts("moved"));
    h.emit(&[("src/moved", DIR_CHANGED)]);
    h.settle();
    assert!(h.hinted("src/moved/in.ts"));
}

#[test]
fn incremental_events_merge_into_one_round() {
    let h = indexed();
    let reconciles = h.stats().reconciles;
    let rereads = h.stats().rereads;
    for (i, path) in ["src/a.ts", "src/b.ts", "docs/c.ts"].iter().enumerate() {
        h.git.write("main", path, &ts(&format!("burst{i}")));
        h.emit(&[(path, CHANGED)]);
        h.driver.run_until_idle();
    }
    h.settle();
    assert_eq!(
        h.stats().reconciles,
        reconciles + 1,
        "one batch, one reconcile"
    );
    assert_eq!(h.stats().rereads, rereads + 3);
    assert_eq!(h.map().overlay.len(), 3);
}

#[test]
fn incremental_kernel_busy_until_the_overlay_is_updated() {
    let h = indexed();
    assert!(h.quiescent());
    h.git.write("main", "src/a.ts", &ts("a_busy"));
    h.emit(&[("src/a.ts", CHANGED)]);
    // The event is in fs_watch's coalescing window.
    h.driver.run_until_idle();
    assert!(!h.quiescent(), "busy from the backend's event");
    // Delivered: the reconcile is queued, not run.
    h.driver.advance(std::time::Duration::from_secs(5));
    assert!(!h.quiescent(), "busy until the reconcile ran");
    assert!(h.map().overlay.is_empty());
    h.executor.run_until_idle();
    assert_eq!(h.map().overlay.len(), 1);
    h.settle();
    assert!(h.quiescent(), "idle once the overlay is updated");
}
