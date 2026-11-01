use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::handle::hook;
use crate::testing::{TestHead, TestRepo, discover};
use crate::*;

fn found(path: &Path) -> Discovered {
    discover(path, &[]).unwrap().expect("a repository")
}

fn main_ref() -> RefName {
    RefName::new("refs/heads/main").unwrap()
}

/// A repository with one commit on `main`.
fn with_commit() -> (TestRepo, Oid) {
    let t = TestRepo::new();
    let tree = t.tree(&[("README.md", b"hello\n")]);
    let commit = t.commit(tree, &[], "first");
    t.set_ref("refs/heads/main", commit);
    (t, commit)
}

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

// ---- repo_* -------------------------------------------------------------

#[test]
fn repo_discover_in_a_subdirectory() {
    let (t, commit) = with_commit();
    std::fs::create_dir_all(t.root().join("src/deep")).unwrap();
    let d = found(&t.root().join("src/deep"));
    let location = d.repo.location();
    assert_eq!(location.common_dir, t.common_dir());
    assert_eq!(location.hash, HashKind::Sha1);
    assert!(!location.bare);
    let info = d.worktree.unwrap();
    assert_eq!(info.id, WorktreeId::Main);
    assert!(info.is_main);
    assert_eq!(info.root, t.root());
    assert_eq!(info.git_dir, t.common_dir());
    assert_eq!(
        info.head,
        HeadState::Branch {
            name: main_ref(),
            commit: Some(commit)
        }
    );
    assert!(!info.prunable);
}

#[test]
fn repo_discover_from_a_file_path() {
    let (t, _) = with_commit();
    t.write_file("src/lib.rs", b"");
    let d = found(&t.root().join("src/lib.rs"));
    assert_eq!(d.worktree.unwrap().id, WorktreeId::Main);
}

#[test]
fn repo_discover_in_the_git_dir_has_no_worktree() {
    let (t, _) = with_commit();
    for path in [t.common_dir().to_path_buf(), t.common_dir().join("objects")] {
        let d = found(&path);
        assert_eq!(d.repo.location().common_dir, t.common_dir());
        assert!(d.worktree.is_none(), "{}", path.display());
    }
}

#[test]
fn repo_discover_in_a_plain_folder_is_none() {
    let t = TestRepo::new();
    let plain = t.base().join("plain/sub");
    std::fs::create_dir_all(&plain).unwrap();
    // The ceiling keeps discovery from wandering above the temporary directory.
    assert!(
        discover(&plain, &[t.base().to_path_buf()])
            .unwrap()
            .is_none()
    );
}

#[test]
fn repo_discover_stops_at_ceilings() {
    let (t, _) = with_commit();
    let sub = t.root().join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    // A ceiling at the worktree root: git doesn't climb into it.
    assert!(discover(&sub, &[t.root().to_path_buf()]).unwrap().is_none());
    // A ceiling above the repository doesn't hide it.
    assert!(discover(&sub, &[t.base().to_path_buf()]).unwrap().is_some());
    // Starting in a ceiling still checks it, as git does.
    assert!(
        discover(t.root(), &[t.root().to_path_buf()])
            .unwrap()
            .is_some()
    );
    // An unrelated ceiling is ignored.
    assert!(
        discover(&sub, &[PathBuf::from("/nonexistent/elsewhere")])
            .unwrap()
            .is_some()
    );
}

#[test]
fn repo_discover_a_bare_repository() {
    let t = TestRepo::bare();
    let d = found(t.common_dir());
    assert!(d.repo.location().bare);
    assert!(d.worktree.is_none());
    assert!(d.repo.worktrees().unwrap().is_empty());
}

#[test]
fn repo_discover_in_a_linked_worktree() {
    let (t, commit) = with_commit();
    let root = t.base().join("feature");
    let admin = t.add_worktree("feature", &root, TestHead::Detached(commit));
    std::fs::create_dir_all(root.join("a/b")).unwrap();
    let d = found(&root.join("a/b"));
    assert_eq!(d.repo.location().common_dir, t.common_dir());
    let info = d.worktree.unwrap();
    assert_eq!(info.id, WorktreeId::Linked("feature".into()));
    assert!(!info.is_main);
    assert_eq!(info.root, std::fs::canonicalize(&root).unwrap());
    assert_eq!(info.git_dir, admin);
    assert_eq!(info.head, HeadState::Detached { commit });
    // From inside the linked worktree's own git directory: no worktree.
    assert!(found(&admin).worktree.is_none());
}

#[test]
fn repo_sha256_oids_are_64_digits() {
    let t = TestRepo::sha256();
    let tree = t.tree(&[("a.txt", b"a\n")]);
    let commit = t.commit(tree, &[], "first");
    t.set_ref("refs/heads/main", commit);
    let d = found(t.root());
    assert_eq!(d.repo.location().hash, HashKind::Sha256);
    assert_eq!(commit.kind(), HashKind::Sha256);
    let HeadState::Branch {
        commit: Some(head), ..
    } = d.worktree.unwrap().head
    else {
        panic!("HEAD is main");
    };
    assert_eq!(head.to_hex().len(), 64);
    assert_eq!(d.repo.resolve(&main_ref()).unwrap(), Some(commit));
    let info = d.repo.commit(commit).unwrap();
    assert_eq!(info.tree, tree);
    assert_eq!(info.tree.to_hex().len(), 64);
    assert_eq!(
        &*d.repo
            .blob(Oid::for_blob(HashKind::Sha256, b"a\n"), 100)
            .unwrap(),
        b"a\n"
    );
}

#[test]
fn repo_reftable_is_unsupported() {
    let t = TestRepo::reftable();
    match discover(t.root(), &[]) {
        Err(GitError::Unsupported(UnsupportedRepo::Reftable)) => {}
        other => panic!("{other:?}"),
    }
}

#[test]
fn repo_unknown_extension_is_unsupported() {
    let t = TestRepo::new();
    t.append_config(
        "[core]\n\trepositoryformatversion = 1\n[extensions]\n\tfancyNewThing = true\n",
    );
    assert!(matches!(
        discover(t.root(), &[]),
        Err(GitError::Unsupported(UnsupportedRepo::Extension { .. }))
    ));
}

#[test]
fn repo_missing_path_is_an_io_error() {
    let t = TestRepo::new();
    assert!(matches!(
        discover(&t.base().join("nope"), &[]),
        Err(GitError::Io(_))
    ));
}

#[test]
fn repo_handles_never_outlive_a_call() {
    let (t, commit) = with_commit();
    t.add_worktree(
        "feature",
        &t.base().join("feature"),
        TestHead::Branch("refs/heads/main"),
    );
    let cancel = no_cancel();
    let before = hook::opened_count();
    let d = found(t.root());
    assert_eq!(hook::live_count(), 0);
    let repo = d.repo;
    let worktrees = repo.worktrees().unwrap();
    assert_eq!(hook::live_count(), 0);
    assert_eq!(worktrees.len(), 2);
    repo.resolve(&main_ref()).unwrap();
    repo.commit(commit).unwrap();
    repo.log(commit, 10, &cancel).unwrap();
    repo.branches().unwrap();
    repo.default_branch().unwrap();
    let wt = repo.worktree(&WorktreeId::Main).unwrap();
    wt.head().unwrap();
    wt.tracked(&EntryLimits::default(), &cancel).unwrap();
    assert_eq!(hook::live_count(), 0);
    assert!(hook::opened_count() > before);
}

#[test]
fn repo_location_ignores_how_the_path_is_spelled() {
    let (t, _) = with_commit();
    let a = found(t.root());
    let b = found(&t.root().join(".").join("..").join("repo"));
    assert_eq!(a.repo.location(), b.repo.location());
}

// ---- worktrees_* ----------------------------------------------------------

#[test]
fn worktrees_lists_main_then_linked_by_name() {
    let (t, commit) = with_commit();
    for name in ["b", "main", "a"] {
        t.add_worktree(
            name,
            &t.base().join(format!("wt-{name}")),
            TestHead::Detached(commit),
        );
    }
    let repo = t.repo();
    let ids: Vec<WorktreeId> = repo
        .worktrees()
        .unwrap()
        .into_iter()
        .map(|w| w.id)
        .collect();
    assert_eq!(
        ids,
        vec![
            WorktreeId::Main,
            WorktreeId::Linked("a".into()),
            WorktreeId::Linked("b".into()),
            WorktreeId::Linked("main".into()),
        ]
    );
}

#[test]
fn worktrees_main_and_a_linked_worktree_named_main_differ() {
    let (t, commit) = with_commit();
    let linked_root = t.base().join("other");
    t.add_worktree("main", &linked_root, TestHead::Detached(commit));
    let repo = t.repo();
    let main = repo.worktree(&WorktreeId::Main).unwrap();
    let linked = repo.worktree(&WorktreeId::Linked("main".into())).unwrap();
    assert_ne!(main.id(), linked.id());
    assert_eq!(main.root(), t.root());
    assert_eq!(linked.root(), std::fs::canonicalize(linked_root).unwrap());
    assert_eq!(linked.git_dir(), t.common_dir().join("worktrees/main"));
    assert_eq!(
        main.head().unwrap(),
        HeadState::Branch {
            name: main_ref(),
            commit: Some(commit)
        }
    );
    assert_eq!(linked.head().unwrap(), HeadState::Detached { commit });
}

#[test]
fn worktrees_report_locks() {
    let (t, commit) = with_commit();
    t.add_worktree("usb", &t.base().join("usb"), TestHead::Detached(commit));
    t.add_worktree("empty", &t.base().join("empty"), TestHead::Detached(commit));
    t.add_worktree("free", &t.base().join("free"), TestHead::Detached(commit));
    t.lock_worktree("usb", "on a removable drive\n");
    t.lock_worktree("empty", "");
    let list = t.repo().worktrees().unwrap();
    let locked = |name: &str| {
        list.iter()
            .find(|w| w.id == WorktreeId::Linked(name.into()))
            .unwrap()
            .locked
            .clone()
    };
    assert_eq!(locked("usb").as_deref(), Some("on a removable drive"));
    assert_eq!(locked("empty").as_deref(), Some(""));
    assert_eq!(locked("free"), None);
    assert_eq!(list[0].locked, None);
}

#[test]
fn worktrees_prunable_root_is_the_recorded_path_normalized() {
    let (t, commit) = with_commit();
    let root = t.base().join("gone");
    let admin = t.add_worktree("gone", &root, TestHead::Detached(commit));
    std::fs::remove_dir_all(&root).unwrap();
    // Recorded with `.` and `..` components; normalized only lexically.
    let recorded = format!("{}/x/./../gone/.git\n", t.base().display());
    std::fs::write(admin.join("gitdir"), recorded).unwrap();
    let list = t.repo().worktrees().unwrap();
    let gone = list
        .iter()
        .find(|w| w.id == WorktreeId::Linked("gone".into()))
        .unwrap();
    assert!(gone.prunable);
    assert_eq!(gone.root, t.base().join("gone"));
    assert_eq!(gone.git_dir, admin);
    assert!(matches!(t.repo().worktree(&gone.id), Err(GitError::Io(_))));
    // Discovery from the main worktree still works.
    assert!(!found(t.root()).worktree.unwrap().prunable);
}

#[test]
fn worktrees_relative_gitdir_is_resolved_from_the_admin_dir() {
    let (t, commit) = with_commit();
    let root = t.base().join("rel");
    let admin = t.add_worktree("rel", &root, TestHead::Detached(commit));
    // `extensions.relativeWorktrees`: <base>/repo/.git/worktrees/rel → <base>/rel/.git
    std::fs::write(admin.join("gitdir"), "../../../../rel/.git\n").unwrap();
    t.append_config(
        "[core]\n\trepositoryformatversion = 1\n[extensions]\n\trelativeWorktrees = true\n",
    );
    let list = t.repo().worktrees().unwrap();
    let rel = list
        .iter()
        .find(|w| w.id == WorktreeId::Linked("rel".into()))
        .unwrap();
    assert_eq!(rel.root, std::fs::canonicalize(&root).unwrap());
    assert!(!rel.prunable);
}

#[test]
fn worktrees_skip_admin_dirs_without_gitdir_or_head() {
    let (t, commit) = with_commit();
    let admin = t.add_worktree("half", &t.base().join("half"), TestHead::Detached(commit));
    std::fs::remove_file(admin.join("gitdir")).unwrap();
    let admin2 = t.add_worktree(
        "nohead",
        &t.base().join("nohead"),
        TestHead::Detached(commit),
    );
    std::fs::remove_file(admin2.join("HEAD")).unwrap();
    std::fs::write(t.common_dir().join("worktrees/stray-file"), b"").unwrap();
    let list = t.repo().worktrees().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, WorktreeId::Main);
}

#[test]
fn worktrees_head_states() {
    let (t, commit) = with_commit();
    t.add_worktree(
        "unborn",
        &t.base().join("unborn"),
        TestHead::Branch("refs/heads/new"),
    );
    t.add_worktree(
        "detached",
        &t.base().join("detached"),
        TestHead::Detached(commit),
    );
    t.set_ref("refs/heads/topic", commit);
    t.add_worktree(
        "topic",
        &t.base().join("topic"),
        TestHead::Branch("refs/heads/topic"),
    );
    let list = t.repo().worktrees().unwrap();
    let head = |name: &str| {
        list.iter()
            .find(|w| w.id == WorktreeId::Linked(name.into()))
            .unwrap()
            .head
            .clone()
    };
    assert_eq!(
        head("unborn"),
        HeadState::Branch {
            name: RefName::new("refs/heads/new").unwrap(),
            commit: None
        }
    );
    assert_eq!(head("detached"), HeadState::Detached { commit });
    assert_eq!(
        head("topic"),
        HeadState::Branch {
            name: RefName::new("refs/heads/topic").unwrap(),
            commit: Some(commit)
        }
    );
}

#[test]
fn worktrees_head_is_reread() {
    let (t, commit) = with_commit();
    let wt = t.repo().worktree(&WorktreeId::Main).unwrap();
    assert_eq!(
        wt.head().unwrap(),
        HeadState::Branch {
            name: main_ref(),
            commit: Some(commit)
        }
    );
    t.set_head(TestHead::Detached(commit));
    assert_eq!(wt.head().unwrap(), HeadState::Detached { commit });
    t.set_head(TestHead::Branch("refs/heads/later"));
    assert_eq!(
        wt.head().unwrap(),
        HeadState::Branch {
            name: RefName::new("refs/heads/later").unwrap(),
            commit: None
        }
    );
}

#[test]
fn worktrees_unknown_worktree_is_not_found() {
    let t = TestRepo::new();
    let repo = t.repo();
    for id in [
        WorktreeId::Linked("nope".into()),
        WorktreeId::Linked("../x".into()),
    ] {
        assert!(
            matches!(repo.worktree(&id), Err(GitError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound)
        );
    }
}

// ---- refs_* ---------------------------------------------------------------

#[test]
fn refs_resolve_loose_packed_symbolic_and_tags() {
    let (t, commit) = with_commit();
    t.set_symbolic_ref("refs/heads/alias", "refs/heads/main");
    let tag = {
        let repo = gix::open_opts(t.common_dir(), gix::open::Options::isolated()).unwrap();
        let tag = gix::objs::Tag {
            target: commit.to_gix(),
            target_kind: gix::objs::Kind::Commit,
            name: "v1".into(),
            tagger: None,
            message: "release\n".into(),
            signature: None,
        };
        Oid::from_gix(&repo.write_object(&tag).unwrap())
    };
    t.set_packed_refs(&[("refs/tags/v1", tag), ("refs/heads/packed", commit)]);
    let repo = t.repo();
    let resolve = |name: &str| repo.resolve(&RefName::new(name).unwrap()).unwrap();
    assert_eq!(resolve("refs/heads/main"), Some(commit));
    assert_eq!(resolve("refs/heads/alias"), Some(commit));
    assert_eq!(resolve("refs/heads/packed"), Some(commit));
    assert_eq!(
        resolve("refs/tags/v1"),
        Some(commit),
        "annotated tags are peeled"
    );
    assert_eq!(resolve("HEAD"), Some(commit));
    assert_eq!(resolve("refs/heads/missing"), None);
    // `commit` peels the tag too.
    assert_eq!(repo.commit(tag).unwrap().id, commit);
}

#[test]
fn refs_commit_info() {
    let t = TestRepo::new();
    let tree = t.tree(&[("a", b"a")]);
    let first = t.commit_at(tree, &[], "first", 1_700_000_000);
    let second = t.commit_at(
        tree,
        &[first],
        "Fix the parser\nacross lines\n\nBody text.\n",
        1_700_000_100,
    );
    let info = t.repo().commit(second).unwrap();
    assert_eq!(info.id, second);
    assert_eq!(info.tree, tree);
    assert_eq!(&*info.parents, &[first]);
    assert_eq!(&*info.summary, b"Fix the parser across lines");
    assert_eq!(&*info.author.name, b"Test");
    assert_eq!(&*info.author.email, b"test@example.com");
    assert_eq!(
        info.author.time,
        GitTime {
            seconds: 1_700_000_100,
            offset: 3600
        }
    );
    assert_eq!(
        info.committed,
        GitTime {
            seconds: 1_700_000_100,
            offset: 3600
        }
    );
    let missing = Oid::from_hex(HashKind::Sha1, &"1".repeat(40)).unwrap();
    assert!(matches!(t.repo().commit(missing), Err(GitError::ObjectMissing(id)) if id == missing));
    assert!(matches!(t.repo().commit(tree), Err(GitError::Corrupt(_))));
}

#[test]
fn refs_log_is_newest_first_and_limited() {
    let t = TestRepo::new();
    let tree = t.tree(&[("a", b"a")]);
    // root ── a(200) ── merge(400)
    //     └── b(300) ──┘
    let root = t.commit_at(tree, &[], "root", 100);
    let a = t.commit_at(tree, &[root], "a", 200);
    let b = t.commit_at(tree, &[root], "b", 300);
    let merge = t.commit_at(tree, &[a, b], "merge", 400);
    let repo = t.repo();
    let cancel = no_cancel();
    let ids = |limit| -> Vec<Oid> {
        repo.log(merge, limit, &cancel)
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect()
    };
    assert_eq!(ids(10), vec![merge, b, a, root]);
    assert_eq!(ids(2), vec![merge, b]);
    assert!(ids(0).is_empty());
    let missing = Oid::from_hex(HashKind::Sha1, &"2".repeat(40)).unwrap();
    assert!(repo.log(missing, 5, &cancel).is_err());
}

#[test]
fn refs_cancelled_walks_return_cancelled() {
    let (t, commit) = with_commit();
    let repo = t.repo();
    let cancel = AtomicBool::new(true);
    assert!(matches!(
        repo.log(commit, 5, &cancel),
        Err(GitError::Cancelled)
    ));
    assert!(matches!(
        repo.root_commits(commit, 5, &cancel),
        Err(GitError::Cancelled)
    ));
    assert!(matches!(
        repo.merge_base(commit, commit, &cancel),
        Err(GitError::Cancelled)
    ));
    let tree = repo.commit(commit).unwrap().tree;
    assert!(matches!(
        repo.tree_paths(tree, &EntryLimits::default(), &cancel),
        Err(GitError::Cancelled)
    ));
}

#[test]
fn refs_merge_base_and_ancestry() {
    let t = TestRepo::new();
    let tree = t.tree(&[("a", b"a")]);
    let base = t.commit(tree, &[], "base");
    let left = t.commit(tree, &[base], "left");
    let right = t.commit(tree, &[base], "right");
    let right2 = t.commit(tree, &[right], "right 2");
    let other_root = t.commit(tree, &[], "unrelated");
    let repo = t.repo();
    let c = no_cancel();
    assert_eq!(repo.merge_base(left, right2, &c).unwrap(), Some(base));
    assert_eq!(repo.merge_base(right, right2, &c).unwrap(), Some(right));
    assert_eq!(repo.merge_base(left, other_root, &c).unwrap(), None);
    assert!(repo.is_ancestor(base, right2, &c).unwrap());
    assert!(repo.is_ancestor(right2, right2, &c).unwrap());
    assert!(!repo.is_ancestor(right2, base, &c).unwrap());
    assert!(!repo.is_ancestor(left, right2, &c).unwrap());
    assert!(!repo.is_ancestor(other_root, left, &c).unwrap());
}

#[test]
fn refs_root_commits() {
    let t = TestRepo::new();
    let tree = t.tree(&[("a", b"a")]);
    let r1 = t.commit(tree, &[], "root 1");
    let r2 = t.commit(tree, &[], "root 2");
    let a = t.commit(tree, &[r1], "a");
    let merge = t.commit(tree, &[a, r2], "merge");
    let repo = t.repo();
    let c = no_cancel();
    let mut roots = repo.root_commits(merge, 10, &c).unwrap();
    roots.sort();
    let mut expected = vec![r1, r2];
    expected.sort();
    assert_eq!(roots, expected);
    assert_eq!(repo.root_commits(merge, 1, &c).unwrap().len(), 1);
    assert_eq!(repo.root_commits(r1, 10, &c).unwrap(), vec![r1]);
}

#[test]
fn refs_advance_since_walks_the_new_commits_only() {
    let t = TestRepo::new();
    let tree = t.tree(&[("a", b"a")]);
    let base = t.commit(tree, &[], "base");
    let a = t.commit(tree, &[base], "a");
    let b = t.commit(tree, &[a], "b");
    let repo = t.repo();
    let c = no_cancel();
    assert_eq!(
        repo.advance_since(a, b, 10, &c).unwrap(),
        Some(Advance {
            commits: 1,
            roots: vec![]
        })
    );
    assert_eq!(
        repo.advance_since(base, b, 10, &c)
            .unwrap()
            .unwrap()
            .commits,
        2
    );
    assert_eq!(
        repo.advance_since(b, b, 10, &c).unwrap(),
        Some(Advance::default())
    );
    // The bound: more new commits than that is no answer.
    assert_eq!(repo.advance_since(base, b, 1, &c).unwrap(), None);
    // The ancestor is the new tip's descendant, or on another branch.
    assert_eq!(repo.advance_since(b, a, 10, &c).unwrap(), None);
    let side = t.commit(tree, &[base], "side");
    assert_eq!(repo.advance_since(side, b, 10, &c).unwrap(), None);
    // An old tip that is not in the repository.
    let gone = Oid::from_hex(HashKind::Sha1, &"ab".repeat(20)).unwrap();
    assert_eq!(repo.advance_since(gone, b, 10, &c).unwrap(), None);
    assert!(matches!(
        repo.advance_since(a, b, 10, &AtomicBool::new(true)),
        Err(GitError::Cancelled)
    ));
}

#[test]
fn refs_advance_since_finds_the_roots_a_merge_brings() {
    let t = TestRepo::new();
    let tree = t.tree(&[("a", b"a")]);
    let base = t.commit(tree, &[], "base");
    let other_root = t.commit(tree, &[], "other root");
    let other = t.commit(tree, &[other_root], "other");
    let merge = t.commit(tree, &[base, other], "merge");
    let repo = t.repo();
    let advance = repo
        .advance_since(base, merge, 10, &no_cancel())
        .unwrap()
        .unwrap();
    assert_eq!(advance.commits, 3);
    assert_eq!(advance.roots, vec![other_root]);
}

#[test]
fn refs_stash_parts() {
    let t = TestRepo::new();
    let tree = t.tree(&[("a", b"a")]);
    let head = t.commit(tree, &[], "head");
    let index = t.commit(tree, &[head], "index on main");
    let untracked = t.commit(t.tree(&[("new", b"n")]), &[], "untracked files on main");
    let plain = t.commit(tree, &[head, index], "WIP on main");
    let with_untracked = t.commit(tree, &[head, index, untracked], "WIP on main");
    let repo = t.repo();
    assert_eq!(
        repo.stash(plain).unwrap(),
        StashInfo {
            worktree: plain,
            base: head,
            index,
            untracked: None
        }
    );
    assert_eq!(
        repo.stash(with_untracked).unwrap().untracked,
        Some(untracked)
    );
    assert!(matches!(repo.stash(head), Err(GitError::Corrupt(_))));
}

#[test]
fn refs_tree_paths_and_blobs() {
    let t = TestRepo::new();
    let link = t.blob(b"target");
    let exe = t.blob(b"#!/bin/sh\n");
    let file = t.blob(b"0123456789");
    let tree = t.tree_entries(&[
        ("src/main.rs", Mode::File, file),
        ("src/bin/run", Mode::Executable, exe),
        ("link", Mode::Symlink, link),
        ("a.txt", Mode::File, file),
    ]);
    let repo = t.repo();
    let c = no_cancel();
    let paths: Vec<(String, Mode)> = repo
        .tree_paths(tree, &EntryLimits::default(), &c)
        .unwrap()
        .into_iter()
        .map(|(p, m)| (String::from_utf8(p.as_bytes().to_vec()).unwrap(), m))
        .collect();
    assert_eq!(
        paths,
        vec![
            ("a.txt".to_owned(), Mode::File),
            ("link".to_owned(), Mode::Symlink),
            ("src/bin/run".to_owned(), Mode::Executable),
            ("src/main.rs".to_owned(), Mode::File),
        ]
    );
    assert!(matches!(
        repo.tree_paths(tree, &EntryLimits { entries: 3 }, &c),
        Err(GitError::TooManyEntries)
    ));
    assert_eq!(&*repo.blob(file, 10).unwrap(), b"0123456789");
    assert!(matches!(
        repo.blob(file, 9),
        Err(GitError::TooLarge { size: 10 })
    ));
    let missing = Oid::for_blob(HashKind::Sha1, b"never written");
    assert!(matches!(repo.blob(missing, 100), Err(GitError::ObjectMissing(id)) if id == missing));
    assert!(matches!(
        repo.blob(tree, 1 << 20),
        Err(GitError::Corrupt(_))
    ));
}

#[test]
fn refs_tree_paths_skip_invalid_names() {
    let t = TestRepo::new();
    let blob = t.blob(b"x");
    let repo_gix = gix::open_opts(t.common_dir(), gix::open::Options::isolated()).unwrap();
    let mut tree = gix::objs::Tree::empty();
    for name in ["..", "ok"] {
        tree.entries.push(gix::objs::tree::Entry {
            mode: gix::objs::tree::EntryKind::Blob.into(),
            filename: name.into(),
            oid: blob.to_gix(),
        });
    }
    let tree = Oid::from_gix(&repo_gix.write_object(&tree).unwrap());
    let paths = t
        .repo()
        .tree_paths(tree, &EntryLimits::default(), &no_cancel())
        .unwrap();
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].0.as_bytes(), b"ok");
}

#[test]
fn refs_default_branch() {
    let (t, commit) = with_commit();
    let repo = t.repo();
    // No remote: the local main.
    assert_eq!(repo.default_branch().unwrap(), Some(main_ref()));
    // origin/HEAD wins.
    t.append_config("[remote \"origin\"]\n\turl = https://example.com/x.git\n");
    t.set_ref("refs/remotes/origin/trunk", commit);
    t.set_symbolic_ref("refs/remotes/origin/HEAD", "refs/remotes/origin/trunk");
    assert_eq!(
        repo.default_branch().unwrap(),
        RefName::new("refs/remotes/origin/trunk")
    );

    // The only remote when there is no origin; then master; then nothing.
    let t = TestRepo::new();
    let tree = t.tree(&[("a", b"a")]);
    let c = t.commit(tree, &[], "c");
    t.append_config("[remote \"upstream\"]\n\turl = https://example.com/u.git\n");
    t.set_ref("refs/remotes/upstream/dev", c);
    t.set_symbolic_ref("refs/remotes/upstream/HEAD", "refs/remotes/upstream/dev");
    assert_eq!(
        t.repo().default_branch().unwrap(),
        RefName::new("refs/remotes/upstream/dev")
    );
    t.append_config("[remote \"second\"]\n\turl = https://example.com/s.git\n");
    assert_eq!(t.repo().default_branch().unwrap(), None);
    t.set_ref("refs/heads/master", c);
    assert_eq!(
        t.repo().default_branch().unwrap(),
        RefName::new("refs/heads/master")
    );
}

#[test]
fn refs_remotes() {
    let t = TestRepo::new();
    t.append_config(concat!(
        "[url \"https://github.com/\"]\n\tinsteadOf = gh:\n",
        "[remote \"origin\"]\n\turl = gh:me/x.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n",
        "[remote \"fork\"]\n\turl = https://example.com/fork.git\n\tpushurl = ssh://git@example.com/fork.git\n",
    ));
    let remotes = t.repo().remotes().unwrap();
    let names: Vec<&str> = remotes.iter().map(|r| &*r.name).collect();
    assert_eq!(names, ["fork", "origin"]);
    assert_eq!(
        remotes[0].fetch_url.as_deref(),
        Some("https://example.com/fork.git")
    );
    assert_eq!(
        remotes[0].push_url.as_deref(),
        Some("ssh://git@example.com/fork.git")
    );
    assert_eq!(
        remotes[1].fetch_url.as_deref(),
        Some("https://github.com/me/x.git")
    );
    assert_eq!(
        remotes[1].push_url.as_deref(),
        Some("https://github.com/me/x.git")
    );
}

fn rb(remote: &str, url: Option<&str>, branch: &str) -> Option<RemoteBranch> {
    Some(RemoteBranch {
        remote: remote.into(),
        url: url.map(Arc::from),
        branch: RefName::new(branch).unwrap(),
    })
}

/// Branches `b1`…`b6` on one commit, with `extra` appended to the config.
fn branch_table(extra: &str) -> Vec<BranchInfo> {
    let t = TestRepo::new();
    let tree = t.tree(&[("a", b"a")]);
    let c = t.commit(tree, &[], "c");
    for b in ["b1", "b2", "b3", "b4", "b5", "b6"] {
        t.set_ref(&format!("refs/heads/{b}"), c);
    }
    t.append_config(concat!(
        "[remote \"origin\"]\n\turl = https://example.com/origin.git\n",
        "[remote \"fork\"]\n\turl = https://example.com/fork.git\n",
        "[branch \"b1\"]\n\tremote = origin\n\tmerge = refs/heads/b1\n",
        "[branch \"b2\"]\n\tremote = origin\n\tmerge = refs/heads/other\n",
        "[branch \"b4\"]\n\tremote = origin\n\tmerge = b4\n\tpushRemote = fork\n",
        "[branch \"b5\"]\n\tremote = .\n\tmerge = refs/heads/b1\n",
        "[branch \"b6\"]\n\tremote = https://example.com/direct.git\n\tmerge = refs/heads/b6\n",
    ));
    t.append_config(extra);
    t.repo().branches().unwrap()
}

#[test]
fn refs_branches_upstream_and_push_targets() {
    let origin = Some("https://example.com/origin.git");
    let fork = Some("https://example.com/fork.git");
    let direct = Some("https://example.com/direct.git");

    // push.default unset = simple.
    let simple = branch_table("");
    let names: Vec<&str> = simple.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "refs/heads/b1",
            "refs/heads/b2",
            "refs/heads/b3",
            "refs/heads/b4",
            "refs/heads/b5",
            "refs/heads/b6"
        ]
    );
    let table: Vec<(Option<RemoteBranch>, Option<RemoteBranch>)> =
        simple.into_iter().map(|b| (b.upstream, b.push)).collect();
    assert_eq!(
        table,
        vec![
            // Same name upstream: push to it.
            (
                rb("origin", origin, "refs/heads/b1"),
                rb("origin", origin, "refs/heads/b1")
            ),
            // simple refuses an upstream with another name.
            (rb("origin", origin, "refs/heads/other"), None),
            // No upstream, push remote origin is not triangular: refused.
            (None, None),
            // Triangular (pushRemote = fork): the same name on fork.
            (
                rb("origin", origin, "refs/heads/b4"),
                rb("fork", fork, "refs/heads/b4")
            ),
            // The local repository as upstream.
            (rb(".", None, "refs/heads/b1"), None),
            // A URL instead of a remote name.
            (
                rb("https://example.com/direct.git", direct, "refs/heads/b6"),
                rb("https://example.com/direct.git", direct, "refs/heads/b6")
            ),
        ]
    );

    let pushes = |extra: &str| -> Vec<Option<RemoteBranch>> {
        branch_table(extra).into_iter().map(|b| b.push).collect()
    };
    assert_eq!(
        pushes("[push]\n\tdefault = current\n"),
        vec![
            rb("origin", origin, "refs/heads/b1"),
            rb("origin", origin, "refs/heads/b2"),
            rb("origin", origin, "refs/heads/b3"),
            rb("fork", fork, "refs/heads/b4"),
            // `git push .` with current updates the local branch of the same name.
            rb(".", None, "refs/heads/b5"),
            rb("https://example.com/direct.git", direct, "refs/heads/b6"),
        ]
    );
    assert_eq!(
        pushes("[push]\n\tdefault = upstream\n"),
        vec![
            rb("origin", origin, "refs/heads/b1"),
            rb("origin", origin, "refs/heads/other"),
            None,
            // Triangular: git push refuses with push.default = upstream.
            None,
            rb(".", None, "refs/heads/b1"),
            rb("https://example.com/direct.git", direct, "refs/heads/b6"),
        ]
    );
    assert!(
        pushes("[push]\n\tdefault = nothing\n")
            .iter()
            .all(Option::is_none)
    );
    // remote.pushDefault makes every branch without pushRemote triangular.
    let p = pushes("[remote]\n\tpushDefault = fork\n");
    assert_eq!(p[0], rb("fork", fork, "refs/heads/b1"));
    assert_eq!(p[2], rb("fork", fork, "refs/heads/b3"));
    // A push refspec on the push remote decides the destination.
    let p = pushes("[remote \"origin\"]\n\tpush = refs/heads/*:refs/heads/mine/*\n");
    assert_eq!(p[0], rb("origin", origin, "refs/heads/mine/b1"));
}

// ---- ref_reflog_* -----------------------------------------------------------

/// The 4 MiB `usage` passes.
const REFLOG_BYTES: u64 = 4 * 1024 * 1024;

/// One step of the archived reference reflogs (tests/fixtures/README.md
/// 「ref_reflog 夹具」).
fn ref_reflog_step(step: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/ref_reflog")
        .join(step)
}

/// Every file under `dir`, as `/`-separated paths relative to it, sorted.
fn files_under(dir: &Path) -> Vec<String> {
    fn visit(base: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(base, &path, out);
            } else {
                let rel = path.strip_prefix(base).unwrap();
                out.push(rel.to_str().unwrap().replace('\\', "/"));
            }
        }
    }
    let mut out = Vec::new();
    visit(dir, dir, &mut out);
    out.sort();
    out
}

/// A fresh repository with the step's `logs/` copied into its common
/// directory, and the references that have a reflog there.
fn with_step_reflogs(step: &str) -> (TestRepo, Vec<String>) {
    let t = TestRepo::new();
    let logs = ref_reflog_step(step).join("logs");
    let refs = files_under(&logs);
    for rel in &refs {
        let to = t.common_dir().join("logs").join(rel);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(logs.join(rel), to).unwrap();
    }
    (t, refs)
}

/// git's reading of one reflog, oldest first: (seconds, new OID, message).
fn git_reading(step: &str, name: &str) -> Vec<(i64, String, String)> {
    let path = ref_reflog_step(step).join("expected").join(name);
    let text = std::fs::read_to_string(&path).unwrap();
    let mut out: Vec<(i64, String, String)> = text
        .lines()
        .map(|line| {
            let mut parts = line.splitn(3, ' ');
            let seconds = parts.next().unwrap().parse().unwrap();
            let new = parts.next().unwrap().to_owned();
            (seconds, new, parts.next().unwrap().to_owned())
        })
        .collect();
    // `git log -g` lists newest first.
    out.reverse();
    out
}

fn reading(entries: &[ReflogEntry]) -> Vec<(i64, String, String)> {
    entries
        .iter()
        .map(|e| {
            (
                e.time.seconds,
                e.new.map(|o| o.to_hex()).unwrap_or_default(),
                String::from_utf8(e.message.to_vec()).unwrap(),
            )
        })
        .collect()
}

fn ref_reflog_of(t: &TestRepo, name: &str) -> RefReflog {
    t.repo()
        .ref_reflog(&RefName::new(name).unwrap(), REFLOG_BYTES)
        .unwrap()
}

#[test]
fn ref_reflog_reads_every_fixture_reflog_as_git_does() {
    let mut read = 0;
    for step in ["branches", "stash", "stash-drop", "stash-cleared"] {
        let (t, refs) = with_step_reflogs(step);
        assert_eq!(files_under(&ref_reflog_step(step).join("expected")), refs);
        for name in &refs {
            let log = ref_reflog_of(&t, name);
            assert!(log.exists && log.complete, "{step} {name}");
            assert_eq!(
                reading(&log.entries),
                git_reading(step, name),
                "{step} {name}"
            );
            read += 1;
        }
        let absent = std::fs::read_to_string(ref_reflog_step(step).join("absent")).unwrap();
        for name in absent.lines() {
            assert_eq!(
                ref_reflog_of(&t, name),
                RefReflog {
                    exists: false,
                    entries: Vec::new(),
                    complete: true
                },
                "{step} {name}"
            );
        }
    }
    // Six reflogs after the branch steps, then the stash in two steps.
    assert_eq!(read, 8);
}

#[test]
fn ref_reflog_branch_created_renamed_and_reset() {
    let (t, _) = with_step_reflogs("branches");
    // `branch feat`, a commit, `branch -m feat topic`, `branch -f topic main`:
    // the rename took the old name's records along.
    let topic = ref_reflog_of(&t, "refs/heads/topic").entries;
    assert_eq!(topic.len(), 4);
    assert_eq!(topic[0].old, None);
    assert_eq!(topic[0].action, ReflogAction::BranchCreated);
    assert_eq!(&*topic[0].message, b"branch: Created from main");
    assert!(matches!(
        topic[1].action,
        ReflogAction::Commit {
            amend: false,
            initial: false
        }
    ));
    assert_eq!(
        topic[2].action,
        ReflogAction::BranchRenamed {
            from: RefName::new("refs/heads/feat").unwrap(),
            to: RefName::new("refs/heads/topic").unwrap(),
        }
    );
    assert_eq!(topic[2].old, topic[2].new);
    // `branch -f` is not classified; its old value is the dropped commit.
    assert_eq!(&*topic[3].message, b"branch: Reset to main");
    assert_eq!(topic[3].action, ReflogAction::Other);
    assert_eq!(topic[3].old, topic[2].new);
    assert_ne!(topic[3].new, topic[3].old);
    assert!(
        topic
            .windows(2)
            .all(|w| w[0].time.seconds < w[1].time.seconds)
    );
    // The old name has no reflog any more.
    assert!(!ref_reflog_of(&t, "refs/heads/feat").exists);
    let other = ref_reflog_of(&t, "refs/heads/other").entries;
    assert_eq!(other.len(), 1);
    assert_eq!(other[0].action, ReflogAction::BranchCreated);
    assert_eq!(&*other[0].message, b"branch: Created from HEAD");
    assert!(!ref_reflog_of(&t, "refs/heads/gone").exists);
}

#[test]
fn ref_reflog_remote_tracking_branches() {
    let (t, _) = with_step_reflogs("branches");
    let main = ref_reflog_of(&t, "refs/remotes/origin/main").entries;
    assert_eq!(main.len(), 2);
    assert_eq!(main[0].old, None);
    assert_eq!(main[1].old, main[0].new);
    assert_eq!(&*main[0].message, b"fetch -q origin: storing head");
    assert_eq!(&*main[1].message, b"fetch -q origin: fast-forward");
    assert!(main.iter().all(|e| e.action == ReflogAction::Other));
}

#[test]
fn ref_reflog_stash_push_and_drop() {
    let (t, _) = with_step_reflogs("stash");
    let pushed = ref_reflog_of(&t, "refs/stash").entries;
    let messages: Vec<&[u8]> = pushed.iter().map(|e| &*e.message).collect();
    assert_eq!(
        messages,
        [
            &b"On main: stash one"[..],
            b"On main: stash two",
            b"On main: stash three"
        ]
    );
    // `stash drop stash@{1}` rewrites the file without the middle entry,
    // and chains the old values again.
    let (t, _) = with_step_reflogs("stash-drop");
    let dropped = ref_reflog_of(&t, "refs/stash").entries;
    assert_eq!(dropped.len(), 2);
    assert_eq!(dropped[0].new, pushed[0].new);
    assert_eq!(dropped[1].new, pushed[2].new);
    assert_eq!(dropped[1].old, dropped[0].new);
    // Dropping the last entry deletes the reflog with the reference.
    let (t, _) = with_step_reflogs("stash-cleared");
    assert!(!ref_reflog_of(&t, "refs/stash").exists);
}

/// A synthetic reflog of `n` records, one second apart, and where each
/// line starts.
fn synthetic_reflog(n: i64) -> (Vec<u8>, Vec<u64>) {
    let (a, b) = ("1".repeat(40), "2".repeat(40));
    let mut bytes = Vec::new();
    let mut starts = Vec::new();
    for i in 0..n {
        starts.push(bytes.len() as u64);
        bytes.extend_from_slice(
            format!(
                "{a} {b} Fixture <fixture@example.com> {} +0100\tcommit: {i:05}\n",
                1_700_000_000 + i
            )
            .as_bytes(),
        );
    }
    (bytes, starts)
}

fn write_ref_log(t: &TestRepo, name: &str, bytes: &[u8]) {
    let path = t.common_dir().join("logs").join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn ref_reflog_reads_at_most_max_bytes_from_the_end() {
    let t = TestRepo::new();
    let (bytes, starts) = synthetic_reflog(500);
    write_ref_log(&t, "refs/heads/long", &bytes);
    let name = RefName::new("refs/heads/long").unwrap();
    let len = bytes.len() as u64;
    for max in [1000, 4096, len / 2, len - 1] {
        let log = t.repo().ref_reflog(&name, max).unwrap();
        assert!(log.exists && !log.complete, "{max}");
        // Lines that start after the bound, oldest first; the one cut by
        // the bound is dropped.
        let floor = len - max;
        let first = starts.iter().position(|&s| s > floor).unwrap() as i64;
        let seconds: Vec<i64> = log.entries.iter().map(|e| e.time.seconds).collect();
        let want: Vec<i64> = (first..500).map(|i| 1_700_000_000 + i).collect();
        assert_eq!(seconds, want, "{max}");
    }
    // The whole file fits: everything, complete.
    for max in [len, len + 1, u64::MAX] {
        let log = t.repo().ref_reflog(&name, max).unwrap();
        assert!(log.complete);
        assert_eq!(log.entries.len(), 500);
        assert_eq!(log.entries[0].time.seconds, 1_700_000_000);
    }
    let log = t.repo().ref_reflog(&name, 0).unwrap();
    assert!(log.exists && log.entries.is_empty() && !log.complete);
}

#[test]
fn ref_reflog_without_a_file_or_with_a_directory_in_its_place() {
    let t = TestRepo::new();
    let (bytes, _) = synthetic_reflog(3);
    // `a/b` exists, so `logs/refs/heads/a` is a directory: `a` has no reflog.
    write_ref_log(&t, "refs/heads/a/b", &bytes);
    assert!(!ref_reflog_of(&t, "refs/heads/a").exists);
    assert_eq!(ref_reflog_of(&t, "refs/heads/a/b").entries.len(), 3);
    // `x` exists, so `x/y` can't: its path goes through a file.
    write_ref_log(&t, "refs/heads/x", &bytes);
    let log = ref_reflog_of(&t, "refs/heads/x/y");
    assert!(!log.exists && log.complete && log.entries.is_empty());
    // An empty file exists and has nothing; a line still being appended
    // is not read.
    write_ref_log(&t, "refs/heads/empty", b"");
    let log = ref_reflog_of(&t, "refs/heads/empty");
    assert!(log.exists && log.complete && log.entries.is_empty());
    let mut partial = bytes.clone();
    partial.extend_from_slice(&bytes[..60]);
    write_ref_log(&t, "refs/heads/partial", &partial);
    let log = ref_reflog_of(&t, "refs/heads/partial");
    assert!(log.complete);
    assert_eq!(log.entries.len(), 3);
}

#[test]
fn ref_reflog_of_head_is_the_main_worktrees() {
    let t = TestRepo::new();
    let fixture = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reflog/logs/HEAD"),
    )
    .unwrap();
    write_ref_log(&t, "HEAD", &fixture);
    let log = ref_reflog_of(&t, "HEAD");
    let wt = t.repo().worktree(&WorktreeId::Main).unwrap();
    let mut head = wt.head_reflog(i64::MIN, usize::MAX).unwrap().entries;
    head.reverse();
    assert!(log.exists && log.complete);
    assert_eq!(log.entries, head);
    assert_eq!(log.entries.len(), 9);
}
