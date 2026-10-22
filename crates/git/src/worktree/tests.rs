use std::sync::Arc;
use std::sync::atomic::AtomicBool;

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

// ---- head_reflog_* ----------------------------------------------------------

/// The archived reflog git wrote (tests/fixtures/README.md「reflog 夹具」).
fn reflog_fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reflog")
}

fn fixture_reflog() -> Vec<u8> {
    std::fs::read(reflog_fixture().join("logs/HEAD")).unwrap()
}

/// git's own reading of the fixture, newest first: (seconds, new OID, message).
fn git_entries() -> Vec<(i64, String, String)> {
    let text = std::fs::read_to_string(reflog_fixture().join("expected/entries")).unwrap();
    text.lines()
        .map(|line| {
            let mut parts = line.splitn(3, ' ');
            let seconds = parts.next().unwrap().parse().unwrap();
            let new = parts.next().unwrap().to_owned();
            (seconds, new, parts.next().unwrap().to_owned())
        })
        .collect()
}

fn summary(entries: &[ReflogEntry]) -> Vec<(i64, String, String)> {
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

fn write_reflog(git_dir: &std::path::Path, bytes: &[u8]) {
    std::fs::create_dir_all(git_dir.join("logs")).unwrap();
    std::fs::write(git_dir.join("logs/HEAD"), bytes).unwrap();
}

/// The fixture's reflog in a fresh repository's main worktree.
fn with_reflog(bytes: &[u8]) -> (TestRepo, Worktree) {
    let t = TestRepo::new();
    write_reflog(t.common_dir(), bytes);
    let wt = main_worktree(&t);
    (t, wt)
}

/// The fixture's first record: 1700000000; the others 100 s apart, with
/// one at 1700000450.
const T0: i64 = 1_700_000_000;

#[test]
fn head_reflog_reads_the_fixture_newest_first_as_git_does() {
    let (_t, wt) = with_reflog(&fixture_reflog());
    let log = wt.head_reflog(i64::MIN, 256).unwrap();
    assert!(log.exists && log.complete);
    assert_eq!(summary(&log.entries), git_entries());
    assert_eq!(log.entries.len(), 9);
    // `checkout -b` moves nothing: old = new.
    let branch = &log.entries[6];
    assert_eq!(&*branch.message, b"checkout: moving from main to feature");
    assert_eq!(branch.old, branch.new);
    assert!(matches!(branch.action, ReflogAction::Checkout { .. }));
    assert_eq!(log.entries[0].time.offset, 3600);
}

#[test]
fn head_reflog_all_zero_old_value_is_none() {
    let (_t, wt) = with_reflog(&fixture_reflog());
    let log = wt.head_reflog(i64::MIN, 256).unwrap();
    let first = log.entries.last().unwrap();
    assert_eq!(first.old, None);
    assert!(first.new.is_some());
    assert_eq!(
        first.action,
        ReflogAction::Commit {
            amend: false,
            initial: true
        }
    );
    assert!(log.entries[..8].iter().all(|e| e.old.is_some()));
}

#[test]
fn head_reflog_stop_equal_to_a_record_time_reads_one_older() {
    let (_t, wt) = with_reflog(&fixture_reflog());
    // The record at `stop` is not older than it: the reading goes on to
    // the one before (200), which is, and stops there.
    let log = wt.head_reflog(T0 + 300, 256).unwrap();
    let times: Vec<i64> = log.entries.iter().map(|e| e.time.seconds - T0).collect();
    assert_eq!(times, [700, 600, 500, 450, 400, 300, 200]);
    assert!(log.exists && !log.complete);
    // `stop` at the first record's time: nothing is older, the whole file.
    let log = wt.head_reflog(T0, 256).unwrap();
    assert_eq!(log.entries.len(), 9);
    assert!(log.complete);
    // The first record older than `stop` is the file's first line: it is
    // included, and the reading did reach the start.
    let log = wt.head_reflog(T0 + 1, 256).unwrap();
    assert_eq!(summary(&log.entries), git_entries());
    assert!(log.complete);
}

#[test]
fn head_reflog_every_record_after_stop_is_the_whole_file() {
    let (_t, wt) = with_reflog(&fixture_reflog());
    let log = wt.head_reflog(T0 - 14 * 24 * 3600, 256).unwrap();
    assert_eq!(summary(&log.entries), git_entries());
    assert!(log.exists && log.complete);
}

#[test]
fn head_reflog_without_a_reflog_file() {
    let t = TestRepo::new();
    let log = main_worktree(&t).head_reflog(i64::MIN, 256).unwrap();
    assert_eq!(
        log,
        HeadReflog {
            exists: false,
            entries: Vec::new(),
            complete: true
        }
    );
    // An empty file exists and has nothing.
    let (_t, wt) = with_reflog(b"");
    let log = wt.head_reflog(i64::MIN, 256).unwrap();
    assert!(log.exists && log.complete && log.entries.is_empty());
}

#[test]
fn head_reflog_stops_at_max_entries() {
    let (_t, wt) = with_reflog(&fixture_reflog());
    let log = wt.head_reflog(i64::MIN, 3).unwrap();
    assert_eq!(summary(&log.entries), git_entries()[..3]);
    assert!(!log.complete);
    // The ninth record is the first line: the file was read to its start.
    let log = wt.head_reflog(i64::MIN, 9).unwrap();
    assert_eq!(log.entries.len(), 9);
    assert!(log.complete);
    let log = wt.head_reflog(i64::MIN, 0).unwrap();
    assert!(log.entries.is_empty() && !log.complete);
}

#[test]
fn head_reflog_reads_at_most_4_mib_from_the_end() {
    let a = "1".repeat(40);
    let b = "2".repeat(40);
    let mut bytes = Vec::new();
    let mut starts = Vec::new();
    let mut i: i64 = 0;
    while bytes.len() < 5 * 1024 * 1024 {
        starts.push(bytes.len() as u64);
        bytes.extend_from_slice(
            format!(
                "{a} {b} Fixture <fixture@example.com> {} +0100\tcommit: {i:07}\n",
                T0 + i
            )
            .as_bytes(),
        );
        i += 1;
    }
    let floor = bytes.len() as u64 - crate::reflog::HEAD_REFLOG_MAX_BYTES;
    // The line at the bound may be the end of a longer one: only lines
    // starting after it count.
    let kept = starts.iter().filter(|&&s| s > floor).count();
    let (_t, wt) = with_reflog(&bytes);
    let log = wt.head_reflog(i64::MIN, usize::MAX).unwrap();
    assert!(log.exists && !log.complete);
    assert_eq!(log.entries.len(), kept);
    assert_eq!(log.entries[0].time.seconds, T0 + i - 1);
    let oldest = starts.iter().position(|&s| s > floor).unwrap() as i64;
    assert_eq!(log.entries.last().unwrap().time.seconds, T0 + oldest);
}

#[test]
fn head_reflog_ignores_an_incomplete_last_line() {
    let whole = fixture_reflog();
    let last = whole[..whole.len() - 1]
        .iter()
        .rposition(|&b| b == b'\n')
        .unwrap();
    // A record being appended: all of it but the `\n`, or only its start.
    for partial in [
        &whole[last + 1..whole.len() - 1],
        &whole[last + 1..last + 30],
    ] {
        let mut bytes = whole.clone();
        bytes.extend_from_slice(partial);
        let (_t, wt) = with_reflog(&bytes);
        let log = wt.head_reflog(i64::MIN, 256).unwrap();
        assert_eq!(summary(&log.entries), git_entries());
        assert!(log.complete);
    }
    // Only an incomplete line.
    let (_t, wt) = with_reflog(&whole[last + 1..whole.len() - 1]);
    let log = wt.head_reflog(i64::MIN, 256).unwrap();
    assert!(log.entries.is_empty() && log.complete);
}

#[test]
fn head_reflog_skips_lines_that_do_not_parse() {
    let whole = fixture_reflog();
    let lines: Vec<&[u8]> = whole.split_inclusive(|&b| b == b'\n').collect();
    let mut bytes = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if i == 4 {
            bytes.extend_from_slice(b"not a reflog line\n\n");
        }
        bytes.extend_from_slice(line);
    }
    bytes.splice(0..0, b"garbage first\n".iter().copied());
    let (_t, wt) = with_reflog(&bytes);
    let log = wt.head_reflog(i64::MIN, 256).unwrap();
    assert_eq!(summary(&log.entries), git_entries());
    assert!(log.complete);
}

#[test]
fn head_reflog_same_result_for_every_chunk_size() {
    use crate::reflog::{Tail, read_tail};

    let mut bytes = fixture_reflog();
    bytes.extend_from_slice(b"garbage\n0000 partial");
    let len = bytes.len() as u64;
    for (stop, max_entries, max_bytes) in [
        (i64::MIN, 256, u64::MAX),
        (T0 + 300, 256, u64::MAX),
        (i64::MIN, 4, u64::MAX),
        (i64::MIN, 256, 600),
    ] {
        let read = |chunk| {
            let tail = Tail {
                stop,
                max_entries,
                max_bytes,
                chunk,
            };
            read_tail(&mut std::io::Cursor::new(&bytes), len, HashKind::Sha1, tail).unwrap()
        };
        let expected = read(64 * 1024);
        for chunk in 1..=300 {
            assert_eq!(read(chunk), expected, "chunk {chunk}, stop {stop}");
        }
    }
}

#[test]
fn head_reflog_of_a_linked_worktree_is_its_own() {
    let t = TestRepo::new();
    let root = t.base().join("feature");
    let admin = t.add_worktree("feature", &root, TestHead::Branch("refs/heads/feature"));
    write_reflog(&admin, &fixture_reflog());
    let linked = t
        .repo()
        .worktree(&WorktreeId::Linked("feature".into()))
        .unwrap();
    let log = linked.head_reflog(i64::MIN, 256).unwrap();
    assert_eq!(summary(&log.entries), git_entries());
    assert!(!main_worktree(&t).head_reflog(i64::MIN, 256).unwrap().exists);
}

// ---- mtimes_* ----------------------------------------------------------------

fn nanos(path: &std::path::Path) -> i64 {
    let modified = std::fs::symlink_metadata(path).unwrap().modified().unwrap();
    let since = modified.duration_since(std::time::UNIX_EPOCH).unwrap();
    i64::try_from(since.as_nanos()).unwrap()
}

#[cfg(unix)]
#[test]
fn mtimes_of_files_and_links_without_following_them() {
    let t = TestRepo::new();
    t.write_file("f.txt", b"x");
    let at = std::time::UNIX_EPOCH + std::time::Duration::new(1_700_000_000, 123_456_789);
    t.set_mtime("f.txt", at);
    t.write_file("real/secret", b"inside");
    t.symlink("link", "real/secret");
    t.symlink("dangling", "nowhere");
    let wt = main_worktree(&t);
    let got = wt
        .mtimes(&[rp("f.txt"), rp("link"), rp("dangling"), rp("real/secret")])
        .unwrap();
    assert_eq!(
        got,
        [
            Some(1_700_000_000_123_456_789),
            Some(nanos(&t.root().join("link"))),
            Some(nanos(&t.root().join("dangling"))),
            Some(nanos(&t.root().join("real/secret"))),
        ]
    );
    assert!(wt.mtimes(&[]).unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn mtimes_through_a_symlinked_directory_is_none() {
    let t = TestRepo::new();
    t.write_file("real/secret", b"inside");
    t.symlink("linkdir", "real");
    t.symlink("outside", "/etc");
    t.write_file("plain", b"p");
    let wt = main_worktree(&t);
    let got = wt
        .mtimes(&[
            rp("linkdir/secret"),
            rp("outside/passwd"),
            rp("real/secret"),
            rp("missing"),
            rp("real/missing"),
            rp("plain/x"),
            rp("real"),
        ])
        .unwrap();
    assert_eq!(
        got,
        [
            None,
            None,
            Some(nanos(&t.root().join("real/secret"))),
            None,
            None,
            None,
            // A directory is neither a regular file nor a symbolic link.
            None,
        ]
    );
}

#[test]
#[cfg(target_os = "linux")]
fn mtimes_of_a_fifo_is_none() {
    let t = TestRepo::new();
    t.write_file("f", b"x");
    rustix::fs::mknodat(
        rustix::fs::CWD,
        t.root().join("pipe"),
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o644),
        0,
    )
    .unwrap();
    let got = main_worktree(&t).mtimes(&[rp("pipe"), rp("f")]).unwrap();
    assert_eq!(got, [None, Some(nanos(&t.root().join("f")))]);
}
