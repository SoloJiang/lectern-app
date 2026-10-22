use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::*;
use crate::testing::TestRepo;

/// The archived commits and git's patch IDs (tests/fixtures/README.md
/// 「patch_id 夹具」).
fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/patch_id")
}

/// The fixture's commits in a fresh repository: each tree rebuilt from
/// git's `ls-tree -r -z` listing (and checked against git's OID), each
/// commit made on top of its parent. Returns them by name.
fn import(t: &TestRepo) -> HashMap<String, Oid> {
    let dir = fixture();
    let commits = std::fs::read_to_string(dir.join("commits")).unwrap();
    let mut out: HashMap<String, Oid> = HashMap::new();
    for line in commits.lines() {
        let fields: Vec<&str> = line.split(' ').collect();
        let [name, tree_hex, parent] = fields[..] else {
            panic!("bad commits line {line:?}");
        };
        let listing = std::fs::read(dir.join("trees").join(tree_hex)).unwrap();
        let mut entries: Vec<(String, Mode, Oid)> = Vec::new();
        for record in listing.split(|&b| b == 0).filter(|r| !r.is_empty()) {
            let record = std::str::from_utf8(record).unwrap();
            let (meta, path) = record.split_once('\t').unwrap();
            let fields: Vec<&str> = meta.split(' ').collect();
            let [mode, kind, hex] = fields[..] else {
                panic!("bad ls-tree record {record:?}");
            };
            let mode = match mode {
                "100644" => Mode::File,
                "100755" => Mode::Executable,
                "120000" => Mode::Symlink,
                "160000" => Mode::Gitlink,
                other => panic!("mode {other}"),
            };
            let oid = Oid::from_hex(HashKind::Sha1, hex).unwrap();
            if kind == "blob" {
                let content = std::fs::read(dir.join("blobs").join(hex)).unwrap();
                assert_eq!(t.blob(&content), oid, "blob {hex}");
            }
            entries.push((path.to_owned(), mode, oid));
        }
        let entries: Vec<(&str, Mode, Oid)> = entries
            .iter()
            .map(|(p, m, o)| (p.as_str(), *m, *o))
            .collect();
        let tree = t.tree_entries(&entries);
        assert_eq!(tree.to_hex(), tree_hex, "tree of {name}");
        let parents: Vec<Oid> = match parent {
            "-" => Vec::new(),
            p => vec![out[p]],
        };
        out.insert(name.to_owned(), t.commit(tree, &parents, name));
    }
    out
}

/// git's `patch-id --stable` by fixture name (`order -O` is the same commit
/// shown in another file order); None where git printed none.
fn git_ids() -> Vec<(String, Option<String>)> {
    let text = std::fs::read_to_string(fixture().join("expected/patch-id")).unwrap();
    text.lines()
        .map(|line| {
            let (name, id) = line.rsplit_once(' ').unwrap();
            (name.to_owned(), (id != "-").then(|| id.to_owned()))
        })
        .collect()
}

fn hex(id: Option<PatchId>) -> Option<String> {
    id.map(|id| id.0.to_hex())
}

#[test]
fn patch_id_matches_git_for_every_fixture_commit() {
    let t = TestRepo::new();
    let commits = import(&t);
    let repo = t.repo();
    let mut compared = 0;
    for (name, git) in git_ids() {
        // git's ID of a binary diff depends on the abbreviated `index`
        // line it is shown; this crate hashes the full OIDs instead.
        if name == "binary" {
            continue;
        }
        let commit = name.strip_suffix(" -O").unwrap_or(&name);
        let ours = hex(repo.patch_id(commits[commit]).unwrap());
        assert_eq!(ours, git, "{name}");
        compared += 1;
    }
    assert_eq!(compared, 13);
}

#[test]
fn patch_id_of_root_and_merge_commits_is_none() {
    let t = TestRepo::new();
    let commits = import(&t);
    let repo = t.repo();
    assert_eq!(repo.patch_id(commits["root"]).unwrap(), None);
    let merge_tree = repo.commit(commits["modify"]).unwrap().tree;
    let merge = t.commit(merge_tree, &[commits["modify"], commits["slide"]], "merge");
    assert_eq!(repo.patch_id(merge).unwrap(), None);
    // A commit that changes nothing has no patch either (git prints none).
    assert_eq!(repo.patch_id(commits["empty"]).unwrap(), None);
}

#[test]
fn patch_id_ignores_whitespace_as_git_does() {
    let t = TestRepo::new();
    let commits = import(&t);
    let repo = t.repo();
    // `call(a,b)` and `call( a ,\tb )  ` from the same `call(a, b)`.
    let a = repo.patch_id(commits["ws-a"]).unwrap();
    let b = repo.patch_id(commits["ws-b"]).unwrap();
    assert!(a.is_some());
    assert_eq!(a, b);
    let git: HashMap<String, Option<String>> = git_ids().into_iter().collect();
    assert_eq!(hex(a), git["ws-a"]);
    assert_eq!(git["ws-a"], git["ws-b"]);
}

#[test]
fn patch_id_does_not_depend_on_file_order() {
    let t = TestRepo::new();
    let commits = import(&t);
    let ours = hex(t.repo().patch_id(commits["order"]).unwrap());
    // git gives the same ID for the diff listed in another order (`-O`).
    let git: HashMap<String, Option<String>> = git_ids().into_iter().collect();
    assert_eq!(ours, git["order"]);
    assert_eq!(ours, git["order -O"]);
    // The sum of the file hashes is the same in every order.
    let digests: Vec<Vec<u8>> = (1u8..=5).map(|i| vec![i.wrapping_mul(97); 20]).collect();
    let total = |order: &[usize]| {
        let mut sum = vec![0u8; 20];
        for &i in order {
            add(&mut sum, &digests[i]);
        }
        sum
    };
    assert_eq!(total(&[0, 1, 2, 3, 4]), total(&[4, 2, 0, 3, 1]));
    assert_eq!(total(&[0, 1, 2, 3, 4]), total(&[1, 0, 4, 3, 2]));
}

#[test]
fn patch_id_sum_carries_little_endian() {
    let mut sum = vec![0xff, 0xff, 0x01];
    add(&mut sum, &[0x01, 0x00, 0x00]);
    assert_eq!(sum, [0x00, 0x00, 0x02]);
    // The carry out of the last byte is dropped.
    let mut sum = vec![0xff, 0xff, 0xff];
    add(&mut sum, &[0x01, 0x00, 0x00]);
    assert_eq!(sum, [0, 0, 0]);
}

/// A commit changing `files` on top of a parent with `base`.
fn change(t: &TestRepo, base: &[(&str, &[u8])], files: &[(&str, &[u8])]) -> Oid {
    let parent = t.commit(t.tree(base), &[], "base");
    t.commit(t.tree(files), &[parent], "change")
}

#[test]
fn patch_id_of_binary_files_hashes_their_oids() {
    let t = TestRepo::new();
    let repo = t.repo();
    let id = |base: &[(&str, &[u8])], files: &[(&str, &[u8])]| {
        repo.patch_id(change(&t, base, files)).unwrap().unwrap()
    };
    let one = id(
        &[("b.bin", b"x\0old"), ("other", b"1\n")],
        &[("b.bin", b"x\0new"), ("other", b"1\n")],
    );
    // The same change on another parent (a rebased commit).
    let rebased = id(
        &[("b.bin", b"x\0old"), ("other", b"2\n")],
        &[("b.bin", b"x\0new"), ("other", b"2\n")],
    );
    assert_eq!(one, rebased);
    // Another binary change on the same path.
    let other = id(&[("b.bin", b"x\0old")], &[("b.bin", b"x\0newer")]);
    assert_ne!(one, other);
    // `-diff` makes a text file binary: no longer the text diff's ID.
    let text = id(&[("f.txt", b"a\n")], &[("f.txt", b"b\n")]);
    let attributed = id(
        &[("f.txt", b"a\n"), (".gitattributes", b"*.txt -diff\n")],
        &[("f.txt", b"b\n"), (".gitattributes", b"*.txt -diff\n")],
    );
    assert_ne!(text, attributed);
}

#[test]
fn patch_id_limits_fail_instead_of_hashing_part() {
    let t = TestRepo::new();
    let repo = t.repo();
    let commit = change(
        &t,
        &[("a", b"1\n"), ("b", b"2\n")],
        &[("a", b"one\n"), ("b", b"two\n")],
    );
    let limits = |files, file_bytes, total_bytes| PatchLimits {
        files,
        file_bytes,
        total_bytes,
    };
    assert!(
        repo.patch_id_within(commit, &limits(2, 4, 12))
            .unwrap()
            .is_some()
    );
    assert!(matches!(
        repo.patch_id_within(commit, &limits(1, 4, 8)),
        Err(GitError::TooManyEntries)
    ));
    // "one\n" is 4 bytes.
    assert!(matches!(
        repo.patch_id_within(commit, &limits(2, 3, 8)),
        Err(GitError::TooLarge { size: 4 })
    ));
    // 2 + 4 + 2 + 4 bytes read in all.
    assert!(matches!(
        repo.patch_id_within(commit, &limits(2, 4, 11)),
        Err(GitError::TooLarge { size: 12 })
    ));
    assert_eq!(
        repo.patch_id_within(commit, &limits(2, 4, 12)).unwrap(),
        repo.patch_id(commit).unwrap()
    );
}

#[test]
fn patch_id_of_a_sha256_repository_is_a_sha256_oid() {
    let t = TestRepo::sha256();
    let commit = change(&t, &[("a", b"1\n")], &[("a", b"2\n")]);
    let id = t.repo().patch_id(commit).unwrap().unwrap();
    assert_eq!(id.0.kind(), HashKind::Sha256);
    assert_eq!(id.0.to_hex().len(), 64);
}

#[test]
fn patch_id_with_a_missing_parent_fails() {
    let t = TestRepo::new();
    let absent = Oid::from_hex(HashKind::Sha1, &"5".repeat(40)).unwrap();
    let commit = t.commit(t.tree(&[("a", b"1\n")]), &[absent], "orphan");
    assert!(matches!(
        t.repo().patch_id(commit),
        Err(GitError::ObjectMissing(oid)) if oid == absent
    ));
}

#[test]
fn patch_id_quotes_paths_as_git_does() {
    assert_eq!(quoted(b"a/", b"plain/p.txt"), b"a/plain/p.txt");
    assert_eq!(quoted(b"a/", b"sp ace"), b"a/sp ace");
    assert_eq!(quoted(b"b/", "é".as_bytes()), b"\"b/\\303\\251\"");
    assert_eq!(quoted(b"a/", b"t\tq\"b\\"), b"\"a/t\\tq\\\"b\\\\\"");
    assert_eq!(
        quoted(b"a/", b"\x07\x08\n\x0b\x0c\r\x01\x7f"),
        b"\"a/\\a\\b\\n\\v\\f\\r\\001\\177\""
    );
}
