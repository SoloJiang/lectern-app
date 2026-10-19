//! `resolve` (any path, open or not) and `resolve_package` (inactive
//! worktrees) (`resolve_*`).

use std::path::Path;
use std::sync::atomic::Ordering;

use lectern_services::project::{
    OpenError, PackageMatch, ProjectKey, Projects, Resolved, ScanError, WorktreeKey,
};

use super::support::*;
use crate::testing::Call;

#[test]
fn resolve_places_paths_without_opening() {
    let h = Harness::new();
    let common = h.git.init("repo");
    monorepo(&h.git, &common);
    let linked = h.git.add_worktree(&common, "feature", "repo-feature");
    std::fs::create_dir_all(h.base().join("repo-feature/apps")).unwrap();
    let git = ProjectKey::Git {
        common_dir: common.as_path().into(),
    };
    assert_eq!(
        h.model
            .resolve(&h.base().join("repo/apps/web/src/index.ts")),
        Ok(Resolved {
            project: git.clone(),
            worktree: Some(WorktreeKey(common.as_path().into())),
            rel: "apps/web/src/index.ts".into(),
        })
    );
    assert_eq!(
        h.model.resolve(&h.base().join("repo-feature/apps")),
        Ok(Resolved {
            project: git.clone(),
            worktree: Some(WorktreeKey(linked.as_path().into())),
            rel: "apps".into(),
        })
    );
    // In a git directory: the project, no worktree.
    assert_eq!(
        h.model.resolve(&common),
        Ok(Resolved {
            project: git,
            worktree: None,
            rel: "".into(),
        })
    );
    assert!(h.model.open_projects().is_empty());
}

#[test]
fn resolve_outside_repositories_gives_folders() {
    let h = Harness::new();
    let root = h.base().join("folder");
    std::fs::create_dir_all(root.join("a/b")).unwrap();
    std::fs::write(root.join("a/b/file.txt"), b"").unwrap();
    // Not open: the directory itself, or a file's directory.
    assert_eq!(
        h.model.resolve(&root.join("a/b")),
        Ok(Resolved {
            project: ProjectKey::Folder {
                root: root.join("a/b").as_path().into()
            },
            worktree: None,
            rel: "".into(),
        })
    );
    assert_eq!(
        h.model.resolve(&root.join("a/b/file.txt")),
        Ok(Resolved {
            project: ProjectKey::Folder {
                root: root.join("a/b").as_path().into()
            },
            worktree: None,
            rel: "file.txt".into(),
        })
    );
    // Inside an open folder project: that project.
    h.open(&root).unwrap();
    assert_eq!(
        h.model.resolve(&root.join("a/b/file.txt")),
        Ok(Resolved {
            project: ProjectKey::Folder {
                root: root.as_path().into()
            },
            worktree: None,
            rel: "a/b/file.txt".into(),
        })
    );
    assert_eq!(
        h.model.resolve(&h.base().join("missing")),
        Err(OpenError::NotFound)
    );
}

/// The main worktree and three linked ones, each with its own packages:
/// one more (`extra`), one fewer (`fewer`), one moved (`moved`).
fn branches(h: &Harness) -> (ProjectKey, [WorktreeKey; 3]) {
    let common = h.git.init("repo");
    monorepo(&h.git, &common);
    let extra = h.git.add_worktree(&common, "extra", "wt-extra");
    monorepo(&h.git, &extra);
    h.git
        .write(&extra, "apps/admin/package.json", br#"{ "name": "admin" }"#);
    h.git.write(&extra, "apps/admin/src/a.ts", b"");
    let fewer = h.git.add_worktree(&common, "fewer", "wt-fewer");
    monorepo(&h.git, &fewer);
    h.git.remove_unstaged(&fewer, "apps/web/package.json");
    h.git.untrack(&fewer, "apps/web/package.json");
    let moved = h.git.add_worktree(&common, "moved", "wt-moved");
    monorepo(&h.git, &moved);
    h.git.remove_unstaged(&moved, "apps/console/package.json");
    h.git.untrack(&moved, "apps/console/package.json");
    h.git.write(&moved, "pom.xml", b"<project><artifactId>parent</artifactId><modules><module>services/console</module></modules></project>");
    h.git.write(
        &moved,
        "services/console/package.json",
        br#"{ "name": "console" }"#,
    );
    let key = h.open(&h.base().join("repo")).unwrap();
    let wt = |p: &Path| WorktreeKey(p.into());
    (key, [wt(&extra), wt(&fewer), wt(&moved)])
}

#[test]
fn resolve_package_follows_each_worktrees_own_packages() {
    let h = Harness::new();
    let (key, [extra, fewer, moved]) = branches(&h);
    let at = |w: &WorktreeKey, rel: &str| h.model.resolve_package(&key, w, rel).unwrap();
    // In the active worktree `apps/admin` is not a package.
    let main = WorktreeKey(h.base().join("repo/.git").as_path().into());
    assert_eq!(at(&main, "apps/admin/src/a.ts"), found(""));
    assert_eq!(at(&extra, "apps/admin/src/a.ts"), found("apps/admin"));
    assert_eq!(at(&main, "apps/web/src/index.ts"), found("apps/web"));
    assert_eq!(at(&fewer, "apps/web/src/index.ts"), found(""));
    assert_eq!(at(&main, "apps/console/src/main.ts"), found("apps/console"));
    // Moved: the pom names `services/console` now and `apps/console` has no
    // package.json left.
    assert_eq!(at(&moved, "apps/console/src/main.ts"), found(""));
    assert_eq!(
        at(&moved, "services/console/x.ts"),
        found("services/console")
    );
}

#[test]
fn resolve_package_hits_the_cache_without_reading_manifests() {
    let h = Harness::new();
    let (key, [extra, ..]) = branches(&h);
    assert_eq!(
        h.model.resolve_package(&key, &extra, "apps/admin/x"),
        Ok(found("apps/admin"))
    );
    h.git.take_calls();
    assert_eq!(
        h.model.resolve_package(&key, &extra, "apps/web/x"),
        Ok(found("apps/web"))
    );
    assert_eq!(
        h.git.take_calls(),
        vec![
            Call::OpenWorktree(extra.0.to_path_buf()),
            Call::IndexChecksum(extra.0.to_path_buf()),
        ]
    );
}

#[test]
fn resolve_package_unstaged_change_recomputes_and_reparses_only_it() {
    let h = Harness::new();
    let (key, [extra, ..]) = branches(&h);
    let extra_dir = extra.0.to_path_buf();
    assert_eq!(
        h.model.resolve_package(&key, &extra, "apps/web/x"),
        Ok(found("apps/web"))
    );
    // Not staged: the index checksum is unchanged, the lstat is not.
    h.git.write_unstaged(
        &extra_dir,
        "package.json",
        br#"{ "name": "root", "workspaces": ["apps/admin"] }"#,
    );
    h.git.take_calls();
    let parses = h.model.parses.load(Ordering::Relaxed);
    // `apps/web` still has its own package.json: undeclared, still a package.
    assert_eq!(
        h.model.resolve_package(&key, &extra, "apps/web/x"),
        Ok(found("apps/web"))
    );
    assert_eq!(h.model.parses.load(Ordering::Relaxed), parses + 1);
    assert!(
        h.git
            .take_calls()
            .contains(&Call::IndexFiles(extra_dir.clone()))
    );

    assert_eq!(
        h.model.resolve_package(&key, &extra, "apps/admin/src/a.ts"),
        Ok(found("apps/admin"))
    );

    // Deleting a manifest without staging: npm globs only match directories
    // with a package.json, so `apps/admin` is no package any more. Nothing
    // else is parsed again.
    h.git.remove_unstaged(&extra_dir, "apps/admin/package.json");
    let parses = h.model.parses.load(Ordering::Relaxed);
    assert_eq!(
        h.model.resolve_package(&key, &extra, "apps/admin/src/a.ts"),
        Ok(found(""))
    );
    assert_eq!(h.model.parses.load(Ordering::Relaxed), parses);
}

#[test]
fn resolve_package_for_the_active_worktree_uses_the_snapshot() {
    let h = Harness::new();
    let common = h.git.init("repo");
    monorepo(&h.git, &common);
    let main = WorktreeKey(common.as_path().into());
    h.model.open(&h.base().join("repo"));
    assert!(h.host.step());
    let key = h.model.open_projects().pop().unwrap();
    h.git.take_calls();
    assert_eq!(
        h.model.resolve_package(&key, &main, "apps/web/x"),
        Ok(PackageMatch::NotLoaded)
    );
    assert!(h.git.calls().is_empty());
    h.host.run();
    h.git.take_calls();
    assert_eq!(
        h.model.resolve_package(&key, &main, "apps/web/x"),
        Ok(found("apps/web"))
    );
    assert!(h.git.calls().is_empty());
}

#[test]
fn resolve_package_unknown_or_prunable_is_root_missing() {
    let h = Harness::new();
    let common = h.git.init("repo");
    monorepo(&h.git, &common);
    let gone = h.git.add_worktree(&common, "gone", "wt-gone");
    h.git.delete_root(&gone);
    let key = h.open(&h.base().join("repo")).unwrap();
    let missing = Err(ScanError::RootMissing);
    assert_eq!(
        h.model
            .resolve_package(&key, &WorktreeKey(gone.as_path().into()), "x"),
        missing
    );
    assert_eq!(
        h.model.resolve_package(
            &key,
            &WorktreeKey(h.base().join("nowhere").as_path().into()),
            "x"
        ),
        missing
    );
    let closed = ProjectKey::Folder {
        root: h.base().into(),
    };
    assert_eq!(
        h.model
            .resolve_package(&closed, &WorktreeKey(common.as_path().into()), "x"),
        missing
    );
}

#[test]
fn resolve_package_keeps_equal_lists_once() {
    let h = Harness::new();
    let common = h.git.init("repo");
    monorepo(&h.git, &common);
    let same = h.git.add_worktree(&common, "same", "wt-same");
    monorepo(&h.git, &same);
    let mut extra = Vec::new();
    for name in ["extra-a", "extra-b"] {
        let git_dir = h.git.add_worktree(&common, name, &format!("wt-{name}"));
        monorepo(&h.git, &git_dir);
        h.git.write(
            &git_dir,
            "apps/admin/package.json",
            br#"{ "name": "admin" }"#,
        );
        extra.push(git_dir);
    }
    let key = h.open(&h.base().join("repo")).unwrap();
    for git_dir in [&same, &extra[0], &extra[1]] {
        let found = h.model.resolve_package(
            &key,
            &WorktreeKey(git_dir.as_path().into()),
            "apps/web/src/index.ts",
        );
        assert_eq!(found, Ok(PackageMatch::Found("apps/web".into())));
    }
    let active = match &h.snapshot(&key).active.as_ref().unwrap().packages {
        lectern_services::project::Scan::Ready(p) => p.clone(),
        other => panic!("{other:?}"),
    };
    let cached = |git_dir: &Path| h.model.cached_packages(git_dir).unwrap();
    // The same sub-projects as the active tree: its list.
    assert!(std::sync::Arc::ptr_eq(&cached(&same), &active));
    // Equal to each other, not to the active tree: one list for both.
    let (a, b) = (cached(&extra[0]), cached(&extra[1]));
    assert!(!std::sync::Arc::ptr_eq(&a, &active));
    assert_eq!(a.len(), active.len() + 1);
    assert!(std::sync::Arc::ptr_eq(&a, &b));
}
