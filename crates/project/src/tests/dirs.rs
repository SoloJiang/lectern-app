//! The directory table (`dirs_*`, file_reader design「测试」for `project`):
//! built in a background task once a file list is published, published as
//! its own generation (`ChangedParts::dirs`), ready only after `files` and
//! from that very list; a newer list, a close or an unreadable index never
//! lets an older table be published.

use std::sync::Arc;

use lectern_services::project::{DirId, Projects, Scan, ScanError, WorktreeKey};

use super::support::*;

fn paths(table: &lectern_services::project::DirTable) -> Vec<String> {
    (0..table.len())
        .map(|i| table.path(DirId(i as u32)).to_owned())
        .collect()
}

#[test]
fn dirs_follow_the_file_list_in_their_own_generation() {
    let h = Harness::new();
    let git_dir = h.git.init("repo");
    monorepo(&h.git, &git_dir);
    h.model.open(&h.base().join("repo"));
    // The open task, then the scan: files and packages published, the
    // table still pending with its build queued.
    assert!(h.host.step());
    assert!(h.host.step());
    let key = h.model.open_projects().pop().unwrap();
    let before = h.snapshot(&key);
    check_dirs(&before);
    let active = before.active.as_ref().unwrap();
    assert!(matches!(active.files, Scan::Ready(_)));
    assert!(matches!(active.packages, Scan::Ready(_)));
    assert_eq!(active.dirs, Scan::Pending);
    assert_eq!(h.host.queued(), 1);
    h.host.run();
    let events = h.host.take_events();
    check_event_order(&events);
    insta::assert_snapshot!(render_events(&events, h.base()), @r#"
    Opened Ok(Git { common_dir: "[base]/repo/.git" })
    Changed 2 [files]
    Changed 3 [packages]
    Changed 4 [dirs]
    "#);
    let after = h.snapshot(&key);
    check_dirs(&after);
    let table = active_dirs(&after);
    assert!(Arc::ptr_eq(table.files(), &active_files(&after)));
    assert_eq!(
        paths(&table),
        [
            "",
            "apps",
            "apps/console",
            "apps/console/src",
            "apps/web",
            "apps/web/src"
        ]
    );
    assert_eq!(table.file_count(table.root()), 7);
    // Only `dirs` changed in that generation.
    let (a, b) = (
        before.active.as_ref().unwrap(),
        after.active.as_ref().unwrap(),
    );
    assert_eq!((&a.files, &a.packages), (&b.files, &b.packages));
}

#[test]
fn dirs_of_a_folder_project() {
    let h = Harness::new();
    let root = h.base().join("folder");
    for rel in ["README.md", "src/main.rs", "src/bin/tool.rs"] {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"").unwrap();
    }
    let key = h.open(&root).unwrap();
    let snapshot = h.snapshot(&key);
    check_dirs(&snapshot);
    assert_eq!(paths(&active_dirs(&snapshot)), ["", "src", "src/bin"]);
}

#[test]
fn dirs_a_new_file_list_resets_and_rebuilds_the_table() {
    let h = Harness::new();
    let git_dir = h.git.init("repo");
    monorepo(&h.git, &git_dir);
    let key = h.open(&h.base().join("repo")).unwrap();
    let old = active_dirs(&h.snapshot(&key));
    h.git.write(&git_dir, "docs/guide.md", b"# guide\n");
    // Reopening reads the index again: a new list, so the table is pending
    // until its own build publishes the new one.
    h.model.open(&h.base().join("repo"));
    assert!(h.host.step());
    assert!(h.host.step());
    let pending = h.snapshot(&key);
    check_dirs(&pending);
    assert_eq!(pending.active.as_ref().unwrap().dirs, Scan::Pending);
    h.host.run();
    let events = h.host.take_events();
    check_event_order(&events);
    insta::assert_snapshot!(render_events(&events, h.base()), @r#"
    Opened Ok(Git { common_dir: "[base]/repo/.git" })
    Changed 5 [files, packages, dirs]
    Changed 6 [packages]
    Changed 7 [dirs]
    "#);
    let snapshot = h.snapshot(&key);
    check_dirs(&snapshot);
    let new = active_dirs(&snapshot);
    assert!(!Arc::ptr_eq(&old, &new));
    assert_eq!(old.find("docs"), None);
    assert!(new.find("docs").is_some());
    assert_eq!(new.file_count(new.root()), 8);
}

#[test]
fn dirs_switching_worktrees_builds_the_new_tree_s_table() {
    let h = Harness::new();
    let common = h.git.init("repo");
    monorepo(&h.git, &common);
    let linked = h.git.add_worktree(&common, "feature", "repo-feature");
    h.git.write(&linked, "lib/feature.ts", b"export {};\n");
    let key = h.open(&h.base().join("repo")).unwrap();
    h.model
        .set_active_worktree(&key, &WorktreeKey(linked.as_path().into()));
    h.host.run();
    let events = h.host.take_events();
    check_event_order(&events);
    // The pending generation drops the old table with the old list.
    insta::assert_snapshot!(render_events(&events, h.base()), @r#"
    Changed 5 [active, files, packages, dirs]
    Changed 6 [files]
    Changed 7 [packages]
    Changed 8 [dirs]
    "#);
    let snapshot = h.snapshot(&key);
    check_dirs(&snapshot);
    assert_eq!(paths(&active_dirs(&snapshot)), ["", "lib"]);
}

#[test]
fn dirs_a_build_overtaken_by_a_newer_list_publishes_nothing() {
    let h = Harness::new();
    let git_dir = h.git.init("repo");
    monorepo(&h.git, &git_dir);
    h.model.open(&h.base().join("repo"));
    assert!(h.host.step());
    assert!(h.host.step());
    let key = h.model.open_projects().pop().unwrap();
    let stale = h.host.hold().expect("the first list's build");
    h.host.take_events();
    // A newer list is published (its build queued) before the first build
    // runs.
    h.git.write(&git_dir, "docs/guide.md", b"# guide\n");
    h.model.open(&h.base().join("repo"));
    assert!(h.host.step());
    assert!(h.host.step());
    assert_eq!(h.host.queued(), 1);
    let generation = h.snapshot(&key).generation;
    h.host.take_events();
    stale();
    assert!(h.host.take_events().is_empty());
    let snapshot = h.snapshot(&key);
    assert_eq!(snapshot.generation, generation);
    assert_eq!(snapshot.active.as_ref().unwrap().dirs, Scan::Pending);
    // The newer list's own build publishes its table.
    h.host.run();
    let snapshot = h.snapshot(&key);
    check_dirs(&snapshot);
    assert!(active_dirs(&snapshot).find("docs").is_some());
    assert_eq!(snapshot.generation, generation + 1);
}

#[test]
fn dirs_a_build_after_close_publishes_nothing() {
    let h = Harness::new();
    let git_dir = h.git.init("repo");
    monorepo(&h.git, &git_dir);
    h.model.open(&h.base().join("repo"));
    assert!(h.host.step());
    assert!(h.host.step());
    let key = h.model.open_projects().pop().unwrap();
    let build = h.host.hold().expect("the build");
    h.host.take_events();
    h.model.close(&key);
    h.host.run();
    build();
    insta::assert_snapshot!(render_events(&h.host.take_events(), h.base()), @r#"Closed Git { common_dir: "[base]/repo/.git" }"#);
}

#[test]
fn dirs_unavailable_with_the_file_list() {
    let h = Harness::new();
    let git_dir = h.git.init("repo");
    h.git.fail_index(&git_dir, "unsupported extension");
    let key = h.open(&h.base().join("repo")).unwrap();
    let snapshot = h.snapshot(&key);
    check_dirs(&snapshot);
    let error = ScanError::IndexUnreadable("corrupt repository data: unsupported extension".into());
    assert_eq!(
        snapshot.active.as_ref().unwrap().dirs,
        Scan::Unavailable(error)
    );
    // Nothing to build.
    assert_eq!(h.host.queued(), 0);
}
