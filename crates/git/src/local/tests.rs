//! `change_source_*` (docs/design/review.md「改动来源」): the conversion of
//! `ChangeSet` to `ChangeList` field by field, refresh sharing, texts,
//! fingerprint and errors.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use lectern_services::changes::{
    BaseSpec, ChangeDetail, ChangeError, ChangeList, ChangePath, ChangeSource, ChangeStatus,
    DiffHunk, FileChange, FileMode, FileSide, ListHandle, RawIdentity as SvcRaw, RefreshHint,
    SideIdentity, SideText as SvcText, SourceKind, UnavailableKind,
};
use lectern_services::project::{BlobId, ProjectKey, WorktreeKey};

use crate::testing::{IndexEntry, TestRepo};
use crate::*;

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

fn source_with(t: &TestRepo, base: BaseSpec, limits: Limits) -> LocalChanges {
    let repo = t.repo();
    let worktree = repo.worktree(&WorktreeId::Main).unwrap();
    LocalChanges::new(
        repo,
        worktree,
        ProjectKey::Git {
            common_dir: t.common_dir().into(),
        },
        WorktreeKey(t.common_dir().into()),
        base,
        limits,
    )
}

fn source(t: &TestRepo) -> LocalChanges {
    source_with(t, BaseSpec::Head, Limits::default())
}

fn fetch(source: &LocalChanges) -> Arc<ChangeList> {
    source.fetch(&no_cancel()).unwrap()
}

/// The `ChangeSet` the list's handle holds.
fn set_of<'a>(source: &LocalChanges, list: &'a ChangeList) -> &'a Arc<ChangeSet> {
    source.held(list).expect("a list of this source")
}

fn commit_files(t: &TestRepo, files: &[(&str, &[u8])]) -> Oid {
    let commit = t.commit(t.tree(files), &[], "base");
    t.set_ref("refs/heads/main", commit);
    commit
}

/// HEAD and the index are `files`, the worktree matches them by stat.
fn clean_repo(files: &[(&str, &[u8])]) -> TestRepo {
    let t = TestRepo::new();
    for (path, content) in files {
        t.write_file(path, content);
        t.backdate(path);
    }
    commit_files(&t, files);
    let entries: Vec<IndexEntry> = files.iter().map(|(p, _)| t.add(p)).collect();
    t.write_index(t.common_dir(), &entries);
    t
}

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

// ---- field by field ------------------------------------------------------------

fn same_id(oid: Oid, id: &BlobId) {
    let kind = match id {
        BlobId::Sha1(_) => HashKind::Sha1,
        BlobId::Sha256(_) => HashKind::Sha256,
    };
    assert_eq!(oid.kind(), kind);
    assert_eq!(oid.as_bytes(), id.as_bytes());
}

fn same_mode(mode: Mode, m: FileMode) {
    let expected = match mode {
        Mode::File => FileMode::File,
        Mode::Executable => FileMode::Executable,
        Mode::Symlink => FileMode::Symlink,
        Mode::Gitlink => FileMode::Gitlink,
        Mode::Special => FileMode::Special,
    };
    assert_eq!(m, expected);
}

fn same_path(path: &RepoPath, p: &ChangePath) {
    assert_eq!(path.as_bytes(), &p.0[..]);
    assert!(Arc::ptr_eq(path.shared(), &p.0), "{path:?} was copied");
}

fn same_side(side: &Side, s: &FileSide) {
    same_mode(side.mode, s.mode);
    match (&side.content, &s.content) {
        (Content::Blob(oid), SideIdentity::Blob(id)) => same_id(*oid, id),
        (
            Content::Unhashed {
                size,
                mtime_ns,
                inode,
            },
            SideIdentity::Unhashed {
                size: s,
                mtime_ns: m,
                inode: i,
            },
        ) => assert_eq!((*size, *mtime_ns, *inode), (*s, i128::from(*m), *i)),
        (Content::Unavailable(Unavailable::NotRetained { oid }), SideIdentity::NotRetained(id)) => {
            same_id(*oid, id)
        }
        (Content::Unavailable(Unavailable::Filtered { raw }), SideIdentity::Filtered(r)) => {
            match (raw, r) {
                (RawIdentity::Sha256(a), SvcRaw::Sha256(b)) => assert_eq!(a, b),
                (
                    RawIdentity::Stat {
                        size,
                        mtime_ns,
                        inode,
                    },
                    SvcRaw::Stat {
                        size: s,
                        mtime_ns: m,
                        inode: i,
                    },
                ) => assert_eq!((*size, *mtime_ns, *inode), (*s, i128::from(*m), *i)),
                other => panic!("{other:?}"),
            }
        }
        (Content::Unavailable(u), SideIdentity::Unavailable(k)) => {
            let expected = match u {
                Unavailable::UnsupportedType => UnavailableKind::UnsupportedType,
                Unavailable::PathThroughSymlink => UnavailableKind::PathThroughSymlink,
                Unavailable::NestedRepository => UnavailableKind::NestedRepository,
                Unavailable::Missing => UnavailableKind::Missing,
                Unavailable::Unreadable => UnavailableKind::Unreadable,
                other => panic!("{other:?} is not a plain Unavailable"),
            };
            assert_eq!(*k, expected);
        }
        other => panic!("{other:?}"),
    }
}

fn same_opt_side(side: Option<&Side>, s: Option<&FileSide>) {
    match (side, s) {
        (None, None) => {}
        (Some(side), Some(s)) => same_side(side, s),
        other => panic!("{other:?}"),
    }
}

fn same_change(c: &Change, f: &FileChange) {
    match (&c.status, &f.status) {
        (Status::Added, ChangeStatus::Added)
        | (Status::Deleted, ChangeStatus::Deleted)
        | (Status::Modified, ChangeStatus::Modified)
        | (Status::TypeChanged, ChangeStatus::TypeChanged)
        | (Status::Conflicted, ChangeStatus::Conflicted) => {}
        (
            Status::Renamed { from, similarity },
            ChangeStatus::Renamed {
                from: f,
                similarity: s,
            },
        ) => {
            same_path(from, f);
            assert_eq!(similarity, s);
        }
        other => panic!("{other:?}"),
    }
    same_path(&c.path, &f.path);
    same_opt_side(c.old.as_ref(), f.old.as_ref());
    same_opt_side(c.new.as_ref(), f.new.as_ref());
    match (&c.detail, &f.detail) {
        (
            Detail::Text(diff),
            ChangeDetail::Text {
                hunks,
                added,
                removed,
            },
        ) => {
            assert_eq!((diff.added, diff.removed), (*added, *removed));
            assert_eq!(diff.hunks.len(), hunks.len());
            for (h, d) in diff.hunks.iter().zip(hunks.iter()) {
                assert_eq!(
                    (&h.old, &h.new, h.old_byte, h.new_byte),
                    (&d.old, &d.new, d.old_byte, d.new_byte)
                );
            }
        }
        (
            Detail::Truncated { added, removed },
            ChangeDetail::Truncated {
                added: a,
                removed: r,
            },
        ) => assert_eq!((added, removed), (a, r)),
        (
            Detail::Binary { old_size, new_size },
            ChangeDetail::Binary {
                old_size: o,
                new_size: n,
            },
        ) => assert_eq!((old_size, new_size), (o, n)),
        (Detail::TooLarge, ChangeDetail::TooLarge)
        | (Detail::Listed, ChangeDetail::Listed)
        | (Detail::ModeOnly, ChangeDetail::ModeOnly)
        | (Detail::StagedOnly, ChangeDetail::StagedOnly)
        | (Detail::NoDiff, ChangeDetail::NoDiff) => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(c.generated, f.generated);
    assert_eq!(c.untracked, f.untracked);
    match (&c.staged, &f.staged) {
        (None, None) => {}
        (Some(staged), Some(s)) => {
            same_mode(staged.mode, s.mode);
            let SideIdentity::Blob(id) = &s.content else {
                panic!("{s:?}");
            };
            same_id(staged.oid, id);
        }
        other => panic!("{other:?}"),
    }
}

/// Every field of `list` against the `ChangeSet` it was converted from.
fn assert_same(set: &ChangeSet, list: &ChangeList) {
    same_id(set.base_tree(), &list.base);
    assert_eq!(set.files().len(), list.files.len());
    for (c, f) in set.files().iter().zip(list.files.iter()) {
        same_change(c, f);
    }
    let t = set.totals();
    let l = &list.totals;
    assert_eq!(
        (
            t.files,
            t.added,
            t.removed,
            t.truncated,
            t.too_large,
            t.listed,
            t.not_retained,
            t.untracked_capped
        ),
        (
            l.files as usize,
            l.added,
            l.removed,
            l.truncated as usize,
            l.too_large as usize,
            l.listed as usize,
            l.not_retained as usize,
            l.untracked_capped
        )
    );
    assert!(Arc::ptr_eq(&t.invalid_paths, &l.invalid_paths));
}

/// `fetch`, checked field by field against its `ChangeSet`.
fn checked(source: &LocalChanges) -> Arc<ChangeList> {
    let list = fetch(source);
    assert_same(set_of(source, &list), &list);
    list
}

// ---- the snapshot form ---------------------------------------------------------------

fn short(id: &BlobId) -> String {
    id.to_string()[..7].to_owned()
}

fn mode(m: FileMode) -> &'static str {
    match m {
        FileMode::File => "f",
        FileMode::Executable => "x",
        FileMode::Symlink => "l",
        FileMode::Gitlink => "g",
        FileMode::Special => "s",
    }
}

fn side(s: Option<&FileSide>) -> String {
    let Some(s) = s else {
        return "-".into();
    };
    // Sizes only: mtimes and inodes differ from run to run (compared above).
    let content = match &s.content {
        SideIdentity::Blob(id) => short(id),
        SideIdentity::NotRetained(id) => format!("not-retained:{}", short(id)),
        SideIdentity::Unhashed { size, .. } => format!("unhashed({size})"),
        SideIdentity::Filtered(SvcRaw::Sha256(_)) => "filtered".into(),
        SideIdentity::Filtered(SvcRaw::Stat { size, .. }) => format!("filtered-stat({size})"),
        SideIdentity::Unavailable(kind) => format!("{kind:?}"),
    };
    format!("{}:{content}", mode(s.mode))
}

fn line(f: &FileChange) -> String {
    let path = |p: &ChangePath| String::from_utf8_lossy(&p.0).into_owned();
    let status = match &f.status {
        ChangeStatus::Added => "A".to_owned(),
        ChangeStatus::Deleted => "D".to_owned(),
        ChangeStatus::Modified => "M".to_owned(),
        ChangeStatus::TypeChanged => "T".to_owned(),
        ChangeStatus::Conflicted => "C".to_owned(),
        ChangeStatus::Renamed { from, similarity } => format!("R{similarity}({})", path(from)),
    };
    let detail = match &f.detail {
        ChangeDetail::Text {
            hunks,
            added,
            removed,
        } => {
            let hunks: Vec<String> = hunks
                .iter()
                .map(|h| {
                    format!(
                        "{}..{}@{}/{}..{}@{}",
                        h.old.start, h.old.end, h.old_byte, h.new.start, h.new.end, h.new_byte
                    )
                })
                .collect();
            format!("text(+{added},-{removed})[{}]", hunks.join(" "))
        }
        ChangeDetail::Truncated { added, removed } => format!("truncated(+{added},-{removed})"),
        ChangeDetail::Binary { old_size, new_size } => format!("binary({old_size:?},{new_size:?})"),
        ChangeDetail::TooLarge => "too-large".into(),
        ChangeDetail::Listed => "listed".into(),
        ChangeDetail::ModeOnly => "mode-only".into(),
        ChangeDetail::StagedOnly => "staged-only".into(),
        ChangeDetail::NoDiff => "no-diff".into(),
    };
    let mut out = format!(
        "{status} {}{} {} -> {} {detail}",
        path(&f.path),
        if f.untracked { " (untracked)" } else { "" },
        side(f.old.as_ref()),
        side(f.new.as_ref()),
    );
    if let Some(s) = &f.staged {
        out.push_str(&format!(" staged={}", side(Some(s))));
    }
    if f.generated {
        out.push_str(" generated");
    }
    out
}

fn render(list: &ChangeList) -> String {
    let mut out = format!("base {}\n", short(&list.base));
    for f in list.files.iter() {
        out.push_str(&line(f));
        out.push('\n');
    }
    let t = &list.totals;
    out.push_str(&format!(
        "totals files={} +{} -{} truncated={} too_large={} listed={} not_retained={} capped={}",
        t.files,
        t.added,
        t.removed,
        t.truncated,
        t.too_large,
        t.listed,
        t.not_retained,
        t.untracked_capped
    ));
    for p in t.invalid_paths.iter() {
        out.push_str(&format!(" invalid={}", String::from_utf8_lossy(p)));
    }
    out
}

// ---- change_source_*: conversion -------------------------------------------------------

/// The worktree fixture (`tests/fixtures/README.md`「工作区夹具」) under
/// each `core.autocrlf`.
#[test]
fn change_source_conversion_fixture_matches_field_by_field() {
    let dir = fixture("worktree_conversions");
    let mut out = String::new();
    for autocrlf in ["false", "true", "input"] {
        let t = TestRepo::new();
        t.append_config(&format!("[core]\n\tautocrlf = {autocrlf}\n"));
        t.import_worktree_fixture(&dir);
        let list = checked(&source(&t));
        assert!(list.files.len() >= 10, "{autocrlf}");
        out.push_str(&format!("# core.autocrlf = {autocrlf}\n"));
        out.push_str(&render(&list));
        out.push('\n');
    }
    insta::assert_snapshot!(out);
}

/// What the worktree holds at a path of the table in git.md「工作区比较」.
#[derive(Clone, Copy)]
enum W<'a> {
    Absent,
    Content(&'a [u8]),
    Socket,
}

const A: &[u8] = b"base\n";
const S: &[u8] = b"staged\n";
const C: &[u8] = b"changed\n";
const LFS_CONFIG: &str = "[filter \"lfs\"]\n\tclean = git-lfs clean -- %f\n\tsmudge = git-lfs smudge -- %f\n\tprocess = git-lfs filter-process\n\trequired = true\n";

/// Every row of the worktree table, a rename, a conflict, a mode-only
/// change, a symbolic link, binary, filtered, unhashed and generated files
/// and an invalid index path.
#[test]
fn change_source_every_kind_matches_field_by_field() {
    use std::os::unix::fs::PermissionsExt;

    let t = TestRepo::new();
    t.append_config(LFS_CONFIG);
    type Row<'a> = (&'a str, Option<&'a [u8]>, Option<&'a [u8]>, W<'a>);
    let rows: [Row<'_>; 16] = [
        ("01-untracked", None, None, W::Content(C)),
        ("02-add-rm", None, Some(S), W::Absent),
        ("03-added", None, Some(S), W::Content(S)),
        ("04-added-changed", None, Some(S), W::Content(C)),
        ("05-added-socket", None, Some(S), W::Socket),
        ("06-removed", Some(A), None, W::Absent),
        ("07-rm-cached", Some(A), None, W::Content(A)),
        ("08-unstaged-rm", Some(A), Some(A), W::Absent),
        ("09-unchanged", Some(A), Some(A), W::Content(A)),
        ("10-unstaged", Some(A), Some(A), W::Content(C)),
        ("11-socket", Some(A), Some(A), W::Socket),
        ("12-staged-rm", Some(A), Some(S), W::Absent),
        ("13-staged", Some(A), Some(S), W::Content(S)),
        ("14-staged-reverted", Some(A), Some(S), W::Content(A)),
        ("15-staged-changed", Some(A), Some(S), W::Content(C)),
        ("16-staged-socket", Some(A), Some(S), W::Socket),
    ];
    let attrs: &[u8] = b"*.bin filter=lfs -text\n*.gen linguist-generated\n";
    let moved: &[u8] = b"line 1\nline 2\nline 3\n";
    let run: &[u8] = b"run\n";
    let mut base: Vec<(&str, &[u8])> = vec![
        (".gitattributes", attrs),
        ("m-old", moved),
        ("run", run),
        ("x.gen", b"one\ntwo\n"),
    ];
    let mut entries = Vec::new();
    for (path, b, i, _) in &rows {
        if let Some(b) = b {
            base.push((*path, *b));
        }
        if let Some(i) = i {
            t.write_file(path, i);
            t.backdate(path);
            entries.push(t.add(path));
            t.remove(path);
        }
    }
    for (path, content) in [
        (".gitattributes", attrs),
        ("m-new", moved),
        ("run", run),
        ("x.gen", b"one\ntwo\n"),
    ] {
        t.write_file(path, content);
        t.backdate(path);
        entries.push(t.add(path));
    }
    entries.push(IndexEntry::new("../evil", Mode::File, t.blob(b"x")));
    let conflict = |stage, content: &[u8]| IndexEntry {
        stage,
        ..IndexEntry::new("conflict", Mode::File, t.blob(content))
    };
    entries.push(conflict(1, b"base\n"));
    entries.push(conflict(3, b"theirs\n"));
    entries.sort_by(|a, b| (&a.path, a.stage).cmp(&(&b.path, b.stage)));
    commit_files(&t, &base);
    t.write_index(t.common_dir(), &entries);
    for (path, _, i, w) in &rows {
        match w {
            W::Absent => {}
            W::Content(content) => {
                t.write_file(path, content);
                if Some(*content) == *i {
                    t.backdate(path);
                }
            }
            W::Socket => {
                std::os::unix::net::UnixListener::bind(t.root().join(path)).unwrap();
            }
        }
    }
    t.write_file("conflict", b"theirs\n");
    std::fs::set_permissions(t.root().join("run"), std::fs::Permissions::from_mode(0o755)).unwrap();
    t.write_file("x.gen", b"one\nTWO\n");
    t.symlink("link", "09-unchanged");
    t.write_file("bin.dat", b"\0binary\0");
    t.write_file("new.bin", b"raw lfs data");
    t.write_file("big", &[b'x'; 300]);
    let limits = Limits {
        read_bytes: 200,
        ..Limits::default()
    };
    let list = checked(&source_with(&t, BaseSpec::Head, limits));
    let rendered = render(&list);
    for wanted in [
        "R100(m-old) m-new",
        "C conflict",
        "mode-only",
        "l:",
        "binary(",
        "f:filtered",
        "unhashed(300)",
        "generated",
        "staged=",
        "staged-only",
        "UnsupportedType",
        "invalid=../evil",
    ] {
        assert!(rendered.contains(wanted), "{wanted} in\n{rendered}");
    }
    insta::assert_snapshot!(rendered);
}

/// Truncated, too large, listed and not-retained files.
#[test]
fn change_source_limits_match_field_by_field() {
    let t = clean_repo(&[("keep", b"k\n")]);
    t.write_file("a.txt", b"aaaa\n");
    t.write_file("b.bin", b"\0binary content");
    t.write_file("c.txt", b"one\ntwo\nthree\n");
    t.write_file("d.big", &[b'd'; 60]);
    t.write_file("e.txt", b"1\n2\n3\n4\n");
    t.write_file("f.txt", b"listed\n");
    let limits = Limits {
        retained_bytes: 10,
        diff_bytes: 50,
        patch_lines: 3,
        total_files: 5,
        ..Limits::default()
    };
    let list = checked(&source_with(&t, BaseSpec::Head, limits));
    let t = &list.totals;
    assert!(t.not_retained > 0 && t.too_large > 0 && t.listed > 0 && t.truncated > 0);
    insta::assert_snapshot!(render(&list));
}

#[test]
fn change_source_sha256_ids_have_the_full_length() {
    let t = TestRepo::sha256();
    t.write_file("a", b"a\n");
    t.backdate("a");
    let commit = t.commit(t.tree(&[("a", b"a\n")]), &[], "c");
    t.set_ref("refs/heads/main", commit);
    t.write_index(t.common_dir(), &[t.add("a")]);
    t.write_file("a", b"b\n");
    t.write_file("u", b"u\n");
    let list = checked(&source(&t));
    assert!(matches!(list.base, BlobId::Sha256(_)));
    assert_eq!(list.files.len(), 2);
    for f in list.files.iter() {
        let content = f.new.as_ref().unwrap().content;
        let sha256 = matches!(content, SideIdentity::Blob(BlobId::Sha256(_)));
        assert!(sha256, "{content:?}");
    }
}

#[test]
fn change_source_info_and_generations() {
    let t = clean_repo(&[("a", b"a\n")]);
    let s = source(&t);
    let info = s.info();
    assert!(info.caps.refresh && info.caps.fingerprint);
    let SourceKind::Worktree {
        project,
        worktree,
        base,
    } = &info.kind;
    assert_eq!(
        project,
        &ProjectKey::Git {
            common_dir: t.common_dir().into()
        }
    );
    assert_eq!(worktree, &WorktreeKey(t.common_dir().into()));
    assert_eq!(base, &BaseSpec::Head);
    let first = fetch(&s);
    let second = s.refresh(&first, &RefreshHint::Full, &no_cancel()).unwrap();
    let third = fetch(&s);
    assert_eq!(
        (first.generation, second.generation, third.generation),
        (1, 2, 3)
    );
}

#[test]
fn change_source_bases_map_to_the_same_trees() {
    let t = TestRepo::new();
    let c1 = t.commit(t.tree(&[("a", b"1\n")]), &[], "c1");
    let c2 = t.commit(t.tree(&[("a", b"2\n")]), &[c1], "c2");
    let c3 = t.commit(t.tree(&[("a", b"3\n")]), &[c1], "c3");
    t.set_ref("refs/heads/main", c2);
    t.set_ref("refs/heads/feature", c3);
    t.write_file("a", b"2\n");
    t.backdate("a");
    t.write_index(t.common_dir(), &[t.add("a")]);
    let tree_of = |c: Oid| t.repo().commit(c).unwrap().tree;
    let id = |oid: Oid| BlobId::from_bytes(oid.as_bytes()).unwrap();

    let head = checked(&source(&t));
    assert_eq!(head.base, id(tree_of(c2)));
    assert!(head.files.is_empty());
    let at_c1 = BaseSpec::Commit(id(c1));
    let commit = checked(&source_with(&t, at_c1, Limits::default()));
    assert_eq!(commit.base, id(tree_of(c1)));
    assert_eq!(commit.files.len(), 1);
    let merge_base = checked(&source_with(
        &t,
        BaseSpec::MergeBase("refs/heads/feature".into()),
        Limits::default(),
    ));
    assert_eq!(merge_base.base, id(tree_of(c1)));
    assert_eq!(merge_base.files, commit.files);
}

#[test]
fn change_source_errors_keep_their_names() {
    let t = TestRepo::new();
    let c1 = t.commit(t.tree(&[("a", b"1\n")]), &[], "c1");
    let other = t.commit(t.tree(&[("b", b"2\n")]), &[], "other");
    t.set_ref("refs/heads/main", c1);
    t.set_ref("refs/heads/other", other);
    let err = |base: BaseSpec, cancel: bool| {
        source_with(&t, base, Limits::default())
            .fetch(&AtomicBool::new(cancel))
            .unwrap_err()
    };
    assert_eq!(err(BaseSpec::Head, true), ChangeError::Cancelled);
    assert_eq!(
        err(BaseSpec::MergeBase("refs/heads/other".into()), false),
        ChangeError::NoMergeBase
    );
    assert!(matches!(
        err(BaseSpec::MergeBase("main".into()), false),
        ChangeError::Io(_)
    ));
    let missing = BlobId::from_bytes(&[7; 20]).unwrap();
    assert!(matches!(
        err(BaseSpec::Commit(missing), false),
        ChangeError::Io(_)
    ));
    let limits = Limits {
        entries: 1,
        ..Limits::default()
    };
    t.write_file("u1", b"1\n");
    t.write_file("u2", b"2\n");
    t.write_file("u3", b"3\n");
    assert_eq!(
        source_with(&t, BaseSpec::Head, limits)
            .fetch(&no_cancel())
            .unwrap_err(),
        ChangeError::TooManyEntries
    );
}

// ---- refresh, texts, fingerprint ------------------------------------------------------------

fn hunks_of<'a>(list: &'a ChangeList, path: &str) -> &'a Arc<[DiffHunk]> {
    let f = list
        .files
        .iter()
        .find(|f| &f.path.0[..] == path.as_bytes())
        .unwrap();
    match &f.detail {
        ChangeDetail::Text { hunks, .. } => hunks,
        other => panic!("{other:?}"),
    }
}

#[test]
fn change_source_refresh_shares_unchanged_hunks() {
    let t = clean_repo(&[("a", b"a\n"), ("b", b"b\n"), ("c", b"c\n")]);
    t.write_file("a", b"a1\n");
    t.write_file("b", b"b1\n");
    let s = source(&t);
    let first = checked(&s);
    t.write_file("a", b"a2\n");
    let paths: Arc<[ChangePath]> = Arc::from([ChangePath(Arc::from(&b"a"[..]))]);
    let second = s
        .refresh(&first, &RefreshHint::Paths(paths), &no_cancel())
        .unwrap();
    assert_same(set_of(&s, &second), &second);
    assert!(Arc::ptr_eq(hunks_of(&first, "b"), hunks_of(&second, "b")));
    assert!(!Arc::ptr_eq(hunks_of(&first, "a"), hunks_of(&second, "a")));
    assert_ne!(first.files[0].new, second.files[0].new);

    // A full refresh shares them too; a new file has its own.
    t.write_file("c", b"c1\n");
    let third = s
        .refresh(&second, &RefreshHint::Full, &no_cancel())
        .unwrap();
    assert_same(set_of(&s, &third), &third);
    assert_eq!(third.files.len(), 3);
    assert!(Arc::ptr_eq(hunks_of(&second, "a"), hunks_of(&third, "a")));
    assert!(Arc::ptr_eq(hunks_of(&first, "b"), hunks_of(&third, "b")));
}

/// A path that isn't a valid `RepoPath` makes it a full comparison.
#[test]
fn change_source_refresh_with_an_invalid_path_compares_everything() {
    let t = clean_repo(&[("a", b"a\n"), ("b", b"b\n")]);
    let s = source(&t);
    let first = checked(&s);
    assert!(first.files.is_empty());
    t.write_file("b", b"b1\n");
    let paths: Arc<[ChangePath]> = Arc::from([ChangePath(Arc::from(&b"../a"[..]))]);
    let second = s
        .refresh(&first, &RefreshHint::Paths(paths), &no_cancel())
        .unwrap();
    assert_eq!(second.files.len(), 1);
    assert_eq!(&second.files[0].path.0[..], b"b");
}

/// No handle (a deserialized list) or another source's handle: compared in
/// full as `fetch`, with this source's base. The other source has another
/// base: refreshing its `ChangeSet` would keep that base.
#[test]
fn change_source_refresh_of_a_foreign_list_fetches() {
    let t = clean_repo(&[("a", b"a\n")]);
    t.write_file("a", b"a1\n");
    let c1 = t.commit(t.tree(&[("a", b"zero\n"), ("z", b"z\n")]), &[], "c1");
    let c1 = BlobId::from_bytes(c1.as_bytes()).unwrap();
    let s = source(&t);
    let other = source_with(&t, BaseSpec::Commit(c1), Limits::default());
    let list = checked(&s);
    let bare = Arc::new(ChangeList {
        handle: ListHandle::default(),
        ..(*list).clone()
    });
    let theirs = checked(&other);
    assert_ne!(theirs.base, list.base);
    assert_ne!(theirs.files, list.files);
    for prev in [theirs, bare] {
        let paths: Arc<[ChangePath]> = Arc::from([]);
        let next = s
            .refresh(&prev, &RefreshHint::Paths(paths), &no_cancel())
            .unwrap();
        assert_same(set_of(&s, &next), &next);
        assert_eq!(next.base, list.base);
        assert_eq!(next.files, list.files);
        assert!(other.held(&next).is_none());
    }
    // Texts and fingerprint need this source's handle.
    let bare = ChangeList {
        handle: ListHandle::default(),
        ..(*list).clone()
    };
    assert!(matches!(s.texts(&bare, 0, 1024), Err(ChangeError::Io(_))));
    assert_eq!(s.fingerprint(&bare), None);
    assert_eq!(other.fingerprint(&list), None);
}

#[test]
fn change_source_texts_share_the_compared_arc() {
    let t = clean_repo(&[("a", b"old\n")]);
    t.write_file("a", b"compared\n");
    t.write_file("u", b"untracked\n");
    let s = source(&t);
    let list = checked(&s);
    t.write_file("a", b"later\n");
    let set = set_of(&s, &list);
    let repo = t.repo();
    for (i, f) in list.files.iter().enumerate() {
        let texts = s.texts(&list, i as u32, 1024).unwrap();
        let git = set.texts(&repo, i).unwrap();
        let (SvcText::Text(ours), SideText::Text(theirs)) = (&texts.new, &git.new) else {
            panic!("{:?} {:?}", texts.new, git.new);
        };
        assert!(Arc::ptr_eq(ours, theirs.shared()), "{:?}", f.path);
        match (&texts.old, &git.old) {
            (SvcText::Text(ours), SideText::Text(theirs)) => assert_eq!(&ours[..], theirs.bytes()),
            (SvcText::Absent, SideText::Absent) => {}
            other => panic!("{other:?}"),
        }
    }
    let texts = s.texts(&list, 0, 1024).unwrap();
    assert!(matches!(&texts.old, SvcText::Text(x) if &x[..] == b"old\n"));
    assert!(matches!(&texts.new, SvcText::Text(x) if &x[..] == b"compared\n"));
}

#[test]
fn change_source_texts_over_max_bytes_are_too_large() {
    let t = clean_repo(&[("a", b"0123456789\n"), ("d", b"gone\n")]);
    t.write_file("a", b"0123\n");
    t.remove("d");
    t.write_file("n", b"0123456789abcdef\n");
    let s = source(&t);
    let list = checked(&s);
    let paths: Vec<&[u8]> = list.files.iter().map(|f| &f.path.0[..]).collect();
    assert_eq!(paths, [&b"a"[..], b"d", b"n"]);
    let texts = s.texts(&list, 0, 8).unwrap();
    assert_eq!(texts.old, SvcText::TooLarge { size: 11 });
    assert!(matches!(&texts.new, SvcText::Text(x) if &x[..] == b"0123\n"));
    let texts = s.texts(&list, 1, 8).unwrap();
    assert_eq!(
        (texts.old, texts.new),
        (SvcText::Text(Arc::from(&b"gone\n"[..])), SvcText::Absent)
    );
    let texts = s.texts(&list, 2, 8).unwrap();
    assert_eq!(
        (texts.old, texts.new),
        (SvcText::Absent, SvcText::TooLarge { size: 17 })
    );
    assert!(matches!(s.texts(&list, 3, 8), Err(ChangeError::Io(_))));
}

#[test]
fn change_source_not_retained_text_is_not_reread() {
    let t = clean_repo(&[("keep", b"k\n")]);
    t.write_file("a.txt", b"aaaa\n");
    t.write_file("b.txt", b"bbbbbbbbbbbbbbbb\n");
    let limits = Limits {
        retained_bytes: 10,
        ..Limits::default()
    };
    let s = source_with(&t, BaseSpec::Head, limits);
    let list = checked(&s);
    assert_eq!(s.texts(&list, 1, 1024).unwrap().new, SvcText::NotRetained);
}

#[test]
fn change_source_fingerprint_is_the_change_sets() {
    let t = clean_repo(&[("a", b"a\n")]);
    t.write_file("a", b"a1\n");
    let s = source(&t);
    let first = checked(&s);
    let fp = s.fingerprint(&first).unwrap();
    assert_eq!(&fp, set_of(&s, &first).fingerprint().as_bytes());
    t.write_file("a", b"a2\n");
    let second = s.refresh(&first, &RefreshHint::Full, &no_cancel()).unwrap();
    let fp2 = s.fingerprint(&second).unwrap();
    assert_eq!(&fp2, set_of(&s, &second).fingerprint().as_bytes());
    assert_ne!(fp, fp2);
}

/// The list's handle keeps the `ChangeSet`, and with it the worktree
/// contents, alive; dropping the list drops them.
#[test]
fn change_source_list_owns_its_change_set() {
    let t = clean_repo(&[("a", b"a\n")]);
    t.write_file("a", b"a1\n");
    let s = source(&t);
    let list = checked(&s);
    let weak = Arc::downgrade(set_of(&s, &list));
    let SvcText::Text(bytes) = s.texts(&list, 0, 1024).unwrap().new else {
        panic!("no text");
    };
    let weak_bytes = Arc::downgrade(&bytes);
    drop(bytes);
    assert!(weak.upgrade().is_some() && weak_bytes.upgrade().is_some());
    drop(list);
    assert!(weak.upgrade().is_none());
    assert!(weak_bytes.upgrade().is_none());
}
