use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::attrs::Attrs;
use crate::changes::{Assembler, Pending, PendingSide};
use crate::handle::{ConfigScope, Handle, hook};
use crate::testing::TestRepo;
use crate::*;

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

fn diff(t: &TestRepo, old: Oid, new: Oid) -> Arc<ChangeSet> {
    diff_with(t, old, new, &Limits::default())
}

fn diff_with(t: &TestRepo, old: Oid, new: Oid, limits: &Limits) -> Arc<ChangeSet> {
    t.repo().diff_trees(old, new, limits, &no_cancel()).unwrap()
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn short(oid: Oid) -> String {
    oid.to_hex()[..7].to_owned()
}

fn mode_str(mode: Mode) -> &'static str {
    match mode {
        Mode::File => "100644",
        Mode::Executable => "100755",
        Mode::Symlink => "120000",
        Mode::Gitlink => "160000",
        Mode::Special => "special",
    }
}

fn path_str(path: &RepoPath) -> String {
    String::from_utf8_lossy(path.as_bytes()).into_owned()
}

fn side_str(side: Option<&Side>) -> String {
    let Some(side) = side else {
        return "-".to_owned();
    };
    let content = match &side.content {
        Content::Blob(oid) => short(*oid),
        Content::Unavailable(Unavailable::NotRetained { oid }) => {
            format!("{} (not retained)", short(*oid))
        }
        Content::Unavailable(why) => format!("{why:?}"),
        Content::Unhashed { size, .. } => format!("unhashed {size}"),
    };
    format!("{} {content}", mode_str(side.mode))
}

fn detail_str(detail: &Detail) -> String {
    match detail {
        Detail::Text(d) => {
            let hunks: Vec<String> = d
                .hunks
                .iter()
                .map(|h| {
                    format!(
                        "{}..{}>{}..{}",
                        h.old.start, h.old.end, h.new.start, h.new.end
                    )
                })
                .collect();
            format!("text +{} -{} [{}]", d.added, d.removed, hunks.join(" "))
        }
        Detail::Truncated { added, removed } => format!("truncated +{added} -{removed}"),
        Detail::Binary { old_size, new_size } => format!("binary {old_size:?} -> {new_size:?}"),
        other => format!("{other:?}").to_lowercase(),
    }
}

/// One line per change: status, path, both sides, the detail, flags; then
/// the totals.
fn render(set: &ChangeSet) -> String {
    let mut out = String::new();
    for c in set.files() {
        let status = match &c.status {
            Status::Added => "A".to_owned(),
            Status::Deleted => "D".to_owned(),
            Status::Modified => "M".to_owned(),
            Status::TypeChanged => "T".to_owned(),
            Status::Renamed { from, similarity } => {
                format!("R{similarity:03} {} =>", path_str(from))
            }
            Status::Conflicted => "U".to_owned(),
        };
        writeln!(
            out,
            "{status} {}  {} -> {}  {}{}",
            path_str(&c.path),
            side_str(c.old.as_ref()),
            side_str(c.new.as_ref()),
            detail_str(&c.detail),
            if c.generated { "  generated" } else { "" },
        )
        .unwrap();
    }
    let t = set.totals();
    let invalid: Vec<String> = t
        .invalid_paths
        .iter()
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect();
    writeln!(
        out,
        "totals: {} files, +{} -{}, truncated {}, too large {}, listed {}, not retained {}, invalid {:?}",
        t.files, t.added, t.removed, t.truncated, t.too_large, t.listed, t.not_retained, invalid
    )
    .unwrap();
    out
}

fn status_letter(status: &Status) -> String {
    match status {
        Status::Added => "A".into(),
        Status::Deleted => "D".into(),
        Status::Modified => "M".into(),
        Status::TypeChanged => "T".into(),
        Status::Renamed { similarity, .. } => format!("R{similarity:03}"),
        Status::Conflicted => "U".into(),
    }
}

/// The change set in `git diff --raw --no-abbrev -z` form.
fn raw(set: &ChangeSet, hash: HashKind) -> Vec<u8> {
    let zeros = "0".repeat(hash.hex_len());
    let mut out = Vec::new();
    for c in set.files() {
        let mode = |s: Option<&Side>| s.map_or("000000", |s| mode_str(s.mode));
        let oid = |s: Option<&Side>| {
            s.and_then(|s| s.content.oid())
                .map_or(zeros.clone(), |o| o.to_hex())
        };
        out.extend_from_slice(
            format!(
                ":{} {} {} {} {}\0",
                mode(c.old.as_ref()),
                mode(c.new.as_ref()),
                oid(c.old.as_ref()),
                oid(c.new.as_ref()),
                status_letter(&c.status)
            )
            .as_bytes(),
        );
        if let Status::Renamed { from, .. } = &c.status {
            out.extend_from_slice(from.as_bytes());
            out.push(0);
        }
        out.extend_from_slice(c.path.as_bytes());
        out.push(0);
    }
    out
}

/// The change set in `git diff --numstat -z` form. git counts a submodule
/// as one "Subproject commit" line per side.
fn numstat(set: &ChangeSet) -> Vec<u8> {
    let mut out = Vec::new();
    for c in set.files() {
        let counts = match &c.detail {
            Detail::Text(d) => format!("{}\t{}", d.added, d.removed),
            Detail::Truncated { added, removed } => format!("{added}\t{removed}"),
            Detail::Binary { .. } => "-\t-".to_owned(),
            Detail::ModeOnly => "0\t0".to_owned(),
            Detail::NoDiff => {
                let gitlink = |s: Option<&Side>| s.is_some_and(|s| s.mode == Mode::Gitlink);
                assert!(
                    gitlink(c.old.as_ref()) || gitlink(c.new.as_ref()),
                    "no comparable numstat for {c:?}"
                );
                let oid = |s: Option<&Side>| s.and_then(|s| s.content.oid());
                if oid(c.old.as_ref()) == oid(c.new.as_ref()) {
                    "0\t0".to_owned()
                } else {
                    format!(
                        "{}\t{}",
                        u8::from(c.new.is_some()),
                        u8::from(c.old.is_some())
                    )
                }
            }
            other => panic!("no comparable numstat for {other:?} at {:?}", c.path),
        };
        out.extend_from_slice(counts.as_bytes());
        out.push(b'\t');
        if let Status::Renamed { from, .. } = &c.status {
            out.push(0);
            out.extend_from_slice(from.as_bytes());
            out.push(0);
        }
        out.extend_from_slice(c.path.as_bytes());
        out.push(0);
    }
    out
}

fn readable(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace('\0', "\n")
}

/// Imports a fixture into a fresh repository, with the variant's
/// configuration (`default` or `renameLimit=<n>`).
fn fixture(name: &str, variant: &str) -> (TestRepo, Oid, Oid) {
    let t = TestRepo::new();
    if let Some(limit) = variant.strip_prefix("renameLimit=") {
        t.append_config(&format!("[diff]\n\trenameLimit = {limit}\n"));
    }
    let (old, new) = t.import_fixture(&fixtures().join(name));
    (t, old, new)
}

// ---- numstat_matches_git ---------------------------------------------------

#[test]
fn numstat_matches_git() {
    let mut checked = 0;
    let mut names: Vec<_> = std::fs::read_dir(fixtures())
        .unwrap()
        .map(|e| e.unwrap())
        // Tree fixtures have a `trees` file; worktree fixtures don't.
        .filter(|e| e.file_type().unwrap().is_dir() && e.path().join("trees").is_file())
        .map(|e| e.file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert!(names.len() >= 4, "{names:?}");
    for name in &names {
        let expected = fixtures().join(name).join("expected");
        let mut variants: Vec<_> = std::fs::read_dir(&expected)
            .unwrap()
            .map(|e| e.unwrap())
            .filter(|e| e.file_type().unwrap().is_dir())
            .map(|e| e.file_name().into_string().unwrap())
            .collect();
        variants.sort();
        for variant in &variants {
            let (t, old, new) = fixture(name, variant);
            let set = diff(&t, old, new);
            let dir = expected.join(variant);
            let git_numstat = std::fs::read(dir.join("numstat")).unwrap();
            let git_raw = std::fs::read(dir.join("raw")).unwrap();
            assert_eq!(
                readable(&raw(&set, HashKind::Sha1)),
                readable(&git_raw),
                "--raw of {name}/{variant}"
            );
            assert_eq!(
                readable(&numstat(&set)),
                readable(&git_numstat),
                "--numstat of {name}/{variant}"
            );
            checked += 1;
        }
    }
    assert!(checked >= 7, "{checked} fixture variants");
}

// ---- rows_* (byte offsets on the fixtures) --------------------------------

/// Every tree fixture: each text file's hunks and rows point at the start of
/// their lines in the texts `texts` returns (CRLF, no final newline, renames
/// and the histogram's degenerate case among them).
#[test]
fn rows_offsets_start_their_lines_in_every_fixture() {
    let mut names: Vec<_> = std::fs::read_dir(fixtures())
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.file_type().unwrap().is_dir() && e.path().join("trees").is_file())
        .map(|e| e.file_name().into_string().unwrap())
        .collect();
    names.sort();
    let (mut files, mut rows) = (Vec::new(), 0);
    for name in &names {
        let (t, old, new) = fixture(name, "default");
        let repo = t.repo();
        let set = diff(&t, old, new);
        for (i, change) in set.files().iter().enumerate() {
            let Detail::Text(d) = &change.detail else {
                continue;
            };
            let texts = set.texts(&repo, i).unwrap();
            let what = format!("{name}/{}", path_str(&change.path));
            rows += crate::rows::tests::assert_offsets(d, &texts, &what);
            files.push(what);
        }
    }
    for covered in [
        "basic/crlf.txt",
        "basic/noeol.txt",
        "renames/new/hi.txt",
        "real/",
    ] {
        assert!(
            files.iter().any(|f| f.starts_with(covered)),
            "{covered} not checked: {files:?}"
        );
    }
    assert!(rows > 1000, "{rows} rows");
}

// ---- diff_trees_* ----------------------------------------------------------

#[test]
fn diff_trees_basic_fixture() {
    let (t, old, new) = fixture("basic", "default");
    let set = diff(&t, old, new);
    assert_eq!(set.base_tree(), old);
    assert_eq!(set.target(), &Target::Tree(new));
    insta::assert_snapshot!(render(&set));
}

#[test]
fn diff_trees_renames_fixture() {
    let (t, old, new) = fixture("renames", "default");
    insta::assert_snapshot!(render(&diff(&t, old, new)));
}

#[test]
fn diff_trees_peels_commits_and_takes_the_empty_tree() {
    let t = TestRepo::new();
    let tree = t.tree(&[("a.txt", b"one\n"), ("dir/b.txt", b"two\n")]);
    let root = t.commit(tree, &[], "root");
    let empty = Oid::empty_tree(HashKind::Sha1);
    assert_eq!(empty.to_hex(), "4b825dc642cb6eb9a060e54bf8d69288fbee4904");
    // A root commit against the empty tree: everything is added.
    let set = diff(&t, empty, root);
    assert_eq!(set.base_tree(), empty);
    assert_eq!(set.target(), &Target::Tree(tree));
    let statuses: Vec<_> = set
        .files()
        .iter()
        .map(|c| (path_str(&c.path), c.status.clone()))
        .collect();
    assert_eq!(
        statuses,
        [
            ("a.txt".into(), Status::Added),
            ("dir/b.txt".into(), Status::Added)
        ]
    );
    assert_eq!(set.totals().added, 2);
    // Commit to commit is tree to tree.
    let tree2 = t.tree(&[("a.txt", b"one\nmore\n"), ("dir/b.txt", b"two\n")]);
    let child = t.commit(tree2, &[root], "child");
    assert_eq!(
        render(&diff(&t, root, child)),
        render(&diff(&t, tree, tree2))
    );
    // The same tree: nothing.
    assert!(diff(&t, root, tree).files().is_empty());
}

#[test]
fn diff_trees_sha256_oids_are_full_length() {
    let t = TestRepo::sha256();
    let old = t.tree(&[("f", b"a\n")]);
    let new = t.tree(&[("f", b"b\n")]);
    let set = diff(&t, old, new);
    let change = &set.files()[0];
    let oid = change.new.as_ref().unwrap().content.oid().unwrap();
    assert_eq!(oid.kind(), HashKind::Sha256);
    assert_eq!(oid.to_hex().len(), 64);
    assert_eq!(oid, Oid::for_blob(HashKind::Sha256, b"b\n"));
    assert_eq!(Oid::empty_tree(HashKind::Sha256).to_hex().len(), 64);
}

#[test]
fn diff_trees_missing_tree_is_object_missing() {
    let t = TestRepo::new();
    let tree = t.tree(&[("f", b"a\n")]);
    let ghost = Oid::for_blob(HashKind::Sha1, b"never written");
    let err = t
        .repo()
        .diff_trees(tree, ghost, &Limits::default(), &no_cancel())
        .unwrap_err();
    assert!(
        matches!(err, GitError::ObjectMissing(id) if id == ghost),
        "{err:?}"
    );
}

#[test]
fn diff_trees_missing_blob_is_unavailable_and_the_rest_continues() {
    let t = TestRepo::new();
    let ghost = Oid::for_blob(HashKind::Sha1, b"not in the object database\n");
    let present = t.blob(b"x\n");
    let old = t.tree_entries(&[("a", Mode::File, present), ("b", Mode::File, present)]);
    let new = t.tree_entries(&[("a", Mode::File, ghost), ("b", Mode::File, t.blob(b"y\n"))]);
    insta::assert_snapshot!(render(&diff(&t, old, new)));
}

#[test]
fn diff_trees_cancelled_returns_cancelled() {
    let t = TestRepo::new();
    let old = t.tree(&[("f", b"a\n")]);
    let new = t.tree(&[("f", b"b\n")]);
    let err = t
        .repo()
        .diff_trees(old, new, &Limits::default(), &AtomicBool::new(true))
        .unwrap_err();
    assert!(matches!(err, GitError::Cancelled), "{err:?}");
}

#[test]
fn diff_trees_too_many_changes() {
    let t = TestRepo::new();
    let old = t.tree(&[]);
    let new = t.tree(&[("a", b"1"), ("b", b"2"), ("c", b"3")]);
    let limits = Limits {
        entries: 2,
        ..Limits::default()
    };
    let err = t
        .repo()
        .diff_trees(old, new, &limits, &no_cancel())
        .unwrap_err();
    assert!(matches!(err, GitError::TooManyEntries), "{err:?}");
    let limits = Limits {
        entries: 3,
        ..Limits::default()
    };
    assert_eq!(diff_with(&t, old, new, &limits).files().len(), 3);
}

#[test]
fn diff_trees_sets_invalid_paths_aside() {
    let t = TestRepo::new();
    let blob = t.blob(b"x\n");
    let old = t.tree(&[]);
    let new = t.tree_entries(&[
        ("ok", Mode::File, blob),
        ("..", Mode::File, blob),
        ("d/.", Mode::File, blob),
    ]);
    let set = diff(&t, old, new);
    assert_eq!(
        render(&set),
        "A ok  - -> 100644 587be6b  text +1 -0 [0..0>0..1]\ntotals: 1 files, +1 -0, truncated 0, too large 0, listed 0, not retained 0, invalid [\"..\", \"d/.\"]\n"
    );
}

#[test]
fn diff_trees_generated_and_binary_follow_the_new_trees_attributes() {
    let t = TestRepo::new();
    let old = t.tree(&[
        ("gen/a.rs", b"1\n"),
        ("sub/data.x", b"1\n"),
        ("src/b.rs", b"1\n"),
    ]);
    let new = t.tree(&[
        (".gitattributes", b"gen/** linguist-generated\n"),
        ("sub/.gitattributes", b"*.x binary\n"),
        ("gen/a.rs", b"2\n"),
        ("sub/data.x", b"2\n"),
        ("src/b.rs", b"2\n"),
    ]);
    insta::assert_snapshot!(render(&diff(&t, old, new)));
}

#[test]
fn diff_trees_texts_read_both_sides() {
    let t = TestRepo::new();
    let sub = Oid::from_hex(HashKind::Sha1, &"1".repeat(40)).unwrap();
    let old = t.tree_entries(&[
        ("big", Mode::File, t.blob(&[b'x'; 100])),
        ("m", Mode::File, t.blob(b"a\nb\n")),
        ("s", Mode::Gitlink, sub),
    ]);
    let new = t.tree_entries(&[
        ("added", Mode::File, t.blob(b"new\nlast")),
        ("big", Mode::File, t.blob(&[b'y'; 100])),
        ("m", Mode::File, t.blob(b"a\nc\n")),
    ]);
    let limits = Limits {
        diff_bytes: 99,
        ..Limits::default()
    };
    let set = diff_with(&t, old, new, &limits);
    let repo = t.repo();
    let paths: Vec<String> = set.files().iter().map(|c| path_str(&c.path)).collect();
    assert_eq!(paths, ["added", "big", "m", "s"]);
    let added = set.texts(&repo, 0).unwrap();
    assert_eq!(added.old, SideText::Absent);
    let SideText::Text(text) = &added.new else {
        panic!("{added:?}")
    };
    assert_eq!(
        (text.line_count(), text.line(0), text.line(1)),
        (2, &b"new"[..], &b"last"[..])
    );
    assert!(!text.ends_with_newline());
    let big = set.texts(&repo, 1).unwrap();
    assert_eq!(big.old, SideText::TooLarge { size: 100 });
    let m = set.texts(&repo, 2).unwrap();
    let (SideText::Text(old_text), SideText::Text(new_text)) = (&m.old, &m.new) else {
        panic!("{m:?}")
    };
    assert_eq!((old_text.line(1), new_text.line(1)), (&b"b"[..], &b"c"[..]));
    let s = set.texts(&repo, 3).unwrap();
    assert_eq!((s.old, s.new), (SideText::Unavailable, SideText::Absent));
    assert!(matches!(set.texts(&repo, 4), Err(GitError::Io(_))));
}

#[test]
fn diff_trees_opens_one_handle_and_keeps_none() {
    let (t, old, new) = fixture("renames", "default");
    let repo = t.repo();
    let before = hook::opened_count();
    let set = repo
        .diff_trees(old, new, &Limits::default(), &no_cancel())
        .unwrap();
    assert_eq!(hook::opened_count() - before, 1);
    assert_eq!(hook::live_count(), 0);
    set.texts(&repo, 0).unwrap();
    assert_eq!(hook::live_count(), 0);
}

// ---- rename_* ---------------------------------------------------------------

fn renamed(set: &ChangeSet) -> Vec<(String, String, u8)> {
    set.files()
        .iter()
        .filter_map(|c| match &c.status {
            Status::Renamed { from, similarity } => {
                Some((path_str(from), path_str(&c.path), *similarity))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn rename_forty_files_moved_and_edited_are_similar_renames() {
    let (t, old, new) = fixture("rename_many", "default");
    let set = diff(&t, old, new);
    let renames = renamed(&set);
    assert_eq!(renames.len(), 45);
    assert_eq!(set.files().len(), 45);
    let similar = renames.iter().filter(|r| r.2 < 100).count();
    assert_eq!(similar, 40);
    assert!(
        renames
            .iter()
            .any(|r| r.0 == "src/f07.rs" && r.1 == "lib/module_07.rs")
    );
    // The detail is the content diff of the pair.
    let change = set
        .files()
        .iter()
        .find(|c| path_str(&c.path) == "lib/module_07.rs")
        .unwrap();
    assert!(matches!(&change.detail, Detail::Text(d) if d.added == 2 && d.removed == 2));
}

#[test]
fn rename_over_the_limit_squared_finds_only_exact_renames() {
    // 40 sources × 40 destinations = 1600 > 39² once the 5 exact renames are out.
    let (t, old, new) = fixture("rename_many", "renameLimit=39");
    let set = diff(&t, old, new);
    let renames = renamed(&set);
    assert_eq!(renames.len(), 5);
    assert!(renames.iter().all(|r| r.2 == 100));
    assert_eq!(set.files().len(), 85);
    // 40² is within the limit.
    let (t, old, new) = fixture("rename_many", "renameLimit=40");
    assert_eq!(renamed(&diff(&t, old, new)).len(), 45);
}

#[test]
fn rename_basename_pairs_survive_a_limit_of_one() {
    let (t, old, new) = fixture("renames", "renameLimit=1");
    let renames = renamed(&diff(&t, old, new));
    // Exact renames and hi.txt (79 % with the same basename); mid.txt (59 %)
    // and the different-basename pairs need the matrix.
    let names: Vec<&str> = renames.iter().map(|r| r.1.as_str()).collect();
    assert_eq!(
        names,
        [
            "new/b.txt",
            "new/empty2",
            "new/hi.txt",
            "new/link",
            "new/script.sh",
            "new/sub",
            "new/z.txt"
        ]
    );
}

#[test]
fn rename_never_pairs_a_modified_file_or_unlike_symlinks() {
    let (t, old, new) = fixture("renames", "default");
    let set = diff(&t, old, new);
    let find = |p: &str| set.files().iter().find(|c| path_str(&c.path) == p).unwrap();
    assert_eq!(find("new/copy.txt").status, Status::Added);
    assert_eq!(find("keep/modified.txt").status, Status::Modified);
    assert_eq!(find("new/link2").status, Status::Added);
    assert_eq!(find("old/link2").status, Status::Deleted);
    assert_eq!(find("new/below2.txt").status, Status::Added);
}

// ---- binary_* --------------------------------------------------------------

#[test]
fn binary_nul_in_the_first_8000_bytes() {
    let t = TestRepo::new();
    let mut late = vec![b'x'; 8000];
    late.extend_from_slice(b"\0late\n");
    let mut early = vec![b'x'; 7999];
    early.extend_from_slice(b"\0early\n");
    let old = t.tree(&[
        ("early", b"text\n"),
        ("late", b"text\n"),
        ("old-binary", b"\0"),
        ("plain", b"a\n"),
    ]);
    let new = t.tree(&[
        ("early", &early),
        ("late", &late),
        ("old-binary", b"text\n"),
        ("plain", b"b\n"),
    ]);
    let set = diff(&t, old, new);
    let details: Vec<String> = set
        .files()
        .iter()
        .map(|c| format!("{} {}", path_str(&c.path), detail_str(&c.detail)))
        .collect();
    assert_eq!(
        details,
        [
            "early binary Some(5) -> Some(8006)",
            "late text +1 -1 [0..1>0..1]",
            "old-binary binary Some(1) -> Some(5)",
            "plain text +1 -1 [0..1>0..1]",
        ]
    );
}

#[test]
fn binary_by_attribute_without_reading_contents() {
    let t = TestRepo::new();
    let attrs: &[u8] = b"*.bin binary\n*.dat -diff\n*.txt diff\n";
    let old = t.tree(&[
        (".gitattributes", attrs),
        ("a.bin", b"1\n"),
        ("b.dat", b"1\n"),
        ("c.txt", b"1\n"),
    ]);
    let new = t.tree(&[
        (".gitattributes", attrs),
        ("a.bin", b"22\n"),
        ("b.dat", b"22\n"),
        ("c.txt", b"22\n"),
    ]);
    let set = diff(&t, old, new);
    let details: Vec<String> = set.files().iter().map(|c| detail_str(&c.detail)).collect();
    assert_eq!(
        details,
        [
            "binary Some(2) -> Some(3)",
            "binary Some(2) -> Some(3)",
            "text +1 -1 [0..1>0..1]"
        ]
    );
}

#[test]
fn binary_added_and_deleted_sides_have_one_size() {
    let t = TestRepo::new();
    // Different contents: runs of NUL alone would hash alike and pair as a
    // rename, as in git.
    let old = t.tree(&[("gone", b"\0ab")]);
    let new = t.tree(&[("new", b"\0x")]);
    let set = diff(&t, old, new);
    let details: Vec<String> = set.files().iter().map(|c| detail_str(&c.detail)).collect();
    assert_eq!(
        details,
        ["binary Some(3) -> None", "binary None -> Some(2)"]
    );
}

// ---- truncate_* --------------------------------------------------------------

fn numbered(n: u32, tag: &str) -> Vec<u8> {
    (0..n)
        .flat_map(|i| format!("{tag} {i}\n").into_bytes())
        .collect()
}

#[test]
fn truncate_per_file_changed_lines_boundary() {
    let t = TestRepo::new();
    // 5 lines replaced: 10 changed lines.
    let old = t.tree(&[("f", &numbered(5, "old"))]);
    let new = t.tree(&[("f", &numbered(5, "new"))]);
    let at = |patch_lines| {
        let limits = Limits {
            patch_lines,
            ..Limits::default()
        };
        detail_str(&diff_with(&t, old, new, &limits).files()[0].detail)
    };
    assert_eq!(at(10), "text +5 -5 [0..5>0..5]");
    assert_eq!(at(9), "truncated +5 -5");
}

#[test]
fn truncate_per_file_patch_bytes_boundary() {
    let t = TestRepo::new();
    let old = t.tree(&[("f", &numbered(5, "old"))]);
    let new = t.tree(&[("f", &numbered(5, "new"))]);
    // "@@ -1,5 +1,5 @@\n" (16) + 5 × "-old i\n" (7) + 5 × "+new i\n" (7).
    let body = 16 + 10 * 7;
    let at = |patch_bytes| {
        let limits = Limits {
            patch_bytes,
            ..Limits::default()
        };
        let set = diff_with(&t, old, new, &limits);
        (detail_str(&set.files()[0].detail), set.totals().truncated)
    };
    assert_eq!(at(body), ("text +5 -5 [0..5>0..5]".to_owned(), 0));
    assert_eq!(at(body - 1), ("truncated +5 -5".to_owned(), 1));
}

#[test]
fn truncate_line_diff_size_boundary() {
    let t = TestRepo::new();
    let old = t.tree(&[("f", &[b'a'; 50]), ("g", b"small\n")]);
    let new = t.tree(&[("f", &[b'b'; 60]), ("g", b"small!\n")]);
    let at = |diff_bytes| {
        let limits = Limits {
            diff_bytes,
            ..Limits::default()
        };
        let set = diff_with(&t, old, new, &limits);
        (detail_str(&set.files()[0].detail), set.totals().too_large)
    };
    assert_eq!(at(60), ("text +1 -1 [0..1>0..1]".to_owned(), 0));
    assert_eq!(at(59), ("toolarge".to_owned(), 1));
}

#[test]
fn truncate_total_file_limit_counts_every_kind() {
    let t = TestRepo::new();
    let sub = Oid::from_hex(HashKind::Sha1, &"1".repeat(40)).unwrap();
    let blob = t.blob(b"same\n");
    let old = t.tree_entries(&[
        ("a-binary", Mode::File, t.blob(b"\0")),
        ("b-mode", Mode::File, blob),
        ("c-sub", Mode::Gitlink, sub),
        ("d-text", Mode::File, t.blob(b"1\n")),
        ("e-text", Mode::File, t.blob(b"1\n")),
    ]);
    let new = t.tree_entries(&[
        ("a-binary", Mode::File, t.blob(b"\0\0")),
        ("b-mode", Mode::Executable, blob),
        (
            "c-sub",
            Mode::Gitlink,
            Oid::from_hex(HashKind::Sha1, &"2".repeat(40)).unwrap(),
        ),
        ("d-text", Mode::File, t.blob(b"2\n")),
        ("e-text", Mode::File, t.blob(b"2\n")),
    ]);
    let limits = Limits {
        total_files: 4,
        ..Limits::default()
    };
    insta::assert_snapshot!(render(&diff_with(&t, old, new, &limits)));
    let limits = Limits {
        total_files: 5,
        ..Limits::default()
    };
    assert_eq!(diff_with(&t, old, new, &limits).totals().listed, 0);
}

#[test]
fn truncate_total_patch_bytes_lists_the_files_after_it() {
    let t = TestRepo::new();
    let old = t.tree(&[("a", b"1\n"), ("b", b"1\n"), ("c", b"1\n")]);
    let new = t.tree(&[("a", b"2\n"), ("b", b"2\n"), ("c", b"2\n")]);
    let one_patch = {
        let set = diff(&t, old, new);
        assert_eq!(set.totals().listed, 0);
        // Body "@@ -1,1 +1,1 @@\n" (16) + "-1\n" + "+2\n", header for "a" → "a".
        16 + 3 + 3 + (2 * 2 + 100)
    };
    let at = |total_patch_bytes| {
        let limits = Limits {
            total_patch_bytes,
            ..Limits::default()
        };
        let set = diff_with(&t, old, new, &limits);
        set.files()
            .iter()
            .map(|c| detail_str(&c.detail))
            .collect::<Vec<_>>()
    };
    // The file that reaches the limit is kept; the ones after it are listed.
    assert_eq!(
        at(one_patch),
        ["text +1 -1 [0..1>0..1]", "listed", "listed"]
    );
    assert_eq!(
        at(one_patch + 1),
        ["text +1 -1 [0..1>0..1]", "text +1 -1 [0..1>0..1]", "listed"]
    );
    assert_eq!(
        at(3 * one_patch + 1)
            .iter()
            .filter(|d| *d == "listed")
            .count(),
        0
    );
}

#[test]
fn truncate_listed_and_truncated_files_keep_full_oids() {
    let t = TestRepo::new();
    let old = t.tree(&[("a", &numbered(5, "old")), ("b", b"1\n")]);
    let new = t.tree(&[("a", &numbered(5, "new")), ("b", b"2\n")]);
    let limits = Limits {
        patch_lines: 1,
        total_files: 1,
        ..Limits::default()
    };
    let set = diff_with(&t, old, new, &limits);
    for (c, path, content) in [
        (&set.files()[0], "a", numbered(5, "new")),
        (&set.files()[1], "b", b"2\n".to_vec()),
    ] {
        assert_eq!(path_str(&c.path), path);
        assert_eq!(
            c.new.as_ref().unwrap().content,
            Content::Blob(Oid::for_blob(HashKind::Sha1, &content))
        );
    }
    assert_eq!(detail_str(&set.files()[0].detail), "truncated +5 -5");
    assert_eq!(detail_str(&set.files()[1].detail), "listed");
    assert_eq!((set.totals().added, set.totals().removed), (5, 5));
}

/// Runs the assembly on in-memory new sides, as the worktree comparison
/// will: `(path, old content, new content)`.
fn assemble_in_memory(t: &TestRepo, files: &[(&str, &[u8], &[u8])], limits: &Limits) -> String {
    let handle = Handle::open(t.common_dir(), ConfigScope::RepositoryOnly).unwrap();
    let pending: Vec<Pending> = files
        .iter()
        .map(|(path, old, new)| {
            let old_oid = t.blob(old);
            let new_oid = Oid::for_blob(HashKind::Sha1, new);
            Pending::new(
                Status::Modified,
                RepoPath::new(path.as_bytes()).unwrap(),
                Some(PendingSide {
                    mode: Mode::File,
                    content: Content::Blob(old_oid),
                    bytes: None,
                }),
                Some(PendingSide {
                    mode: Mode::File,
                    content: Content::Blob(new_oid),
                    bytes: Some(Arc::from(*new)),
                }),
            )
        })
        .collect();
    let mut attrs = |_: &RepoPath| Ok(Attrs::default());
    let cancel = no_cancel();
    let assembled = Assembler {
        handle: &handle,
        limits,
        cancel: &cancel,
        attrs: &mut attrs,
    }
    .run(pending)
    .unwrap();
    let mut out = String::new();
    for (change, kept) in assembled.files.iter().zip(&assembled.retained) {
        writeln!(
            out,
            "{} {} {}  kept {:?}",
            path_str(&change.path),
            side_str(change.new.as_ref()),
            detail_str(&change.detail),
            kept.as_ref().map(|b| b.len()),
        )
        .unwrap();
    }
    writeln!(out, "not retained {}", assembled.totals.not_retained).unwrap();
    out
}

#[test]
fn truncate_retained_content_boundary_keeps_each_detail() {
    let t = TestRepo::new();
    let mut big = vec![b'x'; 40];
    big.push(b'\n');
    let files: &[(&str, &[u8], &[u8])] = &[
        ("a-text", b"1\n", b"22\n"),      // 3 bytes
        ("b-text", b"1\n", b"333\n"),     // 4 bytes: exactly the limit of 7
        ("c-text", b"1\n", b"4444\n"),    // over the limit: not retained
        ("d-binary", b"1\n", b"\0\0"),    // NotRetained keeps Binary
        ("e-large", b"1\n", &big),        // and TooLarge (over diff_bytes)
        ("f-empty", b"1\n", b""),         // would fit, but retention stopped
        ("g-same", b"same\n", b"same\n"), // the old blob: needs no keeping
    ];
    let limits = Limits {
        retained_bytes: 7,
        diff_bytes: 20,
        ..Limits::default()
    };
    insta::assert_snapshot!(assemble_in_memory(&t, files, &limits));
    // With room for all of them nothing is dropped.
    let limits = Limits {
        diff_bytes: 20,
        ..Limits::default()
    };
    assert!(assemble_in_memory(&t, files, &limits).ends_with("not retained 0\n"));
}

// ---- hunk_cache_* ------------------------------------------------------------

#[test]
fn hunk_cache_hit_has_the_offsets_of_its_own_contents() {
    // Two repositories with the same contents under other paths share the
    // entry; the offsets are the contents', so they hold in both.
    let old: &[u8] = b"hunk_cache_hit_has_the_offsets\r\nsame\r\nold\r\n";
    let new: &[u8] = b"hunk_cache_hit_has_the_offsets\r\nsame\r\nnew\r\nmore";
    let (hits, misses) = crate::hunks::hook::counts();
    let mut details = Vec::new();
    for path in ["first", "dir/second"] {
        let t = TestRepo::new();
        let (o, n) = (t.tree(&[(path, old)]), t.tree(&[(path, new)]));
        let set = diff(&t, o, n);
        let Detail::Text(d) = &set.files()[0].detail else {
            panic!("{:?}", set.files()[0].detail);
        };
        let texts = set.texts(&t.repo(), 0).unwrap();
        crate::rows::tests::assert_offsets(d, &texts, path);
        details.push(d.clone());
    }
    assert_eq!(crate::hunks::hook::counts(), (hits + 1, misses + 1));
    assert_eq!(details[0], details[1]);
    let h = &details[0].hunks[0];
    assert_eq!((h.old.clone(), h.new.clone()), (2..3, 2..4));
    assert_eq!((h.old_byte, h.new_byte), (38, 38));
}

#[test]
fn hunk_cache_second_computation_hits() {
    // Contents unique to this test, so other tests can't warm the cache.
    let t = TestRepo::new();
    let old = t.tree(&[("f", b"hunk_cache_second_computation_hits 1\n")]);
    let new = t.tree(&[("f", b"hunk_cache_second_computation_hits 2\n")]);
    let (hits, misses) = crate::hunks::hook::counts();
    let first = diff(&t, old, new);
    assert_eq!(crate::hunks::hook::counts(), (hits, misses + 1));
    let second = diff(&t, old, new);
    assert_eq!(crate::hunks::hook::counts(), (hits + 1, misses + 1));
    assert_eq!(render(&first), render(&second));
    // The hit has the same hunks, byte offsets included.
    assert_eq!(first.files()[0].detail, second.files()[0].detail);
    // Another repository with the same contents shares the entry.
    let other = TestRepo::new();
    let old = other.tree(&[("elsewhere", b"hunk_cache_second_computation_hits 1\n")]);
    let new = other.tree(&[("elsewhere", b"hunk_cache_second_computation_hits 2\n")]);
    diff(&other, old, new);
    assert_eq!(crate::hunks::hook::counts(), (hits + 2, misses + 1));
}

#[test]
fn hunk_cache_hit_checks_limits_attributes_and_missing_objects() {
    // A hit takes the sizes from the cache and only checks that the objects
    // exist; the results are those of reading the headers.
    let contents: [&[u8]; 2] = [
        b"hunk_cache_hit_checks 1\n",
        b"hunk_cache_hit_checks 2, longer\n",
    ];
    let t = TestRepo::new();
    let (old, new) = (t.tree(&[("f", contents[0])]), t.tree(&[("f", contents[1])]));
    diff(&t, old, new);
    let (hits, misses) = crate::hunks::hook::counts();
    let limits = Limits {
        diff_bytes: contents[0].len() as u64,
        ..Limits::default()
    };
    let set = diff_with(&t, old, new, &limits);
    assert_eq!(set.files()[0].detail, Detail::TooLarge);
    assert_eq!(crate::hunks::hook::counts(), (hits + 1, misses));

    // `-diff` never looks the pair up and reports both sizes.
    let attrs = t.blob(b"f -diff\n");
    let side = |content: &[u8]| {
        t.tree_entries(&[
            (".gitattributes", Mode::File, attrs),
            ("f", Mode::File, t.blob(content)),
        ])
    };
    let set = diff(&t, side(contents[0]), side(contents[1]));
    assert_eq!(
        set.files()[0].detail,
        Detail::Binary {
            old_size: Some(contents[0].len() as u64),
            new_size: Some(contents[1].len() as u64),
        }
    );
    assert_eq!(crate::hunks::hook::counts(), (hits + 1, misses));

    // Another repository without the new blob: a hit, but the side is missing.
    let other = TestRepo::new();
    let present = other.blob(contents[0]);
    let absent = Oid::for_blob(HashKind::Sha1, contents[1]);
    let old = other.tree_entries(&[("f", Mode::File, present)]);
    let new = other.tree_entries(&[("f", Mode::File, absent)]);
    let set = diff(&other, old, new);
    assert_eq!(crate::hunks::hook::counts(), (hits + 2, misses));
    let change = &set.files()[0];
    assert_eq!(change.detail, Detail::NoDiff);
    assert_eq!(
        change.new.as_ref().unwrap().content,
        Content::Unavailable(Unavailable::Missing)
    );
}

#[test]
fn hunk_cache_misses_across_hash_kinds() {
    let contents: [&[u8]; 2] = [
        b"hunk_cache_misses_across_hash_kinds 1\n",
        b"hunk_cache_misses_across_hash_kinds 2\n",
    ];
    let sha1 = TestRepo::new();
    let (o, n) = (
        sha1.tree(&[("f", contents[0])]),
        sha1.tree(&[("f", contents[1])]),
    );
    let (hits, misses) = crate::hunks::hook::counts();
    diff(&sha1, o, n);
    let sha256 = TestRepo::sha256();
    let (o, n) = (
        sha256.tree(&[("f", contents[0])]),
        sha256.tree(&[("f", contents[1])]),
    );
    diff(&sha256, o, n);
    assert_eq!(crate::hunks::hook::counts(), (hits, misses + 2));
}

#[test]
fn hunk_cache_skips_additions_and_type_changes() {
    let t = TestRepo::new();
    let target = t.blob(b"hunk_cache_skips target");
    let old = t.tree_entries(&[("l", Mode::File, t.blob(b"hunk_cache_skips file\n"))]);
    let new = t.tree_entries(&[
        ("l", Mode::Symlink, target),
        ("n", Mode::File, t.blob(b"hunk_cache_skips new\n")),
    ]);
    let before = crate::hunks::hook::counts();
    let set = diff(&t, old, new);
    assert_eq!(crate::hunks::hook::counts(), before);
    // A type change counts like git: every old line out, every new line in.
    assert_eq!(detail_str(&set.files()[0].detail), "text +1 -1 [0..1>0..1]");
    assert_eq!(set.files()[0].status, Status::TypeChanged);
}
