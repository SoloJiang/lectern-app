//! Worktree lists and switching the active worktree (`worktrees_*`).

use lectern_services::project::{Head, Projects, Scan, WorktreeKey};

use super::support::*;

#[test]
fn worktrees_list_states_and_order() {
    let h = Harness::new();
    let common = h.git.init("repo");
    h.git.write(&common, "README.md", b"hi\n");
    let locked = h.git.add_worktree(&common, "locked", "z-locked");
    h.git.lock_worktree(&locked);
    let detached = h.git.add_worktree(&common, "detached", "b-detached");
    h.git.set_head(
        &detached,
        Head::Detached("0123456789abcdef0123456789abcdef01234567".into()),
    );
    let unborn = h.git.add_worktree(&common, "unborn", "c-unborn");
    h.git
        .set_head(&unborn, Head::Unborn("refs/heads/new".into()));
    // Prunable: the root is gone, the key still comes from the
    // administrative directory.
    let gone = h.git.add_worktree(&common, "gone", "a-gone");
    h.git.delete_root(&gone);
    let key = h.open(&h.base().join("repo")).unwrap();
    insta::assert_snapshot!(h.render(&key));
}

#[test]
fn worktrees_switch_publishes_pending_then_the_new_tree() {
    let h = Harness::new();
    let common = h.git.init("repo");
    monorepo(&h.git, &common);
    let linked = h.git.add_worktree(&common, "feature", "repo-feature");
    h.git.write(&linked, "go.work", b"go 1.22\n\nuse ./svc\n");
    h.git
        .write(&linked, "svc/go.mod", b"module example.com/svc\n");
    let key = h.open(&h.base().join("repo")).unwrap();
    h.host.take_events();

    h.model
        .set_active_worktree(&key, &WorktreeKey(linked.as_path().into()));
    // Nothing changes before the executor runs; then the pending snapshot
    // (generation 5) comes before the new tree's files, packages and
    // directory table.
    let before = h.snapshot(&key);
    assert_eq!(
        before
            .active
            .as_ref()
            .unwrap()
            .worktree
            .as_ref()
            .unwrap()
            .0
            .as_ref(),
        common.as_path()
    );
    h.host.run();
    let events = h.host.take_events();
    check_event_order(&events);
    insta::assert_snapshot!(render_events(&events, h.base()), @r"
    Changed 5 [active, files, packages, dirs]
    Changed 6 [files]
    Changed 7 [packages]
    Changed 8 [dirs]
    ");
    insta::assert_snapshot!(h.render(&key));
}

#[test]
fn worktrees_switch_ignores_prunable_unknown_and_current() {
    let h = Harness::new();
    let common = h.git.init("repo");
    h.git.write(&common, "README.md", b"hi\n");
    let gone = h.git.add_worktree(&common, "gone", "repo-gone");
    h.git.delete_root(&gone);
    let key = h.open(&h.base().join("repo")).unwrap();
    h.host.take_events();
    h.git.take_calls();
    h.model
        .set_active_worktree(&key, &WorktreeKey(gone.as_path().into()));
    h.model.set_active_worktree(
        &key,
        &WorktreeKey(h.base().join("nowhere").as_path().into()),
    );
    h.model
        .set_active_worktree(&key, &WorktreeKey(common.as_path().into()));
    h.host.run();
    assert!(h.host.take_events().is_empty());
    assert!(h.git.calls().is_empty());
}

#[test]
fn worktrees_switch_back_before_the_scan_ends_wins() {
    let h = Harness::new();
    let common = h.git.init("repo");
    h.git.write(&common, "README.md", b"main\n");
    let linked = h.git.add_worktree(&common, "feature", "repo-feature");
    h.git.write(&linked, "NOTES.md", b"feature\n");
    let key = h.open(&h.base().join("repo")).unwrap();
    h.model
        .set_active_worktree(&key, &WorktreeKey(linked.as_path().into()));
    h.model
        .set_active_worktree(&key, &WorktreeKey(common.as_path().into()));
    h.host.run();
    let snapshot = h.snapshot(&key);
    let active = snapshot.active.as_ref().unwrap();
    assert_eq!(
        active.worktree.as_ref().unwrap().0.as_ref(),
        common.as_path()
    );
    let Scan::Ready(files) = &active.files else {
        panic!("files not ready");
    };
    assert!(files.find("README.md").is_some());
}
