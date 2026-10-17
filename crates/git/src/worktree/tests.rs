use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use crate::testing::{IndexEntry, IndexOptions, TestHead, TestRepo};
use crate::*;

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

fn main_worktree(t: &TestRepo) -> Worktree {
    t.repo().worktree(&WorktreeId::Main).unwrap()
}

fn paths(tracked: &TrackedPaths) -> Vec<String> {
    tracked
        .iter()
        .map(|e| String::from_utf8(e.path.as_bytes().to_vec()).unwrap())
        .collect()
}

fn file(t: &TestRepo, path: &str, content: &[u8]) -> IndexEntry {
    IndexEntry::new(path, Mode::File, t.blob(content))
}

// ---- tracked_* ------------------------------------------------------------

#[test]
fn tracked_without_an_index_is_empty() {
    let t = TestRepo::new();
    let wt = main_worktree(&t);
    let tracked = wt.tracked(&EntryLimits::default(), &no_cancel()).unwrap();
    assert!(tracked.is_empty());
    assert_eq!(tracked.checksum(), None);
    assert_eq!(wt.index_checksum().unwrap(), None);
}

#[test]
fn tracked_lists_paths_in_index_order_once_per_conflict() {
    let t = TestRepo::new();
    let ours = t.blob(b"ours");
    let mut entries = vec![
        file(&t, "b", b"b"),
        file(&t, "a/x", b"x"),
        file(&t, "a.txt", b"a"),
        IndexEntry::new("bin/run", Mode::Executable, t.blob(b"#!")),
        IndexEntry::new("link", Mode::Symlink, t.blob(b"target")),
        IndexEntry::new("vendor/lib", Mode::Gitlink, t.blob(b"not really a commit")),
    ];
    for (stage, oid) in [(1, t.blob(b"base")), (2, ours), (3, t.blob(b"theirs"))] {
        entries.push(IndexEntry {
            stage,
            ..IndexEntry::new("c", Mode::File, oid)
        });
    }
    t.write_index(t.common_dir(), &entries);
    let tracked = main_worktree(&t)
        .tracked(&EntryLimits::default(), &no_cancel())
        .unwrap();
    assert_eq!(
        paths(&tracked),
        ["a.txt", "a/x", "b", "bin/run", "c", "link", "vendor/lib"]
    );
    let get = |p: &str| {
        tracked
            .iter()
            .find(|e| e.path.as_bytes() == p.as_bytes())
            .unwrap()
            .clone()
    };
    assert_eq!(get("c").oid, ours, "the conflict is listed with our side");
    assert_eq!(get("bin/run").mode, Mode::Executable);
    assert_eq!(get("link").mode, Mode::Symlink);
    assert_eq!(get("vendor/lib").mode, Mode::Gitlink);
    assert!(tracked.iter().all(|e| !e.skip_worktree));
    assert!(tracked.contains(b"a/x"));
    assert!(!tracked.contains(b"a"));
    assert!(tracked.contains_dir(b"a"));
    assert!(tracked.contains_dir(b"bin"));
    assert!(!tracked.contains_dir(b"a.txt"));
    assert!(!tracked.contains_dir(b"b"));
    assert!(!tracked.contains_dir(b"zzz"));
}

#[test]
fn tracked_returns_the_same_arc_while_the_index_is_unchanged() {
    let t = TestRepo::new();
    t.write_index(t.common_dir(), &[file(&t, "a", b"a")]);
    let wt = main_worktree(&t);
    let limits = EntryLimits::default();
    let first = wt.tracked(&limits, &no_cancel()).unwrap();
    let again = wt.tracked(&limits, &no_cancel()).unwrap();
    assert!(Arc::ptr_eq(&first, &again));
    t.write_index(t.common_dir(), &[file(&t, "a", b"a"), file(&t, "b", b"b")]);
    let changed = wt.tracked(&limits, &no_cancel()).unwrap();
    assert!(!Arc::ptr_eq(&first, &changed));
    assert_eq!(changed.len(), 2);
    assert_ne!(first.checksum(), changed.checksum());
    // A cached list above a smaller limit is still refused.
    assert!(matches!(
        wt.tracked(&EntryLimits { entries: 1 }, &no_cancel()),
        Err(GitError::TooManyEntries)
    ));
}

#[test]
fn tracked_checksum_is_the_index_trailer() {
    let t = TestRepo::new();
    t.write_index(t.common_dir(), &[file(&t, "a", b"a")]);
    let bytes = std::fs::read(t.common_dir().join("index")).unwrap();
    let trailer = Oid::from_hex(
        HashKind::Sha1,
        &bytes[bytes.len() - 20..]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
    )
    .unwrap();
    let wt = main_worktree(&t);
    assert_eq!(wt.index_checksum().unwrap(), Some(trailer));
    assert_eq!(
        wt.tracked(&EntryLimits::default(), &no_cancel())
            .unwrap()
            .checksum(),
        Some(trailer)
    );
}

#[test]
fn tracked_skip_hash_index_gets_a_content_checksum() {
    let t = TestRepo::new();
    let skip = IndexOptions {
        skip_hash: true,
        ..Default::default()
    };
    t.write_index_with(t.common_dir(), &[file(&t, "a", b"a")], &skip);
    let bytes = std::fs::read(t.common_dir().join("index")).unwrap();
    assert!(
        bytes[bytes.len() - 20..].iter().all(|&b| b == 0),
        "index.skipHash writes a null trailer"
    );
    let wt = main_worktree(&t);
    let first = wt.index_checksum().unwrap().unwrap();
    let mut hasher = gix::hash::hasher(gix::hash::Kind::Sha1);
    hasher.update(&bytes[..bytes.len() - 20]);
    assert_eq!(first, Oid::from_gix(&hasher.try_finalize().unwrap()));
    assert_eq!(
        wt.tracked(&EntryLimits::default(), &no_cancel())
            .unwrap()
            .checksum(),
        Some(first)
    );
    t.write_index_with(
        t.common_dir(),
        &[file(&t, "a", b"a"), file(&t, "b", b"b")],
        &skip,
    );
    assert_ne!(wt.index_checksum().unwrap(), Some(first));
}

#[test]
fn tracked_reports_skip_worktree_entries() {
    let t = TestRepo::new();
    let hidden = IndexEntry {
        skip_worktree: true,
        ..file(&t, "hidden", b"h")
    };
    t.write_index(t.common_dir(), &[file(&t, "shown", b"s"), hidden]);
    let tracked = main_worktree(&t)
        .tracked(&EntryLimits::default(), &no_cancel())
        .unwrap();
    let flags: Vec<(String, bool)> = tracked
        .iter()
        .map(|e| {
            (
                String::from_utf8_lossy(e.path.as_bytes()).into_owned(),
                e.skip_worktree,
            )
        })
        .collect();
    assert_eq!(
        flags,
        [("hidden".to_owned(), true), ("shown".to_owned(), false)]
    );
}

#[test]
fn tracked_sets_invalid_paths_aside() {
    let t = TestRepo::new();
    let blob = t.blob(b"x");
    let entries: Vec<IndexEntry> = [&b"../evil"[..], b"/abs", b"a//b", b"ok", b"x/./y", b"nul\0"]
        .iter()
        .map(|p| IndexEntry {
            path: p.to_vec(),
            ..IndexEntry::new("placeholder", Mode::File, blob)
        })
        .collect();
    t.write_index(t.common_dir(), &entries);
    let tracked = main_worktree(&t)
        .tracked(&EntryLimits::default(), &no_cancel())
        .unwrap();
    assert_eq!(paths(&tracked), ["ok"]);
    let mut invalid: Vec<&[u8]> = tracked.invalid_paths().iter().map(|p| &p[..]).collect();
    invalid.sort();
    assert_eq!(
        invalid,
        [&b"../evil"[..], b"/abs", b"a//b", b"nul\0", b"x/./y"]
    );
}

/// An index with `README`, `src/main.rs` and the sparse directory `lib`
/// (`lib/a`, `lib/deep/b`, `lib/deep/c`).
fn sparse_repo() -> (TestRepo, Oid) {
    let t = TestRepo::new();
    let lib = t.tree(&[("a", b"a"), ("deep/b", b"b"), ("deep/c", b"c")]);
    let options = IndexOptions {
        sparse_dirs: &[("lib", lib)],
        ..Default::default()
    };
    t.write_index_with(
        t.common_dir(),
        &[file(&t, "README", b"r"), file(&t, "src/main.rs", b"m")],
        &options,
    );
    (t, lib)
}

#[test]
fn tracked_expands_sparse_directory_entries_in_memory() {
    let (t, _) = sparse_repo();
    let index = t.common_dir().join("index");
    let before = std::fs::read(&index).unwrap();
    assert!(
        before.windows(4).any(|w| w == b"sdir"),
        "the fixture is a sparse index"
    );
    let tracked = main_worktree(&t)
        .tracked(&EntryLimits::default(), &no_cancel())
        .unwrap();
    assert_eq!(
        paths(&tracked),
        ["README", "lib/a", "lib/deep/b", "lib/deep/c", "src/main.rs"]
    );
    let skip: Vec<bool> = tracked.iter().map(|e| e.skip_worktree).collect();
    assert_eq!(skip, [false, true, true, true, false]);
    let b = tracked
        .iter()
        .find(|e| e.path.as_bytes() == b"lib/deep/b")
        .unwrap();
    assert_eq!(b.oid, Oid::for_blob(HashKind::Sha1, b"b"));
    assert!(tracked.contains_dir(b"lib/deep"));
    // Nothing is written back: the index stays sparse.
    assert_eq!(std::fs::read(&index).unwrap(), before);
}

#[test]
fn tracked_sparse_expansion_respects_limits() {
    let (t, _) = sparse_repo();
    let wt = main_worktree(&t);
    assert!(matches!(
        wt.tracked(&EntryLimits { entries: 4 }, &no_cancel()),
        Err(GitError::TooManyEntries)
    ));
    assert_eq!(
        wt.tracked(&EntryLimits { entries: 5 }, &no_cancel())
            .unwrap()
            .len(),
        5
    );
}

#[test]
fn tracked_sparse_expansion_is_cancellable() {
    let (t, _) = sparse_repo();
    let wt = main_worktree(&t);
    assert!(matches!(
        wt.tracked(&EntryLimits::default(), &AtomicBool::new(true)),
        Err(GitError::Cancelled)
    ));
    // Nothing was cached: the next call succeeds.
    assert_eq!(
        wt.tracked(&EntryLimits::default(), &no_cancel())
            .unwrap()
            .len(),
        5
    );
}

#[test]
fn tracked_sparse_directory_with_a_missing_tree_is_object_missing() {
    let t = TestRepo::new();
    let missing = Oid::from_hex(HashKind::Sha1, &"3".repeat(40)).unwrap();
    let options = IndexOptions {
        sparse_dirs: &[("lib", missing)],
        ..Default::default()
    };
    t.write_index_with(t.common_dir(), &[file(&t, "README", b"r")], &options);
    let result = main_worktree(&t).tracked(&EntryLimits::default(), &no_cancel());
    assert!(matches!(result, Err(GitError::ObjectMissing(id)) if id == missing));
}

#[test]
fn tracked_index_checksum_reads_only_the_trailer() {
    use crate::handle::hook as handles;
    use crate::tracked::hook as index;
    use crate::tree::hook as trees;

    let (t, _) = sparse_repo();
    let wt = main_worktree(&t);
    let (parsed, expanded, opened) = (
        index::parsed_count(),
        trees::expanded_count(),
        handles::opened_count(),
    );
    let checksum = wt.index_checksum().unwrap();
    assert!(checksum.is_some());
    assert_eq!(index::parsed_count(), parsed, "no entry parsed");
    assert_eq!(
        trees::expanded_count(),
        expanded,
        "no sparse directory expanded"
    );
    assert_eq!(handles::opened_count(), opened, "no gix handle opened");
    let tracked = wt.tracked(&EntryLimits::default(), &no_cancel()).unwrap();
    assert_eq!(tracked.checksum(), checksum);
    assert_eq!(index::parsed_count(), parsed + 1);
    assert_eq!(trees::expanded_count(), expanded + 1);
    assert_eq!(handles::live_count(), 0);
}

#[test]
fn tracked_sha256_index() {
    let t = TestRepo::sha256();
    t.write_index(t.common_dir(), &[file(&t, "a", b"a")]);
    let wt = main_worktree(&t);
    let tracked = wt.tracked(&EntryLimits::default(), &no_cancel()).unwrap();
    assert_eq!(
        tracked.iter().next().unwrap().oid,
        Oid::for_blob(HashKind::Sha256, b"a")
    );
    let checksum = tracked.checksum().unwrap();
    assert_eq!(checksum.kind(), HashKind::Sha256);
    assert_eq!(wt.index_checksum().unwrap(), Some(checksum));
}

#[test]
fn tracked_linked_worktree_reads_its_own_index() {
    let t = TestRepo::new();
    t.write_index(t.common_dir(), &[file(&t, "main-only", b"m")]);
    let admin = t.add_worktree(
        "feature",
        &t.base().join("feature"),
        TestHead::Branch("refs/heads/feature"),
    );
    t.write_index(&admin, &[file(&t, "feature-only", b"f")]);
    let wt = t
        .repo()
        .worktree(&WorktreeId::Linked("feature".into()))
        .unwrap();
    let tracked = wt.tracked(&EntryLimits::default(), &no_cancel()).unwrap();
    assert_eq!(paths(&tracked), ["feature-only"]);
}

// ---- read_file_* ----------------------------------------------------------

fn rp(path: &str) -> RepoPath {
    RepoPath::new(path.as_bytes()).unwrap()
}

#[test]
fn read_file_reads_files_without_conversion() {
    let t = TestRepo::new();
    t.write_file("pkg/package.json", b"{ \"name\": \"x\" }\r\n");
    let wt = main_worktree(&t);
    assert_eq!(
        wt.read_file(&rp("pkg/package.json"), 1024).unwrap(),
        WorktreeFile::File {
            bytes: b"{ \"name\": \"x\" }\r\n".to_vec()
        }
    );
}

#[test]
fn read_file_respects_the_limit() {
    let t = TestRepo::new();
    t.write_file("big", &[b'x'; 100]);
    let wt = main_worktree(&t);
    assert_eq!(
        wt.read_file(&rp("big"), 99).unwrap(),
        WorktreeFile::TooLarge { size: 100 }
    );
    assert!(matches!(
        wt.read_file(&rp("big"), 100).unwrap(),
        WorktreeFile::File { .. }
    ));
}

#[test]
fn read_file_missing_is_not_found() {
    let t = TestRepo::new();
    t.write_file("file", b"");
    let wt = main_worktree(&t);
    assert_eq!(
        wt.read_file(&rp("nope"), 10).unwrap(),
        WorktreeFile::NotFound
    );
    assert_eq!(
        wt.read_file(&rp("dir/nope"), 10).unwrap(),
        WorktreeFile::NotFound
    );
    // A path through a regular file.
    assert_eq!(
        wt.read_file(&rp("file/x"), 10).unwrap(),
        WorktreeFile::NotFound
    );
}

#[test]
fn read_file_directory_is_unsupported() {
    let t = TestRepo::new();
    t.write_file("dir/x", b"");
    let wt = main_worktree(&t);
    assert_eq!(
        wt.read_file(&rp("dir"), 10).unwrap(),
        WorktreeFile::Unavailable(Unavailable::UnsupportedType)
    );
}

#[cfg(unix)]
#[test]
fn read_file_never_follows_symlinks() {
    let t = TestRepo::new();
    t.write_file("real/secret", b"inside");
    t.symlink("link", "real");
    t.symlink("passwd", "/etc/passwd");
    let wt = main_worktree(&t);
    assert_eq!(
        wt.read_file(&rp("link/secret"), 100).unwrap(),
        WorktreeFile::Unavailable(Unavailable::PathThroughSymlink)
    );
    assert_eq!(
        wt.read_file(&rp("link"), 100).unwrap(),
        WorktreeFile::Symlink {
            target: b"real".to_vec()
        }
    );
    assert_eq!(
        wt.read_file(&rp("passwd"), 100).unwrap(),
        WorktreeFile::Symlink {
            target: b"/etc/passwd".to_vec()
        }
    );
}

#[cfg(unix)]
#[test]
fn read_file_unreadable_is_unavailable() {
    use std::os::unix::fs::PermissionsExt;

    let t = TestRepo::new();
    t.write_file("locked", b"x");
    let path = t.root().join("locked");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    let wt = main_worktree(&t);
    let result = wt.read_file(&rp("locked"), 10).unwrap();
    // root can read anything; everyone else gets Unreadable.
    if std::fs::read(&path).is_err() {
        assert_eq!(result, WorktreeFile::Unavailable(Unavailable::Unreadable));
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn read_file_in_a_linked_worktree_stays_in_its_root() {
    let t = TestRepo::new();
    t.write_file("only-in-main", b"m");
    let root = t.base().join("feature");
    t.add_worktree("feature", &root, TestHead::Branch("refs/heads/feature"));
    std::fs::write(root.join("only-in-feature"), b"f").unwrap();
    let wt = t
        .repo()
        .worktree(&WorktreeId::Linked("feature".into()))
        .unwrap();
    assert_eq!(
        wt.read_file(&rp("only-in-main"), 10).unwrap(),
        WorktreeFile::NotFound
    );
    assert_eq!(
        wt.read_file(&rp("only-in-feature"), 10).unwrap(),
        WorktreeFile::File {
            bytes: b"f".to_vec()
        }
    );
}

// ---- timing ---------------------------------------------------------------

/// `discover` + `worktrees` (11 worktrees) + `tracked` (728 files) on the
/// `cloudprint-miniapps` shape (docs/design/git.md「性能」, budget < 15 ms).
/// Prints the median and the slowest run. The `git_open` bench in
/// `crates/bench` replaces it once the bench design (#7) is merged.
/// Run with `cargo test --release -p lectern-git -- --ignored perf_smoke`.
#[test]
#[ignore = "timing; run in release"]
fn perf_smoke_git_open() {
    let t = TestRepo::new();
    let blob = t.blob(b"export const x = 1;\n");
    let entries: Vec<IndexEntry> = (0..728)
        .map(|i| {
            IndexEntry::new(
                &format!("packages/p{:02}/src/file{i:03}.ts", i % 25),
                Mode::File,
                blob,
            )
        })
        .collect();
    t.write_index(t.common_dir(), &entries);
    let tree = t.tree(&[("README.md", b"x")]);
    let commit = t.commit(tree, &[], "c");
    t.set_ref("refs/heads/main", commit);
    for i in 0..10 {
        let branch = format!("refs/heads/wt{i}");
        t.set_ref(&branch, commit);
        let admin = t.add_worktree(
            &format!("wt{i}"),
            &t.base().join(format!("wt{i}")),
            TestHead::Branch(&branch),
        );
        t.write_index(&admin, &entries);
    }
    let cancel = no_cancel();
    let mut runs = Vec::new();
    for _ in 0..200 {
        let start = Instant::now();
        let d = crate::testing::discover(t.root(), &[]).unwrap().unwrap();
        let worktrees = d.repo.worktrees().unwrap();
        assert_eq!(worktrees.len(), 11);
        let wt = d.repo.worktree(&WorktreeId::Main).unwrap();
        let tracked = wt.tracked(&EntryLimits::default(), &cancel).unwrap();
        assert_eq!(tracked.len(), 728);
        runs.push(start.elapsed());
    }
    runs.sort();
    println!(
        "git_open (discover + worktrees x11 + tracked x728): median {:?}, p95 {:?}, max {:?}",
        runs[runs.len() / 2],
        runs[runs.len() * 95 / 100],
        runs[runs.len() - 1]
    );
    // One gix handle on the common directory (budget < 1 ms per top-level call).
    let mut opens = Vec::new();
    for _ in 0..200 {
        let start = Instant::now();
        let handle =
            crate::handle::Handle::open(t.common_dir(), crate::handle::ConfigScope::RepositoryOnly)
                .unwrap();
        drop(handle);
        opens.push(start.elapsed());
    }
    opens.sort();
    println!(
        "git_open_handle: median {:?}, max {:?}",
        opens[opens.len() / 2],
        opens[opens.len() - 1]
    );
}
