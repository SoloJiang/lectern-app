//! Opening paths: git projects, folders, errors (`open_*`).

use std::sync::Arc;

use lectern_git::HashKind;
use lectern_services::project::{
    FileKind, OpenError, ProjectEvent, ProjectKey, Projects, Scan, ScanError,
};

use super::support::*;
use crate::model::Model;
use crate::testing::{Call, FakeGit};

#[test]
fn open_subdirectory_opens_the_whole_repository() {
    let h = Harness::new();
    let git_dir = h.git.init("repo");
    monorepo(&h.git, &git_dir);
    let key = h.open(&h.base().join("repo/apps/console")).unwrap();
    assert_eq!(
        key,
        ProjectKey::Git {
            common_dir: git_dir.as_path().into()
        }
    );
    insta::assert_snapshot!(h.render(&key));
    assert_eq!(h.model.open_projects(), vec![key]);
}

#[test]
fn open_emits_opened_then_files_then_packages() {
    let h = Harness::new();
    let git_dir = h.git.init("repo");
    monorepo(&h.git, &git_dir);
    let request = h.model.open(&h.base().join("repo"));
    // Before the scan runs there is no project.
    assert!(h.model.open_projects().is_empty());
    h.host.run();
    let events = h.host.take_events();
    assert_eq!(opened(&events, request).map(|_| ()), Ok(()));
    check_event_order(&events);
    insta::assert_snapshot!(render_events(&events, h.base()), @r#"
    Opened Ok(Git { common_dir: "[base]/repo/.git" })
    Changed 2 [files]
    Changed 3 [packages]
    Changed 4 [dirs]
    "#);
}

#[test]
fn open_snapshot_is_pending_until_the_scan_runs() {
    let h = Harness::new();
    let git_dir = h.git.init("repo");
    monorepo(&h.git, &git_dir);
    h.model.open(&h.base().join("repo"));
    // Only the open task: the project exists, the scan is still queued.
    assert!(h.host.step());
    let key = h.model.open_projects().pop().unwrap();
    let snapshot = h.snapshot(&key);
    assert_eq!(snapshot.generation, 1);
    let active = snapshot.active.as_ref().unwrap();
    assert_eq!(active.files, Scan::Pending);
    assert_eq!(active.packages, Scan::Pending);
    h.host.run();
    let active = h.snapshot(&key).active.clone().unwrap();
    assert!(matches!(active.files, Scan::Ready(_)));
    assert!(matches!(active.packages, Scan::Ready(_)));
}

#[test]
fn open_from_another_worktree_gives_the_same_key_and_switches() {
    let h = Harness::new();
    let common = h.git.init("repo");
    monorepo(&h.git, &common);
    let linked = h.git.add_worktree(&common, "feature", "repo-feature");
    h.git
        .write(&linked, "package.json", br#"{ "name": "feature" }"#);
    let first = h.open(&h.base().join("repo")).unwrap();
    let second = h.open(&h.base().join("repo-feature")).unwrap();
    assert_eq!(first, second);
    assert_eq!(h.model.open_projects(), vec![first.clone()]);
    let snapshot = h.snapshot(&first);
    let active = snapshot.active.as_ref().unwrap();
    assert_eq!(
        active.worktree.as_ref().unwrap().0.as_ref(),
        linked.as_path()
    );
    insta::assert_snapshot!(h.render(&first));
}

#[test]
fn open_reopening_rescans_the_index() {
    let h = Harness::new();
    let git_dir = h.git.init("repo");
    h.git.write(&git_dir, "README.md", b"hi\n");
    let key = h.open(&h.base().join("repo")).unwrap();
    h.git.write(&git_dir, "go.mod", b"module example.com/x\n");
    h.open(&h.base().join("repo")).unwrap();
    let snapshot = h.snapshot(&key);
    let Scan::Ready(files) = &snapshot.active.as_ref().unwrap().files else {
        panic!("files not ready");
    };
    assert_eq!(files.find("go.mod"), Some(FileKind::File));
    let Scan::Ready(packages) = &snapshot.active.as_ref().unwrap().packages else {
        panic!("packages not ready");
    };
    assert_eq!(&*packages[0].name, "example.com/x");
}

#[test]
fn open_bare_repository_takes_the_first_usable_linked_worktree() {
    let h = Harness::new();
    let common = h.git.init_bare("repo.git");
    let gone = h.git.add_worktree(&common, "a", "wt-a");
    let usable = h.git.add_worktree(&common, "b", "wt-b");
    h.git
        .write(&usable, "Cargo.toml", b"[package]\nname = \"b\"\n");
    h.git.delete_root(&gone);
    let key = h.open(&common).unwrap();
    insta::assert_snapshot!(h.render(&key));
}

#[test]
fn open_bare_repository_without_worktrees_has_no_active_tree() {
    let h = Harness::new();
    let common = h.git.init_bare("empty.git");
    let request = h.model.open(&common);
    h.host.run();
    let events = h.host.take_events();
    let key = opened(&events, request).unwrap();
    check_event_order(&events);
    // `Opened` is followed by a `Changed` even with nothing to scan.
    assert!(matches!(events.last(), Some(ProjectEvent::Changed { .. })));
    let snapshot = h.snapshot(&key);
    assert_eq!(&*snapshot.name, "empty");
    assert!(snapshot.active.is_none());
}

#[test]
fn open_git_directory_itself_opens_its_worktree() {
    let h = Harness::new();
    let git_dir = h.git.init("repo");
    h.git.write(&git_dir, "README.md", b"hi\n");
    let linked = h.git.add_worktree(&git_dir, "feature", "repo-feature");
    let key = h.open(&git_dir).unwrap();
    let active = h.snapshot(&key).active.clone().unwrap();
    assert_eq!(active.worktree.unwrap().0.as_ref(), git_dir.as_path());
    assert_eq!(&*active.opened_at, "");
    // A linked worktree's administrative directory opens that worktree.
    h.open(&linked).unwrap();
    let active = h.snapshot(&key).active.clone().unwrap();
    assert_eq!(active.worktree.unwrap().0.as_ref(), linked.as_path());
}

#[test]
fn open_below_a_home_dotfiles_repository_opens_a_folder() {
    let h = Harness::new();
    let home = h.git.init("home");
    h.git.write(&home, ".zshrc", b"export X=1\n");
    let code = h.base().join("home/code/x");
    std::fs::create_dir_all(&code).unwrap();
    std::fs::write(code.join("main.go"), b"package main\n").unwrap();
    let key = h.open(&code).unwrap();
    assert_eq!(
        key,
        ProjectKey::Folder {
            root: code.as_path().into()
        }
    );
    // Home itself is the dotfiles repository.
    let key = h.open(&h.base().join("home")).unwrap();
    assert_eq!(
        key,
        ProjectKey::Git {
            common_dir: home.as_path().into()
        }
    );
}

#[test]
fn open_errors_create_no_project() {
    let h = Harness::new();
    let git_dir = h.git.init("repo");
    h.git.write(&git_dir, "README.md", b"hi\n");
    assert_eq!(h.open(&h.base().join("missing")), Err(OpenError::NotFound));
    assert_eq!(
        h.open(&h.base().join("repo/README.md")),
        Err(OpenError::NotADirectory)
    );
    h.git.fail_repo(&git_dir, "bad commondir");
    assert_eq!(
        h.open(&h.base().join("repo")),
        Err(OpenError::RepoUnreadable(
            "corrupt repository data: bad commondir".into()
        ))
    );
    assert!(h.model.open_projects().is_empty());
}

#[test]
fn open_unreadable_index_keeps_the_worktree_list() {
    let h = Harness::new();
    let git_dir = h.git.init("repo");
    h.git.add_worktree(&git_dir, "feature", "repo-feature");
    h.git.fail_index(&git_dir, "unsupported extension");
    let key = h.open(&h.base().join("repo")).unwrap();
    let snapshot = h.snapshot(&key);
    assert_eq!(snapshot.worktrees.len(), 2);
    let active = snapshot.active.as_ref().unwrap();
    let error = ScanError::IndexUnreadable("corrupt repository data: unsupported extension".into());
    assert_eq!(active.files, Scan::Unavailable(error.clone()));
    assert_eq!(active.packages, Scan::Unavailable(error));
}

#[test]
fn open_submodules_are_file_entries_not_packages() {
    let h = Harness::new();
    let git_dir = h.git.init("repo");
    h.git.write(
        &git_dir,
        "Cargo.toml",
        b"[workspace]\nmembers = [\"libs/*\"]\n",
    );
    h.git.track(&git_dir, "libs/sub", FileKind::Submodule);
    let key = h.open(&h.base().join("repo")).unwrap();
    insta::assert_snapshot!(h.render(&key));
}

#[test]
fn open_folder_walks_with_skip_rules() {
    let h = Harness::new();
    let root = h.base().join("folder");
    let write = |rel: &str, content: &[u8]| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    };
    write(
        "package.json",
        br#"{ "name": "folder", "workspaces": ["pkgs/*"] }"#,
    );
    write("pkgs/a/package.json", br#"{ "name": "a" }"#);
    write("pkgs/a/index.js", b"");
    write(".eslintrc", b"{}");
    write(".cache/x", b"");
    write("node_modules/dep/package.json", b"{}");
    write("target/debug/x", b"");
    write("build/out.js", b"");
    write("dist/out.js", b"");
    write("out/x", b"");
    write("vendor/x/go.mod", b"module x\n");
    write("venv/bin/python", b"");
    write("__pycache__/x.pyc", b"");
    write("src/build.rs", b"");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        write("run.sh", b"#!/bin/sh\n");
        std::fs::set_permissions(root.join("run.sh"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        std::os::unix::fs::symlink("pkgs/a", root.join("link")).unwrap();
    }
    let key = h.open(&root).unwrap();
    insta::assert_snapshot!(h.render(&key));
    // A folder never reaches git beyond discovery.
    assert!(h.git.calls().iter().all(|c| matches!(c, Call::Discover(_))));
}

#[test]
fn open_file_lists_carry_index_oids_for_git_only() {
    for hash in [HashKind::Sha1, HashKind::Sha256] {
        let h = Harness::new();
        let git_dir = h.git.init_with("repo", hash);
        monorepo(&h.git, &git_dir);
        h.git.track(&git_dir, "vendor/lib", FileKind::Submodule);
        let key = h.open(&h.base().join("repo")).unwrap();
        let files = active_files(&h.snapshot(&key));
        // Every entry has the OID of what was staged, at the repository's
        // width; a submodule too.
        for i in 0..files.len() {
            let oid = files.oid(i).unwrap();
            assert_eq!(oid.as_bytes().len(), hash.byte_len(), "{hash:?}");
        }
        assert_eq!(
            oid_of(&files, "README.md"),
            blob(hash, b"# root\n"),
            "{hash:?}"
        );
        assert_eq!(
            oid_of(&files, "apps/web/src/index.ts"),
            blob(hash, b"export {};\n"),
            "{hash:?}"
        );
        // A gitlink holds a commit OID, not the empty blob's.
        assert_ne!(oid_of(&files, "vendor/lib"), blob(hash, b""), "{hash:?}");
    }

    // A folder project's list has none.
    let h = Harness::new();
    let root = h.base().join("folder");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("package.json"), br#"{ "name": "folder" }"#).unwrap();
    std::fs::write(root.join("src/index.ts"), b"export {};\n").unwrap();
    let key = h.open(&root).unwrap();
    let files = active_files(&h.snapshot(&key));
    assert_eq!(files.len(), 2);
    assert!((0..files.len()).all(|i| files.oid(i).is_none()));
}

#[test]
fn open_folder_walk_stops_at_the_limit() {
    let git = Arc::new(FakeGit::new());
    let home = git.base().join("home");
    let h = Harness::with_model(git, |g| Model::with_folder_limit(g, home, 3));
    let root = h.base().join("folder");
    for name in ["a", "b", "c", "d", "e"] {
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(name), b"").unwrap();
    }
    let key = h.open(&root).unwrap();
    let snapshot = h.snapshot(&key);
    let Scan::Ready(files) = &snapshot.active.as_ref().unwrap().files else {
        panic!("files not ready");
    };
    assert_eq!(files.len(), 3);
    assert!(files.truncated());
    let names: Vec<&str> = files.iter().map(|(p, _)| p).collect();
    assert_eq!(names, ["a", "b", "c"]);
}

#[test]
fn open_close_emits_closed_and_drops_the_snapshot() {
    let h = Harness::new();
    let git_dir = h.git.init("repo");
    monorepo(&h.git, &git_dir);
    let key = h.open(&h.base().join("repo")).unwrap();
    h.model.close(&key);
    // Gone at once; `Closed` comes from the executor.
    assert!(h.model.snapshot(&key).is_none());
    assert!(h.model.open_projects().is_empty());
    h.host.run();
    assert_eq!(
        h.host.take_events(),
        vec![ProjectEvent::Closed {
            project: key.clone()
        }]
    );
}

#[test]
fn open_close_during_the_scan_stops_it() {
    let h = Harness::new();
    let git_dir = h.git.init("repo");
    monorepo(&h.git, &git_dir);
    h.model.open(&h.base().join("repo"));
    assert!(h.host.step());
    let key = h.model.open_projects().pop().unwrap();
    h.model.close(&key);
    h.host.run();
    let events = h.host.take_events();
    insta::assert_snapshot!(render_events(&events, h.base()), @r#"
    Opened Ok(Git { common_dir: "[base]/repo/.git" })
    Closed Git { common_dir: "[base]/repo/.git" }
    "#);
    assert!(!h.git.calls().contains(&Call::IndexFiles(git_dir)));
}
