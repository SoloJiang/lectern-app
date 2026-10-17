//! The production `GitAccess` on repositories written by
//! `lectern_git::testing::TestRepo` (no git CLI), against the same layout in
//! `FakeGit` (`real_repo_*`).

use std::path::Path;
use std::sync::Arc;

use lectern_git::testing::{IndexEntry, TestHead, TestRepo};
use lectern_git::{HashKind, Mode};
use lectern_services::project::{FileKind, Head, ProjectKey, Projects, WorktreeKey};

use super::support::*;
use crate::git_access::LecternGit;
use crate::model::Model;
use crate::testing::FakeGit;

/// (path, content, executable)
type Files = &'static [(&'static str, &'static [u8], bool)];

const MAIN: Files = &[
    (
        "package.json",
        br#"{ "name": "root", "workspaces": ["apps/*"] }"#,
        false,
    ),
    ("apps/web/package.json", br#"{ "name": "web" }"#, false),
    ("apps/web/src/index.ts", b"export {};\n", false),
    (
        "pom.xml",
        b"<project><artifactId>parent</artifactId><modules><module>svc</module></modules></project>",
        false,
    ),
    ("svc/src/Main.java", b"class Main {}\n", false),
    ("scripts/run.sh", b"#!/bin/sh\n", true),
    ("README.md", b"# root\n", false),
];

const FEATURE: Files = &[
    (
        "Cargo.toml",
        b"[workspace]\nmembers = [\"crates/*\"]\n",
        false,
    ),
    ("crates/a/Cargo.toml", b"[package]\nname = \"a\"\n", false),
    ("crates/a/src/lib.rs", b"", false),
];

/// Writes `files` into the worktree at `root` and its index.
fn write_real(repo: &TestRepo, root: &Path, git_dir: &Path, files: Files) {
    let mut entries = Vec::new();
    for (path, content, exec) in files {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, content).unwrap();
        let mode = if *exec { Mode::Executable } else { Mode::File };
        entries.push(IndexEntry::new(path, mode, repo.blob(content)));
    }
    repo.write_index(git_dir, &entries);
}

fn write_fake(git: &FakeGit, git_dir: &Path, files: Files) {
    for (path, content, exec) in files {
        git.write_unstaged(git_dir, path, content);
        let kind = if *exec {
            FileKind::Executable
        } else {
            FileKind::File
        };
        git.track(git_dir, path, kind);
    }
}

struct Real {
    repo: TestRepo,
    host: Arc<QueueHost>,
    model: Arc<Model>,
    linked: std::path::PathBuf,
}

/// A repository with a main worktree on `refs/heads/main` and a linked
/// worktree `feature` with a detached HEAD.
fn real(hash: HashKind) -> Real {
    let repo = match hash {
        HashKind::Sha1 => TestRepo::new(),
        HashKind::Sha256 => TestRepo::sha256(),
    };
    let tree = repo.tree(&[("README.md", b"# root\n")]);
    let commit = repo.commit(tree, &[], "initial");
    repo.set_ref("refs/heads/main", commit);
    write_real(&repo, repo.root(), repo.common_dir(), MAIN);
    let feature_root = repo.base().join("repo-feature");
    let linked = repo.add_worktree("feature", &feature_root, TestHead::Detached(commit));
    write_real(&repo, &feature_root, &linked, FEATURE);
    let host = Arc::new(QueueHost::default());
    let git = Arc::new(LecternGit::with_discover(lectern_git::testing::discover));
    let model = Model::new(git, repo.base().join("home"));
    model.attach(host.clone());
    Real {
        repo,
        host,
        model,
        linked,
    }
}

/// The same layout in `FakeGit`; the detached HEAD is `commit`.
fn fake(hash: HashKind, commit: &str) -> Harness {
    let h = Harness::new();
    let common = h.git.init_with("repo", hash);
    write_fake(&h.git, &common, MAIN);
    let linked = h.git.add_worktree(&common, "feature", "repo-feature");
    h.git.set_head(&linked, Head::Detached(commit.into()));
    write_fake(&h.git, &linked, FEATURE);
    h
}

impl Real {
    fn open(&self, path: &Path) -> ProjectKey {
        let request = self.model.open(path);
        self.host.run();
        let events = self.host.take_events();
        check_event_order(&events);
        opened(&events, request).unwrap()
    }

    fn render(&self, key: &ProjectKey) -> String {
        render(&self.model.snapshot(key).unwrap(), self.repo.base())
    }

    fn detached(&self) -> String {
        let key = ProjectKey::Git {
            common_dir: self.repo.common_dir().into(),
        };
        let snapshot = self.model.snapshot(&key).unwrap();
        match &snapshot.worktrees[1].head {
            Head::Detached(hex) => hex.to_string(),
            other => panic!("{other:?}"),
        }
    }
}

fn same_as_fake(hash: HashKind) {
    let real = real(hash);
    let key = real.open(&real.repo.root().join("apps/web"));
    let fake = fake(hash, &real.detached());
    let fake_key = fake.open(&fake.base().join("repo/apps/web")).unwrap();
    assert_eq!(real.render(&key), fake.render(&fake_key));

    // Switching to the linked worktree.
    real.model
        .set_active_worktree(&key, &WorktreeKey(real.linked.as_path().into()));
    real.host.run();
    let linked = fake.base().join("repo/.git/worktrees/feature");
    fake.model
        .set_active_worktree(&fake_key, &WorktreeKey(linked.as_path().into()));
    fake.host.run();
    assert_eq!(real.render(&key), fake.render(&fake_key));

    // `resolve_package` for the main worktree, now inactive.
    let main = WorktreeKey(real.repo.common_dir().into());
    let fake_main = WorktreeKey(fake.base().join("repo/.git").as_path().into());
    for rel in ["apps/web/src/index.ts", "svc/src/Main.java", "README.md"] {
        assert_eq!(
            real.model.resolve_package(&key, &main, rel),
            fake.model.resolve_package(&fake_key, &fake_main, rel),
            "{rel}"
        );
    }
}

#[test]
fn real_repo_sha1_matches_fake_git() {
    same_as_fake(HashKind::Sha1);
}

#[test]
fn real_repo_sha256_matches_fake_git() {
    same_as_fake(HashKind::Sha256);
}

#[test]
fn real_repo_open_and_reopen() {
    let real = real(HashKind::Sha1);
    let key = real.open(real.repo.root());
    insta::assert_snapshot!(real.render(&key));
    // A new staged manifest shows on the next open (the refresh until
    // fs_watch drives it, step 4).
    let mut files = MAIN.to_vec();
    files.push(("svc/go.mod", b"module example.com/svc\n", false));
    let files: &'static [_] = Vec::leak(files);
    write_real(&real.repo, real.repo.root(), real.repo.common_dir(), files);
    real.open(real.repo.root());
    let snapshot = real.model.snapshot(&key).unwrap();
    let lectern_services::project::Scan::Ready(packages) =
        &snapshot.active.as_ref().unwrap().packages
    else {
        panic!("packages not ready");
    };
    let svc = packages.iter().find(|p| &*p.dir == "svc").unwrap();
    assert_eq!(&*svc.name, "example.com/svc");
    assert_eq!(svc.manifests.len(), 1);
}

#[test]
fn real_repo_resolve_and_bare() {
    let real = real(HashKind::Sha1);
    let resolved = real
        .model
        .resolve(&real.repo.base().join("repo-feature/crates/a"))
        .unwrap();
    assert_eq!(
        resolved.worktree,
        Some(WorktreeKey(real.linked.as_path().into()))
    );
    assert_eq!(&*resolved.rel, "crates/a");
    // A linked worktree's administrative directory opens that worktree.
    let key = real.open(&real.linked);
    let snapshot = real.model.snapshot(&key).unwrap();
    assert_eq!(
        snapshot
            .active
            .as_ref()
            .unwrap()
            .worktree
            .as_ref()
            .unwrap()
            .0
            .as_ref(),
        real.linked.as_path()
    );

    let bare = TestRepo::bare();
    let host = Arc::new(QueueHost::default());
    let model = Model::new(
        Arc::new(LecternGit::with_discover(lectern_git::testing::discover)),
        bare.base().join("home"),
    );
    model.attach(host.clone());
    let request = model.open(bare.common_dir());
    host.run();
    let key = opened(&host.take_events(), request).unwrap();
    let snapshot = model.snapshot(&key).unwrap();
    assert_eq!(&*snapshot.name, "repo");
    assert!(snapshot.worktrees.is_empty());
    assert!(snapshot.active.is_none());
}
