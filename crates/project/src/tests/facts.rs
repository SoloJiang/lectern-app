//! When the model submits `WorkspaceFacts` (`facts_*`; the pattern
//! semantics are tested in `facts/tests.rs`).

use lectern_services::project::{Projects, WorktreeKey};

use super::support::*;

#[test]
fn facts_follow_open_switch_and_close() {
    let h = Harness::new();
    let common = h.git.init("repo");
    monorepo(&h.git, &common);
    let linked = h.git.add_worktree(&common, "feature", "repo-feature");
    h.git.write(&linked, "go.mod", b"module x\n");
    let key = h.open(&h.base().join("repo")).unwrap();
    let facts = h.host.facts().unwrap();
    assert!(facts.contains("pom.xml"));
    assert!(!facts.contains("go.mod"));

    h.model
        .set_active_worktree(&key, &WorktreeKey(linked.as_path().into()));
    h.host.run();
    let facts = h.host.facts().unwrap();
    assert!(facts.contains("go.mod"));
    assert!(!facts.contains("pom.xml"));

    h.model.close(&key);
    h.host.run();
    assert!(!h.host.facts().unwrap().contains("go.mod"));
}

#[test]
fn facts_cover_every_open_project() {
    let h = Harness::new();
    let a = h.git.init("a");
    h.git.write(&a, "pom.xml", b"<project/>");
    let b = h.git.init("b");
    h.git.write(&b, "Cargo.toml", b"[package]\nname = \"b\"\n");
    let folder = h.base().join("folder");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("go.mod"), b"module f\n").unwrap();
    h.open(&h.base().join("a")).unwrap();
    h.open(&h.base().join("b")).unwrap();
    h.open(&folder).unwrap();
    let facts = h.host.facts().unwrap();
    assert!(facts.contains("pom.xml"));
    assert!(facts.contains("Cargo.toml"));
    assert!(facts.contains("go.mod"));
}

#[test]
fn facts_are_submitted_once_per_file_list() {
    let h = Harness::new();
    let common = h.git.init("repo");
    monorepo(&h.git, &common);
    h.open(&h.base().join("repo")).unwrap();
    // The file list is ready once; the packages do not change the facts.
    assert_eq!(h.host.facts_count(), 1);
}
