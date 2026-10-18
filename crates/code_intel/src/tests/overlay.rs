//! `overlay_*`: `FakeGit` builds the cases of git's「工作区比较」table; after
//! the reconcile the overlay agrees with the worktree's actual content.

use std::collections::BTreeMap;

use crate::map::WorktreeState;
use crate::testing::FakeGit;

use super::support::{Harness, ts};

/// What the map must say about a path: its effective OID is this file's
/// content (canonical when `eol`), or the path is not in the map.
fn check_against_disk(h: &Harness) {
    let map = h.map();
    let root = h.git.root("main");
    for (path, state) in map.overlay.entries() {
        let on_disk = std::fs::read(root.join(&**path));
        match state {
            WorktreeState::Content(oid) => {
                let bytes = on_disk.unwrap_or_else(|_| panic!("{path} is not on disk"));
                assert_eq!(*oid, h.git.oid(&bytes), "{path}");
            }
            WorktreeState::Absent => assert!(on_disk.is_err(), "{path} is on disk"),
            WorktreeState::Unindexable => {}
        }
    }
}

struct Case {
    name: &'static str,
    /// Run after the first commit of `a.ts`, `b.ts` and `big.ts`.
    change: fn(&FakeGit),
    overlay: &'static [(&'static str, Expect)],
}

#[derive(Clone, Copy, Debug)]
enum Expect {
    /// Content of these bytes.
    Content(&'static str),
    Absent,
    Unindexable,
}

const CASES: &[Case] = &[
    Case {
        name: "clean",
        change: |_| {},
        overlay: &[],
    },
    Case {
        name: "unstaged modification",
        change: |g| g.write("main", "a.ts", b"export const a = 2;\n"),
        overlay: &[("a.ts", Expect::Content("export const a = 2;\n"))],
    },
    Case {
        name: "staged and unchanged since",
        change: |g| g.add("main", &[("a.ts", b"export const a = 3;\n")]),
        overlay: &[],
    },
    Case {
        name: "StagedOnly: the worktree equals HEAD, the index does not",
        change: |g| {
            g.stage_content("main", "a.ts", b"export const a = 4;\n");
        },
        overlay: &[("a.ts", Expect::Content("export const a = 1;\n"))],
    },
    Case {
        name: "unstaged deletion",
        change: |g| g.delete("main", "b.ts"),
        overlay: &[("b.ts", Expect::Absent)],
    },
    Case {
        name: "untracked file",
        change: |g| g.write("main", "src/new.ts", b"export const n = 1;\n"),
        overlay: &[("src/new.ts", Expect::Content("export const n = 1;\n"))],
    },
    Case {
        name: "ignored untracked file",
        change: |g| {
            g.ignore("gen/");
            g.write("main", "gen/out.ts", b"export const g = 1;\n");
        },
        overlay: &[],
    },
    Case {
        name: "untracked file of no supported language",
        change: |g| g.write("main", "notes.md", b"# notes\n"),
        overlay: &[],
    },
    Case {
        name: "rename not staged",
        change: |g| {
            g.delete("main", "b.ts");
            g.write("main", "c.ts", b"export const b = 1;\n");
        },
        overlay: &[
            ("b.ts", Expect::Absent),
            ("c.ts", Expect::Content("export const b = 1;\n")),
        ],
    },
    Case {
        name: "Filtered (a clean filter's pointer)",
        change: |g| {
            g.filter("a.ts");
            g.write(
                "main",
                "a.ts",
                b"version https://git-lfs.github.com/spec/v1\n",
            );
        },
        overlay: &[("a.ts", Expect::Unindexable)],
    },
    Case {
        name: "over 1 MiB",
        change: |g| g.write("main", "big.ts", &vec![b' '; (1 << 20) + 1]),
        overlay: &[("big.ts", Expect::Unindexable)],
    },
    Case {
        name: "CRLF checkout of an LF blob",
        change: |g| {
            g.eol("a.ts");
            g.write("main", "a.ts", b"export const a = 1;\r\n");
        },
        overlay: &[],
    },
];

#[test]
fn overlay_follows_the_worktree() {
    for case in CASES {
        let h = Harness::new();
        h.git.add(
            "main",
            &[
                ("a.ts", b"export const a = 1;\n"),
                ("b.ts", b"export const b = 1;\n"),
                ("big.ts", &ts("big")),
            ],
        );
        h.git.commit("main");
        (case.change)(&h.git);
        h.open();
        let got: BTreeMap<String, WorktreeState> = h
            .map()
            .overlay
            .entries()
            .iter()
            .map(|(p, s)| (p.to_string(), *s))
            .collect();
        let want: BTreeMap<String, WorktreeState> = case
            .overlay
            .iter()
            .map(|(p, e)| {
                let state = match e {
                    Expect::Content(text) => WorktreeState::Content(h.git.oid(text.as_bytes())),
                    Expect::Absent => WorktreeState::Absent,
                    Expect::Unindexable => WorktreeState::Unindexable,
                };
                (p.to_string(), state)
            })
            .collect();
        assert_eq!(got, want, "{}", case.name);
        check_against_disk(&h);
    }
}

/// The production adapter over a real repository built with gix
/// (`lectern_git::testing::TestRepo`, no git command line): the same
/// answers `FakeGit` gives the indexer.
#[test]
fn overlay_lectern_git_answers_like_the_fake() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use lectern_git::testing::{TestHead, TestRepo};

    use crate::git_access::{GitAccess, LecternGit, Tree, blob_id, hash_blob};

    let repo = TestRepo::new();
    let a1: &[u8] = b"export const a = 1;\n";
    let b1: &[u8] = b"export const b = 1;\n";
    repo.write_file("a.ts", a1);
    repo.write_file("src/b.ts", b1);
    repo.backdate("a.ts");
    repo.backdate("src/b.ts");
    let entries = [repo.add("a.ts"), repo.add("src/b.ts")];
    repo.write_index(repo.common_dir(), &entries);
    let tree = repo.tree(&[("a.ts", a1), ("src/b.ts", b1)]);
    let commit = repo.commit(tree, &[], "init");
    repo.set_ref("refs/heads/main", commit);
    repo.set_head(TestHead::Branch("refs/heads/main"));
    // An unstaged edit and an untracked file.
    let a2: &[u8] = b"export const a = 2;\n";
    repo.write_file("a.ts", a2);
    repo.write_file("c.ts", b"export const c = 1;\n");

    let git = LecternGit::with_discover(lectern_git::testing::discover);
    let common = repo.common_dir().canonicalize().expect("canonical");
    let root = repo.root().canonicalize().expect("canonical");
    let tree = Tree {
        common_dir: Arc::from(common.as_path()),
        git_dir: Arc::from(common.as_path()),
        root: Arc::from(root.as_path()),
    };
    let cancel = AtomicBool::new(false);
    let changed = git.changed_paths(&tree, &cancel).expect("compare");
    let paths: Vec<&str> = changed.paths.iter().map(|p| &**p).collect();
    assert_eq!(paths, ["a.ts", "c.ts"]);
    let kind = repo.hash();
    let head: Vec<(&str, Option<_>)> = changed.head.iter().map(|(p, o)| (&**p, *o)).collect();
    assert_eq!(head, [("a.ts", Some(hash_blob(kind, a1))), ("c.ts", None)]);
    assert!(!changed.untracked_capped);
    assert_eq!(git.head(&tree).expect("head"), Some(blob_id(&commit)));
    assert!(git.index_checksum(&tree).expect("checksum").is_some());
    assert_eq!(
        git.canonical_oid(&tree, "a.ts", a2).expect("canonical"),
        Some(hash_blob(kind, a2))
    );
    let tracked: Vec<(String, _)> = git
        .tracked(&tree, &cancel)
        .expect("tracked")
        .into_iter()
        .map(|(p, o)| (p.to_string(), o))
        .collect();
    assert_eq!(
        tracked,
        [
            ("a.ts".to_owned(), hash_blob(kind, a1)),
            ("src/b.ts".to_owned(), hash_blob(kind, b1)),
        ]
    );
    assert_eq!(
        &*git.blob(&tree, hash_blob(kind, b1), 1 << 20).expect("blob"),
        b1
    );
}

/// Directory entries: an untracked nested repository is listed as its
/// directory, a tracked file replaced by a directory as the file and the
/// directory's files. `FakeGit` answers like lectern-git.
#[test]
fn overlay_lectern_git_and_the_fake_list_directories_alike() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use lectern_git::testing::{TestHead, TestRepo};

    use crate::git_access::{GitAccess, LecternGit, Tree};
    use crate::testing::FakeGit;

    let repo = TestRepo::new();
    let a1: &[u8] = b"export const a = 1;\n";
    repo.write_file("a.ts", a1);
    repo.write_file("foo.ts", a1);
    repo.backdate("a.ts");
    repo.backdate("foo.ts");
    let entries = [repo.add("a.ts"), repo.add("foo.ts")];
    repo.write_index(repo.common_dir(), &entries);
    let tree = repo.tree(&[("a.ts", a1), ("foo.ts", a1)]);
    let commit = repo.commit(tree, &[], "init");
    repo.set_ref("refs/heads/main", commit);
    repo.set_head(TestHead::Branch("refs/heads/main"));
    repo.write_file("vendor/lib/.git/HEAD", b"ref: refs/heads/main\n");
    repo.write_file("vendor/lib/x.ts", b"export const x = 1;\n");
    std::fs::create_dir_all(repo.root().join("vendor/lib/.git/objects")).unwrap();
    std::fs::create_dir_all(repo.root().join("vendor/lib/.git/refs")).unwrap();
    std::fs::remove_file(repo.root().join("foo.ts")).unwrap();
    repo.write_file("foo.ts/bar.ts", b"export const b = 1;\n");
    let git = LecternGit::with_discover(lectern_git::testing::discover);
    let common = repo.common_dir().canonicalize().unwrap();
    let root = repo.root().canonicalize().unwrap();
    let tree = Tree {
        common_dir: Arc::from(common.as_path()),
        git_dir: Arc::from(common.as_path()),
        root: Arc::from(root.as_path()),
    };
    let cancel = AtomicBool::new(false);
    let real = git.changed_paths(&tree, &cancel).expect("compare");

    let fake = FakeGit::with_hash(repo.hash());
    fake.add("main", &[("a.ts", a1), ("foo.ts", a1)]);
    fake.commit("main");
    fake.write("main", "vendor/lib/.git/HEAD", b"ref: refs/heads/main\n");
    fake.write("main", "vendor/lib/x.ts", b"export const x = 1;\n");
    fake.delete("main", "foo.ts");
    fake.write("main", "foo.ts/bar.ts", b"export const b = 1;\n");
    let fake_tree = Tree {
        common_dir: Arc::from(fake.common_dir()),
        git_dir: fake.worktree_key("main").0,
        root: Arc::from(fake.root("main").as_path()),
    };
    let faked = fake.changed_paths(&fake_tree, &cancel).expect("compare");

    let paths: Vec<&str> = real.paths.iter().map(|p| &**p).collect();
    assert_eq!(paths, ["foo.ts", "foo.ts/bar.ts", "vendor/lib"]);
    assert_eq!(real.paths, faked.paths);
    assert_eq!(real.head, faked.head);
}

#[test]
fn overlay_failed_comparison_keeps_the_overlay() {
    let h = Harness::new();
    h.git.add("main", &[("a.ts", b"export const a = 1;\n")]);
    h.git.commit("main");
    h.git.write("main", "a.ts", b"export const a = 2;\n");
    h.open();
    assert_eq!(h.map().overlay.len(), 1);
    h.git.fail_compare(true);
    h.git.move_head("main");
    h.emit_git(&[("HEAD", super::support::CHANGED)]);
    h.settle();
    // The comparison failed: the overlay is the previous one.
    assert_eq!(
        h.map().overlay.get("a.ts"),
        Some(WorktreeState::Content(h.git.oid(b"export const a = 2;\n")))
    );
}
