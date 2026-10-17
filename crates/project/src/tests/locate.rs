//! Placing paths among the open projects (`locate_*`).

use lectern_services::project::{
    FileKind, Location, PackageMatch, ProjectKey, Projects, WorktreeKey,
};

use super::support::*;

#[test]
fn locate_takes_the_deepest_package() {
    let h = Harness::new();
    let git_dir = h.git.init("repo");
    monorepo(&h.git, &git_dir);
    let key = h.open(&h.base().join("repo")).unwrap();
    let main = Some(WorktreeKey(git_dir.as_path().into()));
    let at = |rel: &str| h.model.locate(&h.base().join("repo").join(rel)).unwrap();
    assert_eq!(
        at("apps/console/src/main.ts"),
        Location {
            project: key.clone(),
            worktree: main.clone(),
            rel: "apps/console/src/main.ts".into(),
            package: found("apps/console"),
        }
    );
    assert_eq!(at("apps/web").package, found("apps/web"));
    // `apps/web-old` is not inside `apps/web`.
    assert_eq!(at("apps/web-old/x").package, found(""));
    assert_eq!(at("README.md").package, found(""));
    let root = h.model.locate(&h.base().join("repo")).unwrap();
    assert_eq!(&*root.rel, "");
    assert_eq!(root.package, found(""));
    assert_eq!(h.model.locate(&h.base().join("elsewhere/x")), None);
}

#[test]
fn locate_outside_every_package_is_none() {
    let h = Harness::new();
    let git_dir = h.git.init("repo");
    h.git
        .write(&git_dir, "libs/a/Cargo.toml", b"[package]\nname = \"a\"\n");
    h.git.write(&git_dir, "docs/index.md", b"# docs\n");
    h.open(&h.base().join("repo")).unwrap();
    let at = |rel: &str| h.model.locate(&h.base().join("repo").join(rel)).unwrap();
    assert_eq!(at("libs/a/src/lib.rs").package, found("libs/a"));
    assert_eq!(at("docs/index.md").package, PackageMatch::None);
}

#[test]
fn locate_while_packages_are_pending_is_not_loaded() {
    let h = Harness::new();
    let git_dir = h.git.init("repo");
    monorepo(&h.git, &git_dir);
    h.model.open(&h.base().join("repo"));
    assert!(h.host.step());
    let location = h
        .model
        .locate(&h.base().join("repo/apps/web/src/index.ts"))
        .unwrap();
    assert_eq!(location.package, PackageMatch::NotLoaded);
    assert_eq!(
        location.worktree,
        Some(WorktreeKey(git_dir.as_path().into()))
    );
    h.host.run();
    let location = h
        .model
        .locate(&h.base().join("repo/apps/web/src/index.ts"))
        .unwrap();
    assert_eq!(location.package, found("apps/web"));
}

#[test]
fn locate_in_an_inactive_worktree_is_not_loaded() {
    let h = Harness::new();
    let common = h.git.init("repo");
    monorepo(&h.git, &common);
    let linked = h.git.add_worktree(&common, "feature", "repo-feature");
    monorepo(&h.git, &linked);
    let key = h.open(&h.base().join("repo")).unwrap();
    let path = h.base().join("repo-feature/apps/web/src/index.ts");
    let expected = Location {
        project: key.clone(),
        worktree: Some(WorktreeKey(linked.as_path().into())),
        rel: "apps/web/src/index.ts".into(),
        package: PackageMatch::NotLoaded,
    };
    assert_eq!(h.model.locate(&path), Some(expected.clone()));
    // Filling the `resolve_package` cache changes nothing for `locate`.
    let worktree = WorktreeKey(linked.as_path().into());
    assert_eq!(
        h.model
            .resolve_package(&key, &worktree, "apps/web/src/index.ts"),
        Ok(found("apps/web"))
    );
    assert_eq!(h.model.locate(&path), Some(expected));
}

#[test]
fn locate_prefers_a_submodule_opened_on_its_own() {
    let h = Harness::new();
    let parent = h.git.init("parent");
    h.git
        .write(&parent, "package.json", br#"{ "name": "parent" }"#);
    h.git.track(&parent, "libs/sub", FileKind::Submodule);
    let sub = h.git.init("parent/libs/sub");
    h.git
        .write(&sub, "Cargo.toml", b"[package]\nname = \"sub\"\n");
    h.git.write(&sub, "src/lib.rs", b"");
    let parent_key = h.open(&h.base().join("parent")).unwrap();
    let sub_key = h.open(&h.base().join("parent/libs/sub")).unwrap();
    assert_ne!(parent_key, sub_key);
    let location = h
        .model
        .locate(&h.base().join("parent/libs/sub/src/lib.rs"))
        .unwrap();
    assert_eq!(location.project, sub_key);
    assert_eq!(&*location.rel, "src/lib.rs");
    assert_eq!(location.package, found(""));
    let location = h.model.locate(&h.base().join("parent/README.md")).unwrap();
    assert_eq!(location.project, parent_key);
}

#[test]
fn locate_in_a_folder_project() {
    let h = Harness::new();
    let root = h.base().join("folder");
    std::fs::create_dir_all(root.join("svc")).unwrap();
    std::fs::write(root.join("svc/go.mod"), b"module svc\n").unwrap();
    std::fs::write(root.join("svc/main.go"), b"package main\n").unwrap();
    let key = h.open(&root).unwrap();
    assert_eq!(
        h.model.locate(&root.join("svc/main.go")),
        Some(Location {
            project: key,
            worktree: None,
            rel: "svc/main.go".into(),
            package: found("svc"),
        })
    );
    assert!(matches!(
        h.model.locate(&root),
        Some(Location {
            project: ProjectKey::Folder { .. },
            ..
        })
    ));
}
