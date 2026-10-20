use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::compare::{STAT_CHUNK, hook};
use crate::testing::{IndexEntry, TestHead, TestRepo};
use crate::*;

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

fn main_worktree(t: &TestRepo) -> Worktree {
    t.repo().worktree(&WorktreeId::Main).unwrap()
}

fn rp(path: &str) -> RepoPath {
    RepoPath::new(path.as_bytes()).unwrap()
}

fn short(oid: Oid) -> String {
    oid.to_hex()[..7].to_owned()
}

/// The short OID of `content` as a SHA-1 blob.
fn b7(content: &[u8]) -> String {
    short(Oid::for_blob(HashKind::Sha1, content))
}

/// Every file under the git directory: path, size and content hash.
fn git_dir_state(dir: &Path) -> Vec<(String, u64, Oid)> {
    fn visit(base: &Path, dir: &Path, out: &mut Vec<(String, u64, Oid)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            let rel = path.strip_prefix(base).unwrap().display().to_string();
            if meta.is_dir() {
                visit(base, &path, out);
            } else {
                let content = std::fs::read(&path).unwrap_or_default();
                out.push((rel, meta.len(), Oid::for_blob(HashKind::Sha1, &content)));
            }
        }
    }
    let mut out = Vec::new();
    visit(dir, dir, &mut out);
    out.sort();
    out
}

/// Runs `f` and checks that nothing under the git directory changed
/// (docs/design/git.md「测试」只读).
fn read_only<T>(t: &TestRepo, f: impl FnOnce() -> T) -> T {
    let before = git_dir_state(t.common_dir());
    let index_mtime = std::fs::metadata(t.common_dir().join("index"))
        .and_then(|m| m.modified())
        .ok();
    let out = f();
    assert_eq!(
        git_dir_state(t.common_dir()),
        before,
        "the git directory changed"
    );
    let after = std::fs::metadata(t.common_dir().join("index"))
        .and_then(|m| m.modified())
        .ok();
    assert_eq!(after, index_mtime, "the index was rewritten");
    out
}

fn changes_with(t: &TestRepo, base: Base, limits: &Limits) -> Arc<ChangeSet> {
    read_only(t, || {
        main_worktree(t)
            .changes(base, limits, &no_cancel())
            .unwrap()
    })
}

fn changes(t: &TestRepo) -> Arc<ChangeSet> {
    changes_with(t, Base::Head, &Limits::default())
}

fn mode(m: Mode) -> &'static str {
    match m {
        Mode::File => "f",
        Mode::Executable => "x",
        Mode::Symlink => "l",
        Mode::Gitlink => "g",
        Mode::Special => "s",
    }
}

fn side(s: &Option<Side>) -> String {
    let Some(s) = s else {
        return "-".into();
    };
    let content = match &s.content {
        Content::Blob(oid) => short(*oid),
        Content::Unhashed { size, .. } => format!("unhashed({size})"),
        Content::Unavailable(u) => match u {
            Unavailable::Filtered {
                raw: RawIdentity::Sha256(_),
            } => "filtered".into(),
            Unavailable::Filtered {
                raw: RawIdentity::Stat { size, .. },
            } => format!("filtered-stat({size})"),
            Unavailable::UnsupportedType => "unsupported".into(),
            Unavailable::PathThroughSymlink => "through-symlink".into(),
            Unavailable::NestedRepository => "nested-repository".into(),
            Unavailable::Missing => "missing".into(),
            Unavailable::Unreadable => "unreadable".into(),
            Unavailable::NotRetained { oid } => format!("not-retained:{}", short(*oid)),
        },
    };
    format!("{}:{content}", mode(s.mode))
}

fn line(c: &Change) -> String {
    let status = match &c.status {
        Status::Added => "A".to_owned(),
        Status::Deleted => "D".to_owned(),
        Status::Modified => "M".to_owned(),
        Status::TypeChanged => "T".to_owned(),
        Status::Conflicted => "C".to_owned(),
        Status::Renamed { from, similarity } => format!(
            "R{similarity}({})",
            String::from_utf8_lossy(from.as_bytes())
        ),
    };
    let detail = match &c.detail {
        Detail::Text(d) => format!("text(+{},-{})", d.added, d.removed),
        Detail::Truncated { added, removed } => format!("truncated(+{added},-{removed})"),
        Detail::Binary { .. } => "binary".into(),
        Detail::TooLarge => "too-large".into(),
        Detail::Listed => "listed".into(),
        Detail::ModeOnly => "mode-only".into(),
        Detail::StagedOnly => "staged-only".into(),
        Detail::NoDiff => "no-diff".into(),
    };
    let mut out = format!(
        "{status} {}{} {} -> {} {detail}",
        String::from_utf8_lossy(c.path.as_bytes()),
        if c.untracked { " (untracked)" } else { "" },
        side(&c.old),
        side(&c.new),
    );
    if let Some(s) = c.staged {
        out.push_str(&format!(" staged={}:{}", mode(s.mode), short(s.oid)));
    }
    if c.generated {
        out.push_str(" generated");
    }
    out
}

fn lines(set: &ChangeSet) -> Vec<String> {
    set.files().iter().map(line).collect()
}

/// HEAD's tree is `files`.
fn commit_files(t: &TestRepo, files: &[(&str, &[u8])]) -> Oid {
    let tree = t.tree(files);
    let commit = t.commit(tree, &[], "base");
    t.set_ref("refs/heads/main", commit);
    commit
}

/// Writes the worktree files, backdates them (so the index entries aren't
/// racy), commits them as HEAD and stages them with their stat: a clean
/// worktree.
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

// ---- worktree_* -------------------------------------------------------------

#[test]
fn worktree_clean_reads_nothing() {
    let t = clean_repo(&[("a.txt", b"a\n"), ("dir/b.txt", b"b\n"), ("c", b"c\n")]);
    let before = hook::reads();
    let set = changes(&t);
    assert_eq!(lines(&set), Vec::<String>::new());
    assert_eq!(hook::reads(), before, "stat-clean files aren't read");
    assert_eq!(set.target(), &Target::Worktree(WorktreeId::Main));
    assert_eq!(set.totals(), &Totals::default());
}

// ---- worktree_table_*: one test per row of the table in「工作区比较」 --------

/// What the worktree holds at `p`.
#[derive(Clone, Copy)]
enum W<'a> {
    Absent,
    Content(&'a [u8]),
    /// A socket: not a file.
    Socket,
}

fn put(t: &TestRepo, path: &str, index: Option<&[u8]>, w: W<'_>) {
    match w {
        W::Absent => {}
        W::Content(content) => {
            t.write_file(path, content);
            if Some(content) == index {
                // Back to the stat the index recorded.
                t.backdate(path);
            }
        }
        W::Socket => {
            std::os::unix::net::UnixListener::bind(t.root().join(path)).unwrap();
        }
    }
}

/// A repository whose HEAD has `p` = `base`, whose index has `p` = `index`
/// (with the stat `git add` records) and whose worktree has `worktree` at
/// `p`. Another clean file keeps HEAD and the index non-empty.
fn table_repo(base: Option<&[u8]>, index: Option<&[u8]>, worktree: W<'_>) -> TestRepo {
    let t = TestRepo::new();
    t.write_file("other", b"other\n");
    t.backdate("other");
    let mut files: Vec<(&str, &[u8])> = vec![("other", b"other\n")];
    if let Some(base) = base {
        files.push(("p", base));
    }
    commit_files(&t, &files);
    let mut entries = vec![t.add("other")];
    if let Some(index) = index {
        t.write_file("p", index);
        t.backdate("p");
        entries.push(t.add("p"));
        t.remove("p");
    }
    t.write_index(t.common_dir(), &entries);
    put(&t, "p", index, worktree);
    t
}

fn table(base: Option<&[u8]>, index: Option<&[u8]>, worktree: W<'_>) -> Vec<String> {
    lines(&changes(&table_repo(base, index, worktree)))
}

const A: &[u8] = b"base\n";
const S: &[u8] = b"staged\n";
const C: &[u8] = b"changed\n";

#[test]
fn worktree_table_untracked_is_listed_by_the_walk() {
    assert_eq!(
        table(None, None, W::Content(C)),
        [format!("A p (untracked) - -> f:{} text(+1,-0)", b7(C))]
    );
}

#[test]
fn worktree_table_added_then_removed_is_staged_only() {
    // `git add p; rm p`
    assert_eq!(
        table(None, Some(S), W::Absent),
        [format!(
            "A p - -> f:{s} staged-only staged=f:{s}",
            s = b7(S)
        )]
    );
}

#[test]
fn worktree_table_added_as_staged() {
    assert_eq!(
        table(None, Some(S), W::Content(S)),
        [format!("A p - -> f:{} text(+1,-0)", b7(S))]
    );
}

#[test]
fn worktree_table_added_and_changed_again() {
    assert_eq!(
        table(None, Some(S), W::Content(C)),
        [format!(
            "A p - -> f:{} text(+1,-0) staged=f:{}",
            b7(C),
            b7(S)
        )]
    );
}

#[test]
fn worktree_table_added_and_replaced_by_a_socket() {
    assert_eq!(
        table(None, Some(S), W::Socket),
        [format!("A p - -> s:unsupported no-diff staged=f:{}", b7(S))]
    );
}

#[test]
fn worktree_table_removed_from_the_index_and_the_worktree() {
    assert_eq!(
        table(Some(A), None, W::Absent),
        [format!("D p f:{} -> - text(+0,-1)", b7(A))]
    );
}

#[test]
fn worktree_table_rm_cached_gives_a_deletion_and_an_untracked_addition() {
    // `git rm --cached p` with the file still there.
    assert_eq!(
        table(Some(A), None, W::Content(A)),
        [
            format!("D p f:{} -> - text(+0,-1)", b7(A)),
            format!("A p (untracked) - -> f:{} text(+1,-0)", b7(A)),
        ]
    );
}

#[test]
fn worktree_table_rm_cached_of_an_ignored_path_is_only_a_deletion() {
    let t = table_repo(Some(A), None, W::Content(A));
    std::fs::create_dir_all(t.common_dir().join("info")).unwrap();
    std::fs::write(t.common_dir().join("info/exclude"), b"p\n").unwrap();
    assert_eq!(
        lines(&changes(&t)),
        [format!("D p f:{} -> - text(+0,-1)", b7(A))]
    );
}

#[test]
fn worktree_table_unstaged_rm_is_a_deletion() {
    assert_eq!(
        table(Some(A), Some(A), W::Absent),
        [format!(
            "D p f:{a} -> - text(+0,-1) staged=f:{a}",
            a = b7(A)
        )]
    );
}

#[test]
fn worktree_table_unchanged_is_not_reported() {
    assert_eq!(table(Some(A), Some(A), W::Content(A)), Vec::<String>::new());
}

#[test]
fn worktree_table_unstaged_change() {
    assert_eq!(
        table(Some(A), Some(A), W::Content(C)),
        [format!(
            "M p f:{a} -> f:{} text(+1,-1) staged=f:{a}",
            b7(C),
            a = b7(A)
        )]
    );
}

#[test]
fn worktree_table_tracked_file_replaced_by_a_socket() {
    assert_eq!(
        table(Some(A), Some(A), W::Socket),
        [format!(
            "T p f:{a} -> s:unsupported no-diff staged=f:{a}",
            a = b7(A)
        )]
    );
}

#[test]
fn worktree_table_staged_change_then_removed() {
    assert_eq!(
        table(Some(A), Some(S), W::Absent),
        [format!(
            "D p f:{} -> - text(+0,-1) staged=f:{}",
            b7(A),
            b7(S)
        )]
    );
}

#[test]
fn worktree_table_staged_change() {
    assert_eq!(
        table(Some(A), Some(S), W::Content(S)),
        [format!("M p f:{} -> f:{} text(+1,-1)", b7(A), b7(S))]
    );
}

#[test]
fn worktree_table_staged_change_reverted_in_the_worktree_is_staged_only() {
    assert_eq!(
        table(Some(A), Some(S), W::Content(A)),
        [format!(
            "M p f:{} -> f:{s} staged-only staged=f:{s}",
            b7(A),
            s = b7(S)
        )]
    );
}

#[test]
fn worktree_table_staged_change_and_changed_again() {
    assert_eq!(
        table(Some(A), Some(S), W::Content(C)),
        [format!(
            "M p f:{} -> f:{} text(+1,-1) staged=f:{}",
            b7(A),
            b7(C),
            b7(S)
        )]
    );
}

#[test]
fn worktree_table_staged_change_then_replaced_by_a_socket() {
    assert_eq!(
        table(Some(A), Some(S), W::Socket),
        [format!(
            "T p f:{} -> s:unsupported no-diff staged=f:{}",
            b7(A),
            b7(S)
        )]
    );
}

/// Every row at once, on different paths: the structured output.
#[test]
fn worktree_table_snapshot() {
    let t = TestRepo::new();
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
    let mut base = Vec::new();
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
    commit_files(&t, &base);
    t.write_index(t.common_dir(), &entries);
    for (path, _, i, w) in &rows {
        put(&t, path, *i, *w);
    }
    insta::assert_snapshot!(lines(&changes(&t)).join("\n"));
}

// ---- untracked files, ignore rules, special entries --------------------------

/// An empty repository with its own `.git` at `path`; with a commit when
/// `commit` (its OID is returned).
fn nested_repo(path: &Path, commit: bool) -> Option<Oid> {
    let repo = gix::ThreadSafeRepository::init_opts(
        path,
        gix::create::Kind::WithWorktree,
        Default::default(),
        gix::open::Options::isolated(),
    )
    .unwrap()
    .to_thread_local();
    if !commit {
        return None;
    }
    let tree = repo
        .write_object(gix::objs::Tree::empty())
        .unwrap()
        .detach();
    let signature = gix::actor::Signature {
        name: "Test".into(),
        email: "test@example.com".into(),
        time: gix::date::Time {
            seconds: 1_700_000_000,
            offset: 0,
        },
    };
    let commit = gix::objs::Commit {
        tree,
        parents: Default::default(),
        author: signature.clone(),
        committer: signature,
        encoding: None,
        message: "sub".into(),
        extra_headers: Vec::new(),
    };
    let id = repo.write_object(&commit).unwrap().detach();
    std::fs::write(repo.git_dir().join("refs/heads/main"), format!("{id}\n")).unwrap();
    Some(Oid::from_gix(&id))
}

#[test]
fn worktree_untracked_files_follow_the_ignore_rules() {
    let t = clean_repo(&[(".gitignore", b"*.log\nbuild/\n"), ("src/lib.rs", b"lib\n")]);
    std::fs::create_dir_all(t.common_dir().join("info")).unwrap();
    std::fs::write(t.common_dir().join("info/exclude"), b"secret\n").unwrap();
    t.write_file("notes.md", b"notes\n");
    t.write_file("app.log", b"log\n");
    t.write_file("build/out.o", b"\0obj");
    t.write_file("secret", b"key\n");
    t.write_file("src/new.rs", b"new\n");
    t.write_file("src/deep/er/x.rs", b"x\n");
    std::os::unix::net::UnixListener::bind(t.root().join("sock")).unwrap();
    nested_repo(&t.root().join("nested"), false);
    t.write_file("nested/inner.txt", b"inner\n");
    #[cfg(unix)]
    t.symlink("link", "src/lib.rs");
    let set = changes(&t);
    insta::assert_snapshot!(lines(&set).join("\n"));
    assert!(set.files().iter().all(|c| c.untracked));
    assert!(!set.totals().untracked_capped);
}

#[test]
#[cfg(unix)]
fn worktree_symlinked_gitignore_outside_the_worktree_is_not_read() {
    // gix's own walk follows an untracked `.gitignore` symbolic link; the
    // rules must come only from regular files read through `SafeRoot`.
    let t = clean_repo(&[("a", b"a\n")]);
    let outside = t.base().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("ignore"), b"secret.txt\nsub/\n").unwrap();
    t.symlink(".gitignore", outside.join("ignore").to_str().unwrap());
    t.write_file("secret.txt", b"s\n");
    t.write_file("sub/x", b"x\n");
    let l = lines(&changes(&t));
    let paths: Vec<&str> = l.iter().map(|l| l.split(' ').nth(1).unwrap()).collect();
    assert_eq!(paths, [".gitignore", "secret.txt", "sub/x"], "{l:#?}");
}

#[test]
#[cfg(unix)]
fn worktree_directory_through_a_symlink_is_not_walked() {
    // The outside directory's `.gitignore` would hide `keep.txt` and its
    // other files would show up if the link were followed.
    let t = clean_repo(&[("a", b"a\n")]);
    let outside = t.base().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join(".gitignore"), b"*\n").unwrap();
    std::fs::write(outside.join("OUTSIDE.txt"), b"o\n").unwrap();
    t.symlink("d", outside.to_str().unwrap());
    t.write_file("keep.txt", b"k\n");
    let l = lines(&changes(&t));
    let paths: Vec<&str> = l.iter().map(|l| l.split(' ').nth(1).unwrap()).collect();
    assert_eq!(paths, ["d", "keep.txt"], "{l:#?}");
}

#[test]
#[cfg(unix)]
fn worktree_race_directory_swapped_for_a_symlink_never_escapes() {
    use std::sync::atomic::Ordering;

    let t = clean_repo(&[("a", b"a\n")]);
    let outside = t.base().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    // Would ignore `inside.txt` and add `OUTSIDE.txt` if ever read.
    std::fs::write(outside.join(".gitignore"), b"inside.txt\n").unwrap();
    std::fs::write(outside.join("OUTSIDE.txt"), b"o\n").unwrap();
    t.write_file("w/inside.txt", b"i\n");
    t.write_file("w/deep/more.txt", b"m\n");
    std::os::unix::fs::symlink(&outside, t.root().join("link")).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let swapper = {
        let stop = Arc::clone(&stop);
        let root = t.root().to_owned();
        std::thread::spawn(move || {
            let (w, held, link) = (root.join("w"), root.join("held"), root.join("link"));
            let mut swaps = 0u64;
            while !stop.load(Ordering::Relaxed) {
                std::fs::rename(&w, &held).unwrap();
                std::fs::rename(&link, &w).unwrap();
                std::fs::rename(&w, &link).unwrap();
                std::fs::rename(&held, &w).unwrap();
                swaps += 1;
            }
            swaps
        })
    };
    let wt = main_worktree(&t);
    let cancel = no_cancel();
    let outside_blob = Oid::for_blob(HashKind::Sha1, b"o\n");
    for _ in 0..300 {
        let set = wt.changes(Base::Head, &Limits::default(), &cancel).unwrap();
        for c in set.files() {
            let path = c.path.as_bytes();
            assert!(!path.ends_with(b"OUTSIDE.txt"), "{c:?}");
            assert_ne!(
                c.new.as_ref().and_then(|s| s.content.oid()),
                Some(outside_blob)
            );
        }
    }
    stop.store(true, Ordering::Relaxed);
    assert!(swapper.join().unwrap() > 0);
}

#[test]
fn worktree_untracked_limit_caps_the_list() {
    let t = clean_repo(&[("a", b"a\n")]);
    for name in ["u1", "u2", "u3"] {
        t.write_file(name, b"u\n");
    }
    let limits = Limits {
        untracked: 2,
        ..Limits::default()
    };
    let set = changes_with(&t, Base::Head, &limits);
    assert_eq!(set.files().len(), 2);
    assert!(set.totals().untracked_capped);
}

#[test]
fn worktree_tracked_files_never_show_as_untracked() {
    let t = clean_repo(&[("a", b"a\n"), ("d/b", b"b\n"), ("d/e/c", b"c\n")]);
    // Racy and touched: read and compared, still unchanged.
    t.write_file("d/b", b"b\n");
    assert_eq!(lines(&changes(&t)), Vec::<String>::new());
}

#[test]
#[cfg(target_os = "linux")]
fn worktree_fifo_is_unsupported_and_does_not_block() {
    use std::time::{Duration, Instant};

    let t = clean_repo(&[("pipe", b"was a file\n")]);
    t.remove("pipe");
    let mkfifo = |path: &Path| {
        rustix::fs::mknodat(
            rustix::fs::CWD,
            path,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o644),
            0,
        )
        .unwrap();
    };
    mkfifo(&t.root().join("pipe"));
    mkfifo(&t.root().join("new-pipe"));
    let start = Instant::now();
    let set = changes(&t);
    assert!(start.elapsed() < Duration::from_secs(5), "a FIFO blocked");
    let a = b7(b"was a file\n");
    assert_eq!(
        lines(&set),
        [
            "A new-pipe (untracked) - -> s:unsupported no-diff".to_owned(),
            format!("T pipe f:{a} -> s:unsupported no-diff staged=f:{a}"),
        ]
    );
}

#[test]
#[cfg(unix)]
fn worktree_path_through_a_symlink_is_unavailable() {
    let t = clean_repo(&[("dir/f", b"f\n")]);
    // A directory outside the worktree with the same file name.
    let outside = t.base().join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("f"), b"outside\n").unwrap();
    t.remove("dir");
    let target = outside.to_str().unwrap();
    t.symlink("dir", target);
    let set = changes(&t);
    // `dir/f` isn't read through the link; the link itself is untracked.
    assert_eq!(
        lines(&set),
        [
            format!(
                "A dir (untracked) - -> l:{} text(+1,-0)",
                b7(target.as_bytes())
            ),
            format!("M dir/f f:{} -> f:through-symlink no-diff", b7(b"f\n")),
        ]
    );
}

#[test]
fn worktree_nested_repository_is_listed_once() {
    let t = clean_repo(&[("a", b"a\n")]);
    nested_repo(&t.root().join("vendor/other"), true);
    t.write_file("vendor/other/x", b"x\n");
    assert_eq!(
        lines(&changes(&t)),
        ["A vendor/other (untracked) - -> s:nested-repository no-diff"]
    );
}

#[test]
fn worktree_file_over_the_read_limit_is_unhashed() {
    let t = clean_repo(&[("small", b"small\n")]);
    t.write_file("big", &[b'x'; 100]);
    let limits = Limits {
        read_bytes: 64,
        ..Limits::default()
    };
    let set = changes_with(&t, Base::Head, &limits);
    assert_eq!(
        lines(&set),
        ["A big (untracked) - -> f:unhashed(100) no-diff"]
    );
    let Some(Content::Unhashed {
        mtime_ns, inode, ..
    }) = set.files()[0].new.as_ref().map(|s| s.content.clone())
    else {
        panic!("not unhashed");
    };
    let meta = std::fs::metadata(t.root().join("big")).unwrap();
    use std::os::unix::fs::MetadataExt;
    assert_eq!(inode, meta.ino());
    assert_eq!(
        mtime_ns,
        i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec())
    );
}

// ---- stat, racy-clean, conversions --------------------------------------------

#[test]
fn worktree_racy_clean_entry_compares_content() {
    // Same size, same mtime, ctime not trusted: only the content differs.
    let t = TestRepo::new();
    t.append_config("[core]\n\ttrustctime = false\n");
    t.write_file("a", b"aaaa\n");
    commit_files(&t, &[("a", b"aaaa\n")]);
    let entry = t.add("a");
    t.write_index(t.common_dir(), &[entry]);
    let mtime = std::fs::metadata(t.root().join("a"))
        .unwrap()
        .modified()
        .unwrap();
    t.write_file("a", b"bbbb\n");
    t.set_mtime("a", mtime);
    // Pin the index file's mtime to the entry's, so a second boundary between
    // writing `a` and writing the index cannot make the entry look clean.
    std::fs::File::options()
        .write(true)
        .open(t.common_dir().join("index"))
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    // The entry's mtime is not before the index file's: racy, so it is read.
    let set = changes(&t);
    assert_eq!(
        lines(&set),
        [format!(
            "M a f:{a} -> f:{} text(+1,-1) staged=f:{a}",
            b7(b"bbbb\n"),
            a = b7(b"aaaa\n")
        )]
    );
}

#[test]
fn worktree_matching_stat_skips_the_file() {
    // The same edit as above, but the entry is older than the index: git
    // trusts the stat and so does this crate (docs/design/git.md 第 2 步).
    let t = TestRepo::new();
    t.append_config("[core]\n\ttrustctime = false\n");
    t.write_file("a", b"aaaa\n");
    t.backdate("a");
    commit_files(&t, &[("a", b"aaaa\n")]);
    let entry = t.add("a");
    t.write_index(t.common_dir(), &[entry]);
    let mtime = std::fs::metadata(t.root().join("a"))
        .unwrap()
        .modified()
        .unwrap();
    t.write_file("a", b"bbbb\n");
    t.set_mtime("a", mtime);
    let before = hook::reads();
    assert_eq!(lines(&changes(&t)), Vec::<String>::new());
    assert_eq!(hook::reads(), before);
}

#[test]
fn worktree_touched_file_with_the_same_content_is_not_a_change() {
    let t = clean_repo(&[("a", b"a\n")]);
    t.write_file("a", b"a\n");
    let before = hook::reads();
    assert_eq!(lines(&changes(&t)), Vec::<String>::new());
    assert_eq!(hook::reads(), before + 1, "the stat differs: read once");
}

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// `git ls-files -s -z`: path → (mode, OID).
fn ls_files(path: &Path) -> std::collections::BTreeMap<String, (String, String)> {
    let text = std::fs::read(path).unwrap();
    text.split(|&b| b == 0)
        .filter(|l| !l.is_empty())
        .map(|l| {
            let l = std::str::from_utf8(l).unwrap();
            let (meta, path) = l.split_once('\t').unwrap();
            let fields: Vec<&str> = meta.split(' ').collect();
            (
                path.to_owned(),
                (fields[0].to_owned(), fields[1].to_owned()),
            )
        })
        .collect()
}

#[test]
fn worktree_conversions_match_git() {
    let dir = fixture("worktree_conversions");
    for autocrlf in ["false", "true", "input"] {
        let t = TestRepo::new();
        t.append_config(&format!("[core]\n\tautocrlf = {autocrlf}\n"));
        let paths = t.import_worktree_fixture(&dir);
        let set = changes(&t);
        let expected = ls_files(&dir.join(format!("expected/autocrlf={autocrlf}/ls-files")));
        assert_eq!(expected.len(), paths.len());
        let got: std::collections::BTreeMap<String, (String, String)> = set
            .files()
            .iter()
            .map(|c| {
                assert!(c.untracked);
                let new = c.new.as_ref().unwrap();
                let oid = new.content.oid().unwrap();
                (
                    String::from_utf8(c.path.as_bytes().to_vec()).unwrap(),
                    ("100644".to_owned(), oid.to_hex()),
                )
            })
            .collect();
        assert_eq!(got, expected, "core.autocrlf = {autocrlf}");
    }
}

/// Byte offsets index the canonical content (what `git add` would store and
/// what `texts` returns), not the raw worktree bytes: under
/// `core.autocrlf = true` a CRLF worktree file is LF in its blob.
#[test]
fn worktree_byte_offsets_index_the_canonical_content() {
    let t = clean_repo(&[("a.txt", b"one\ntwo\nthree\n")]);
    t.append_config("[core]\n\tautocrlf = true\n");
    let raw: &[u8] = b"one\r\nTWO\r\nthree\r\n";
    t.write_file("a.txt", raw);
    let set = changes(&t);
    let change = &set.files()[0];
    let Detail::Text(diff) = &change.detail else {
        panic!("{:?}", change.detail);
    };
    let canonical: &[u8] = b"one\nTWO\nthree\n";
    assert_eq!(
        change.new.as_ref().unwrap().content,
        Content::Blob(Oid::for_blob(HashKind::Sha1, canonical))
    );
    let h = &diff.hunks[0];
    assert_eq!((h.old.clone(), h.new.clone()), (1..2, 1..2));
    // "one\n" is 4 bytes; in the raw file it would be 5.
    assert_eq!((h.old_byte, h.new_byte), (4, 4));
    let texts = set.texts(&t.repo(), 0).unwrap();
    let SideText::Text(new) = &texts.new else {
        panic!("{:?}", texts.new);
    };
    assert_eq!(new.bytes(), canonical);
    crate::rows::tests::assert_offsets(diff, &texts, "a.txt");

    // The conversion fixture under each `core.autocrlf`: CRLF kept by
    // `-text`, mixed endings, a lone CR, UTF-16 converted to UTF-8.
    let dir = fixture("worktree_conversions");
    for autocrlf in ["false", "true", "input"] {
        let t = TestRepo::new();
        t.append_config(&format!("[core]\n\tautocrlf = {autocrlf}\n"));
        t.import_worktree_fixture(&dir);
        let set = changes(&t);
        let mut checked = 0;
        for (i, change) in set.files().iter().enumerate() {
            let Detail::Text(diff) = &change.detail else {
                continue;
            };
            let texts = set.texts(&t.repo(), i).unwrap();
            let what = format!("{autocrlf} {:?}", change.path);
            crate::rows::tests::assert_offsets(diff, &texts, &what);
            checked += 1;
        }
        assert!(checked >= 10, "{checked} files under autocrlf = {autocrlf}");
    }
}

#[test]
fn worktree_attribute_change_recomputes_stat_clean_files() {
    let t = clean_repo(&[("a.txt", b"one\r\ntwo\r\n")]);
    let wt = main_worktree(&t);
    let cancel = no_cancel();
    let first = wt.changes(Base::Head, &Limits::default(), &cancel).unwrap();
    assert_eq!(lines(&first), Vec::<String>::new());
    // Only `.gitattributes` changes; a.txt keeps its stat.
    t.write_file(".gitattributes", b"*.txt text eol=lf\n");
    let set = read_only(&t, || wt.refresh(&first, Refresh::Full, &cancel).unwrap());
    let crlf = b7(b"one\r\ntwo\r\n");
    assert_eq!(
        lines(&set),
        [
            format!(
                "A .gitattributes (untracked) - -> f:{} text(+1,-0)",
                b7(b"*.txt text eol=lf\n")
            ),
            format!(
                "M a.txt f:{crlf} -> f:{} text(+2,-2) staged=f:{crlf}",
                b7(b"one\ntwo\n")
            ),
        ]
    );
    // And back: without the rule the stat counts again.
    t.remove(".gitattributes");
    let set = wt.refresh(&set, Refresh::Full, &cancel).unwrap();
    assert_eq!(lines(&set), Vec::<String>::new());
}

// ---- filters ------------------------------------------------------------------

const LFS_CONFIG: &str = "[filter \"lfs\"]\n\tclean = git-lfs clean -- %f\n\tsmudge = git-lfs smudge -- %f\n\tprocess = git-lfs filter-process\n\trequired = true\n";

#[test]
fn worktree_lfs_files_are_filtered() {
    let t = TestRepo::new();
    t.append_config(LFS_CONFIG);
    let pointer =
        |n: u8| format!("version https://git-lfs.github.com/spec/v1\noid sha256:{n:064}\nsize 9\n");
    let attrs: &[u8] = b"*.bin filter=lfs diff=lfs merge=lfs -text\n";
    t.write_file(".gitattributes", attrs);
    t.backdate(".gitattributes");
    for name in ["same.bin", "staged.bin", "edited.bin", "gone.bin"] {
        t.write_file(name, b"real data");
        t.backdate(name);
    }
    let p1 = pointer(1);
    let p2 = pointer(2);
    commit_files(
        &t,
        &[
            (".gitattributes", attrs),
            ("same.bin", p1.as_bytes()),
            ("staged.bin", p1.as_bytes()),
            ("edited.bin", p1.as_bytes()),
            ("gone.bin", p1.as_bytes()),
        ],
    );
    let entry = |name: &str, pointer: &str| IndexEntry {
        oid: t.blob(pointer.as_bytes()),
        ..t.add(name)
    };
    t.write_index(
        t.common_dir(),
        &[
            t.add(".gitattributes"),
            entry("same.bin", &p1),
            entry("staged.bin", &p2),
            entry("edited.bin", &p1),
            entry("gone.bin", &p1),
        ],
    );
    t.write_file("edited.bin", b"new data!!");
    t.remove("gone.bin");
    t.write_file("new.bin", b"untracked");
    let set = changes(&t);
    let (b1, b2) = (b7(p1.as_bytes()), b7(p2.as_bytes()));
    assert_eq!(
        lines(&set),
        [
            format!("M edited.bin f:{b1} -> f:filtered no-diff"),
            format!("D gone.bin f:{b1} -> - text(+0,-3) staged=f:{b1}"),
            "A new.bin (untracked) - -> f:filtered no-diff".to_owned(),
            // The index has an update the worktree may or may not match.
            format!("M staged.bin f:{b1} -> f:filtered no-diff"),
        ]
    );
    let raw = |name: &str| match &set
        .files()
        .iter()
        .find(|c| c.path.as_bytes() == name.as_bytes())
        .unwrap()
        .new
        .as_ref()
        .unwrap()
        .content
    {
        Content::Unavailable(Unavailable::Filtered {
            raw: RawIdentity::Sha256(d),
        }) => *d,
        other => panic!("{other:?}"),
    };
    let sha = |b: &[u8]| {
        let mut h = gix::hash::hasher(gix::hash::Kind::Sha256);
        h.update(b);
        h.try_finalize().unwrap().as_bytes().to_vec()
    };
    assert_eq!(raw("new.bin").to_vec(), sha(b"untracked"));
    assert_eq!(raw("edited.bin").to_vec(), sha(b"new data!!"));
    let _ = b2;
}

// ---- moves, conflicts, directories, submodules --------------------------------

#[test]
fn worktree_git_mv_then_recreate_is_a_rename_and_an_untracked_addition() {
    let content: &[u8] = b"line 1\nline 2\nline 3\n";
    let t = TestRepo::new();
    commit_files(&t, &[("a", content)]);
    t.write_file("b", content);
    t.backdate("b");
    t.write_index(t.common_dir(), &[t.add("b")]);
    t.write_file("a", b"new a\n");
    assert_eq!(
        lines(&changes(&t)),
        [
            format!("A a (untracked) - -> f:{} text(+1,-0)", b7(b"new a\n")),
            format!("R100(a) b f:{c} -> f:{c} text(+0,-0)", c = b7(content)),
        ]
    );
}

#[test]
fn worktree_staged_mv_then_removed_is_not_a_rename() {
    // `git mv a b; rm b`: the staged rename must not hide that `b` is gone.
    let content: &[u8] = b"line 1\nline 2\nline 3\n";
    let t = TestRepo::new();
    commit_files(&t, &[("a", content)]);
    t.write_file("b", content);
    t.write_index(t.common_dir(), &[t.add("b")]);
    t.remove("b");
    let c = b7(content);
    assert_eq!(
        lines(&changes(&t)),
        [
            format!("D a f:{c} -> - text(+0,-3)"),
            format!("A b - -> f:{c} staged-only staged=f:{c}"),
        ]
    );
}

#[test]
#[cfg(unix)]
fn worktree_absorbed_submodule_symlinks_under_modules_are_not_followed() {
    let t = TestRepo::new();
    let outside = t.base().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    // A HEAD that names a commit, outside the repository.
    let foreign = Oid::for_blob(HashKind::Sha1, b"foreign");
    std::fs::write(outside.join("HEAD"), format!("{}\n", foreign.to_hex())).unwrap();
    let modules = t.common_dir().join("modules");
    // `modules/linkhead`: a real git directory whose HEAD is a link outside.
    std::fs::create_dir_all(modules.join("linkhead")).unwrap();
    std::os::unix::fs::symlink(outside.join("HEAD"), modules.join("linkhead/HEAD")).unwrap();
    // `modules/linkdir`: the git directory itself is a link outside.
    std::os::unix::fs::symlink(&outside, modules.join("linkdir")).unwrap();
    t.write_file("a/.git", b"gitdir: ../.git/modules/linkhead\n");
    t.write_file("b/.git", b"gitdir: ../.git/modules/linkdir\n");
    let recorded = t.blob(b"recorded");
    let tree = t.tree_entries(&[
        ("a", Mode::Gitlink, recorded),
        ("b", Mode::Gitlink, recorded),
    ]);
    let commit = t.commit(tree, &[], "c");
    t.set_ref("refs/heads/main", commit);
    t.write_index(
        t.common_dir(),
        &[
            IndexEntry::new("a", Mode::Gitlink, recorded),
            IndexEntry::new("b", Mode::Gitlink, recorded),
        ],
    );
    let r = short(recorded);
    assert_eq!(
        lines(&changes(&t)),
        [
            format!("M a g:{r} -> g:unreadable no-diff"),
            format!("M b g:{r} -> g:unreadable no-diff"),
        ]
    );
}

#[test]
fn worktree_ignore_case_matches_index_paths_without_case() {
    for ignore_case in [true, false] {
        let t = clean_repo(&[("Readme.md", b"r\n"), ("Docs/a.md", b"a\n")]);
        t.append_config(&format!("[core]\n\tignorecase = {ignore_case}\n"));
        // The worktree spells them differently, as a case-insensitive file
        // system may report them.
        std::fs::rename(t.root().join("Readme.md"), t.root().join("README.md")).unwrap();
        std::fs::rename(t.root().join("Docs"), t.root().join("docs")).unwrap();
        let set = changes(&t);
        let untracked: Vec<&[u8]> = set
            .files()
            .iter()
            .filter(|c| c.untracked)
            .map(|c| c.path.as_bytes())
            .collect();
        if ignore_case {
            assert!(untracked.is_empty(), "{:#?}", lines(&set));
        } else {
            assert_eq!(
                untracked,
                [&b"README.md"[..], b"docs/a.md"],
                "{:#?}",
                lines(&set)
            );
        }
    }
}

#[test]
fn worktree_refresh_paths_of_a_directory_reloads_its_attributes() {
    let crlf: &[u8] = b"one\r\ntwo\r\n";
    // A tracked `d/.gitattributes` that the first comparison loads.
    let t = clean_repo(&[("d/.gitattributes", b"# none\n"), ("d/a.txt", crlf)]);
    let wt = main_worktree(&t);
    let cancel = no_cancel();
    let first = wt.changes(Base::Head, &Limits::default(), &cancel).unwrap();
    assert_eq!(lines(&first), Vec::<String>::new());
    t.write_file("d/.gitattributes", b"*.txt text eol=lf\n");
    let set = wt
        .refresh(&first, Refresh::Paths(&[rp("d")]), &cancel)
        .unwrap();
    let l = lines(&set);
    assert!(
        l.iter()
            .any(|l| l.starts_with("M d/a.txt") && l.contains(&b7(b"one\ntwo\n"))),
        "{l:#?}"
    );

    // A new, untracked `.gitattributes` beneath a reported directory.
    let t = clean_repo(&[("e/a.txt", crlf)]);
    let wt = main_worktree(&t);
    let first = wt.changes(Base::Head, &Limits::default(), &cancel).unwrap();
    t.write_file("e/.gitattributes", b"*.txt text eol=lf\n");
    let set = wt
        .refresh(&first, Refresh::Paths(&[rp("e")]), &cancel)
        .unwrap();
    let l = lines(&set);
    assert!(
        l.iter()
            .any(|l| l.starts_with("M e/a.txt") && l.contains(&b7(b"one\ntwo\n"))),
        "{l:#?}"
    );
}

#[test]
fn worktree_submodule_gitdir_outside_modules_is_not_read() {
    let t = TestRepo::new();
    // An absorbed submodule whose `.git` file points outside `modules/`.
    let elsewhere = t.base().join("elsewhere.git");
    let head = nested_repo(&t.base().join("elsewhere"), true).unwrap();
    std::fs::rename(t.base().join("elsewhere/.git"), &elsewhere).unwrap();
    t.write_file(
        "sub/.git",
        format!("gitdir: {}\n", elsewhere.display()).as_bytes(),
    );
    // And one in `modules/`, as `git submodule absorbgitdirs` leaves it.
    let modules = t.common_dir().join("modules/ok");
    let ok_head = nested_repo(&t.base().join("ok-src"), true).unwrap();
    std::fs::create_dir_all(modules.parent().unwrap()).unwrap();
    std::fs::rename(t.base().join("ok-src/.git"), &modules).unwrap();
    t.write_file("ok/.git", b"gitdir: ../.git/modules/ok\n");
    let recorded = t.blob(b"recorded");
    let tree = t.tree_entries(&[
        ("ok", Mode::Gitlink, recorded),
        ("sub", Mode::Gitlink, recorded),
    ]);
    let commit = t.commit(tree, &[], "c");
    t.set_ref("refs/heads/main", commit);
    t.write_index(
        t.common_dir(),
        &[
            IndexEntry::new("ok", Mode::Gitlink, recorded),
            IndexEntry::new("sub", Mode::Gitlink, recorded),
        ],
    );
    let r = short(recorded);
    let _ = head;
    assert_eq!(
        lines(&changes(&t)),
        [
            format!("M ok g:{r} -> g:{} no-diff staged=g:{r}", short(ok_head)),
            format!("M sub g:{r} -> g:unreadable no-diff"),
        ]
    );
}

#[test]
fn worktree_unstaged_mv_is_not_paired() {
    let content: &[u8] = b"line 1\nline 2\nline 3\n";
    let t = clean_repo(&[("a", content)]);
    std::fs::rename(t.root().join("a"), t.root().join("b")).unwrap();
    let c = b7(content);
    assert_eq!(
        lines(&changes(&t)),
        [
            format!("D a f:{c} -> - text(+0,-3) staged=f:{c}"),
            format!("A b (untracked) - -> f:{c} text(+3,-0)"),
        ]
    );
}

#[test]
fn worktree_conflict_deleted_in_head_modified_by_them_has_no_old_side() {
    let t = TestRepo::new();
    commit_files(&t, &[("other", b"o\n")]);
    let conflict = |stage, content: &[u8]| IndexEntry {
        stage,
        ..IndexEntry::new("p", Mode::File, t.blob(content))
    };
    t.write_index(
        t.common_dir(),
        &[
            IndexEntry::new("other", Mode::File, t.blob(b"o\n")),
            conflict(1, b"base\n"),
            conflict(3, b"theirs\n"),
        ],
    );
    t.write_file("other", b"o\n");
    t.write_file("p", b"theirs\n");
    assert_eq!(
        lines(&changes(&t)),
        [format!("C p - -> f:{} text(+1,-0)", b7(b"theirs\n"))]
    );
}

#[test]
fn worktree_add_add_conflict_with_the_file_deleted_has_no_sides() {
    let t = TestRepo::new();
    commit_files(&t, &[("other", b"o\n")]);
    let conflict = |stage, content: &[u8]| IndexEntry {
        stage,
        ..IndexEntry::new("p", Mode::File, t.blob(content))
    };
    t.write_index(
        t.common_dir(),
        &[
            IndexEntry::new("other", Mode::File, t.blob(b"o\n")),
            conflict(2, b"ours\n"),
            conflict(3, b"theirs\n"),
        ],
    );
    t.write_file("other", b"o\n");
    assert_eq!(lines(&changes(&t)), ["C p - -> - text(+0,-0)"]);
}

#[test]
fn worktree_file_replaced_by_a_directory() {
    let t = clean_repo(&[("thing", b"a file\n"), ("keep", b"k\n")]);
    t.remove("thing");
    t.write_file("thing/x", b"x\n");
    t.write_file("thing/y", b"y\n");
    let a = b7(b"a file\n");
    assert_eq!(
        lines(&changes(&t)),
        [
            format!("D thing f:{a} -> - text(+0,-1) staged=f:{a}"),
            format!("A thing/x (untracked) - -> f:{} text(+1,-0)", b7(b"x\n")),
            format!("A thing/y (untracked) - -> f:{} text(+1,-0)", b7(b"y\n")),
        ]
    );
}

#[test]
fn worktree_submodules_compare_their_head() {
    let t = TestRepo::new();
    let head = nested_repo(&t.root().join("sub"), true).unwrap();
    let moved = nested_repo(&t.root().join("moved"), true).unwrap();
    // `moved`'s HEAD moves on after the commit recorded it.
    let recorded = t.blob(b"not the current commit");
    std::fs::create_dir_all(t.root().join("uninit")).unwrap();
    let uninit = t.blob(b"some commit");
    let tree = t.tree_entries(&[
        ("sub", Mode::Gitlink, head),
        ("moved", Mode::Gitlink, recorded),
        ("uninit", Mode::Gitlink, uninit),
    ]);
    let commit = t.commit(tree, &[], "c");
    t.set_ref("refs/heads/main", commit);
    t.write_index(
        t.common_dir(),
        &[
            IndexEntry::new("moved", Mode::Gitlink, recorded),
            IndexEntry::new("sub", Mode::Gitlink, head),
            IndexEntry::new("uninit", Mode::Gitlink, uninit),
        ],
    );
    let r = short(recorded);
    assert_eq!(
        lines(&changes(&t)),
        [format!(
            "M moved g:{r} -> g:{} no-diff staged=g:{r}",
            short(moved)
        )]
    );
}

#[test]
#[cfg(unix)]
fn worktree_unchanged_symlink_is_not_a_change() {
    let t = TestRepo::new();
    t.symlink("link", "target/path");
    let tree = t.tree_entries(&[("link", Mode::Symlink, t.blob(b"target/path"))]);
    let commit = t.commit(tree, &[], "c");
    t.set_ref("refs/heads/main", commit);
    t.write_index(t.common_dir(), &[t.add("link")]);
    assert_eq!(lines(&changes(&t)), Vec::<String>::new());
    t.remove("link");
    t.symlink("link", "other");
    assert_eq!(
        lines(&changes(&t)),
        [format!(
            "M link l:{l} -> l:{} text(+1,-1) staged=l:{l}",
            b7(b"other"),
            l = b7(b"target/path")
        )]
    );
}

#[test]
#[cfg(unix)]
fn worktree_staged_executable_bit_shows_in_staged() {
    use std::os::unix::fs::PermissionsExt;

    let t = TestRepo::new();
    commit_files(&t, &[("run", b"v1\n")]);
    t.write_file("run", b"v1\n");
    std::fs::set_permissions(t.root().join("run"), std::fs::Permissions::from_mode(0o755)).unwrap();
    t.write_index(t.common_dir(), &[t.add("run")]);
    t.write_file("run", b"v2\n");
    let v1 = b7(b"v1\n");
    assert_eq!(
        lines(&changes(&t)),
        [format!(
            "M run f:{v1} -> x:{} text(+1,-1) staged=x:{v1}",
            b7(b"v2\n")
        )]
    );
}

#[test]
fn worktree_intent_to_add_is_an_addition() {
    let t = clean_repo(&[("a", b"a\n")]);
    t.write_file("new", b"new\n");
    t.write_index(
        t.common_dir(),
        &[
            t.add("a"),
            IndexEntry {
                intent_to_add: true,
                ..IndexEntry::new("new", Mode::File, Oid::for_blob(HashKind::Sha1, b""))
            },
        ],
    );
    assert_eq!(
        lines(&changes(&t)),
        [format!("A new - -> f:{} text(+1,-0)", b7(b"new\n"))]
    );
}

#[test]
fn worktree_sparse_entries_are_not_deleted() {
    let t = TestRepo::new();
    let lib = t.tree(&[("a.rs", b"a\n"), ("b.rs", b"b\n")]);
    let readme = t.blob(b"r\n");
    let tree = t.tree_entries(&[("README", Mode::File, readme), ("lib", Mode::File, readme)]);
    // Rebuild with `lib` as a directory.
    let _ = tree;
    let root = {
        let files: Vec<(&str, &[u8])> = vec![
            ("README", b"r\n"),
            ("lib/a.rs", b"a\n"),
            ("lib/b.rs", b"b\n"),
        ];
        t.tree(&files)
    };
    let commit = t.commit(root, &[], "c");
    t.set_ref("refs/heads/main", commit);
    t.write_file("README", b"r\n");
    t.write_index_with(
        t.common_dir(),
        &[t.add("README")],
        &crate::testing::IndexOptions {
            sparse_dirs: &[("lib", lib)],
            ..Default::default()
        },
    );
    assert_eq!(lines(&changes(&t)), Vec::<String>::new());
}

#[test]
fn worktree_unborn_head_makes_everything_added() {
    let t = TestRepo::new();
    t.write_file("a", b"a\n");
    t.write_index(t.common_dir(), &[t.add("a")]);
    t.write_file("b", b"b\n");
    let set = changes(&t);
    assert_eq!(set.base_tree(), Oid::empty_tree(HashKind::Sha1));
    assert_eq!(
        lines(&set),
        [
            format!("A a - -> f:{} text(+1,-0)", b7(b"a\n")),
            format!("A b (untracked) - -> f:{} text(+1,-0)", b7(b"b\n")),
        ]
    );
}

#[test]
fn worktree_sha256_repository_uses_full_length_oids() {
    let t = TestRepo::sha256();
    t.write_file("a", b"a\n");
    t.backdate("a");
    let tree = t.tree(&[("a", b"a\n")]);
    let commit = t.commit(tree, &[], "c");
    t.set_ref("refs/heads/main", commit);
    t.write_index(t.common_dir(), &[t.add("a")]);
    t.write_file("a", b"b\n");
    t.write_file("u", b"u\n");
    let set = changes(&t);
    assert_eq!(set.files().len(), 2);
    for c in set.files() {
        let oid = c.new.as_ref().unwrap().content.oid().unwrap();
        assert_eq!(oid.kind(), HashKind::Sha256);
        assert_eq!(oid.to_hex().len(), 64);
    }
    assert_eq!(
        set.files()[0].new.as_ref().unwrap().content,
        Content::Blob(Oid::for_blob(HashKind::Sha256, b"b\n"))
    );
}

#[test]
fn worktree_base_commit_and_merge_base() {
    let t = TestRepo::new();
    let c1 = t.commit(t.tree(&[("a", b"1\n")]), &[], "c1");
    let c2 = t.commit(t.tree(&[("a", b"2\n")]), &[c1], "c2");
    let c3 = t.commit(t.tree(&[("a", b"3\n")]), &[c1], "c3");
    t.set_ref("refs/heads/main", c2);
    t.set_ref("refs/heads/feature", c3);
    t.write_file("a", b"2\n");
    t.backdate("a");
    t.write_index(t.common_dir(), &[t.add("a")]);
    assert_eq!(lines(&changes(&t)), Vec::<String>::new());
    let from_c1 = format!("M a f:{} -> f:{} text(+1,-1)", b7(b"1\n"), b7(b"2\n"));
    let set = changes_with(&t, Base::Commit(c1), &Limits::default());
    assert_eq!(lines(&set), std::slice::from_ref(&from_c1));
    let feature = RefName::new("refs/heads/feature").unwrap();
    let set = changes_with(&t, Base::MergeBase(feature), &Limits::default());
    assert_eq!(lines(&set), [from_c1]);
    let missing = RefName::new("refs/heads/nope").unwrap();
    assert!(
        main_worktree(&t)
            .changes(Base::MergeBase(missing), &Limits::default(), &no_cancel())
            .is_err()
    );
}

#[test]
fn worktree_linked_worktree_compares_its_own_index_and_root() {
    let t = clean_repo(&[("a", b"main\n")]);
    let commit = t.commit(t.tree(&[("a", b"linked\n")]), &[], "linked");
    t.set_ref("refs/heads/linked", commit);
    let root = t.base().join("linked");
    let admin = t.add_worktree("linked", &root, TestHead::Branch("refs/heads/linked"));
    std::fs::write(root.join("a"), b"linked\n").unwrap();
    std::fs::write(root.join("extra"), b"e\n").unwrap();
    t.write_index(
        &admin,
        &[IndexEntry::new("a", Mode::File, t.blob(b"linked\n"))],
    );
    let wt = t
        .repo()
        .worktree(&WorktreeId::Linked("linked".into()))
        .unwrap();
    let set = wt
        .changes(Base::Head, &Limits::default(), &no_cancel())
        .unwrap();
    assert_eq!(
        set.target(),
        &Target::Worktree(WorktreeId::Linked("linked".into()))
    );
    assert_eq!(
        lines(&set),
        [format!(
            "A extra (untracked) - -> f:{} text(+1,-0)",
            b7(b"e\n")
        )]
    );
    // The main worktree is untouched by it.
    assert_eq!(lines(&changes(&t)), Vec::<String>::new());
}

#[test]
fn worktree_invalid_index_paths_are_not_opened() {
    let t = clean_repo(&[("a", b"a\n")]);
    let blob = t.blob(b"x");
    t.write_index(
        t.common_dir(),
        &[
            t.add("a"),
            IndexEntry::new("../evil", Mode::File, blob),
            IndexEntry::new("x/./y", Mode::File, blob),
        ],
    );
    let set = changes(&t);
    assert_eq!(lines(&set), Vec::<String>::new());
    let invalid: Vec<&[u8]> = set.totals().invalid_paths.iter().map(|p| &p[..]).collect();
    assert_eq!(invalid, [&b"../evil"[..], b"x/./y"]);
}

#[test]
fn worktree_cancel_returns_cancelled() {
    let t = clean_repo(&[("a", b"a\n")]);
    t.write_file("a", b"b\n");
    let cancel = AtomicBool::new(true);
    assert!(matches!(
        main_worktree(&t).changes(Base::Head, &Limits::default(), &cancel),
        Err(GitError::Cancelled)
    ));
}

#[test]
fn worktree_handles_never_outlive_a_call() {
    let t = clean_repo(&[("a", b"a\n")]);
    t.write_file("a", b"b\n");
    let wt = main_worktree(&t);
    let cancel = no_cancel();
    let set = wt.changes(Base::Head, &Limits::default(), &cancel).unwrap();
    assert_eq!(crate::handle::hook::live_count(), 0);
    let set = wt
        .refresh(&set, Refresh::Paths(&[rp("a")]), &cancel)
        .unwrap();
    let _ = wt.refresh(&set, Refresh::Full, &cancel).unwrap();
    assert_eq!(crate::handle::hook::live_count(), 0);
}

// ---- limits and retained content ----------------------------------------------

#[test]
fn worktree_binary_and_mode_only_count_towards_the_file_limit() {
    let t = clean_repo(&[("a.bin", b"\0one"), ("b", b"b\n"), ("c", b"c\n")]);
    t.write_file("a.bin", b"\0two");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(t.root().join("b"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    t.write_file("c", b"changed\n");
    let limits = Limits {
        total_files: 2,
        ..Limits::default()
    };
    let set = changes_with(&t, Base::Head, &limits);
    let details: Vec<&Detail> = set.files().iter().map(|c| &c.detail).collect();
    assert!(matches!(details[0], Detail::Binary { .. }), "{details:?}");
    assert_eq!(details[1], &Detail::ModeOnly);
    assert_eq!(details[2], &Detail::Listed);
    assert_eq!(set.totals().listed, 1);
    // Listed files keep their full OIDs.
    assert_eq!(
        set.files()[2].new.as_ref().unwrap().content,
        Content::Blob(Oid::for_blob(HashKind::Sha1, b"changed\n"))
    );
}

#[test]
fn worktree_retained_content_limit_keeps_each_detail() {
    let t = clean_repo(&[("keep", b"k\n")]);
    t.write_file("a.txt", b"aaaa\n");
    t.write_file("b.bin", b"\0binary content");
    t.write_file("c.txt", b"one\ntwo\nthree\n");
    t.write_file("d.big", &[b'd'; 60]);
    let limits = Limits {
        retained_bytes: 10,
        diff_bytes: 50,
        ..Limits::default()
    };
    let set = changes_with(&t, Base::Head, &limits);
    insta::assert_snapshot!(lines(&set).join("\n"));
    assert_eq!(set.totals().not_retained, 3);
    let repo = t.repo();
    let texts: Vec<SideText> = (0..4).map(|i| set.texts(&repo, i).unwrap().new).collect();
    assert!(matches!(&texts[0], SideText::Text(text) if text.bytes() == b"aaaa\n"));
    for text in &texts[1..] {
        assert_eq!(text, &SideText::NotRetained);
    }
}

#[test]
fn worktree_texts_are_what_was_compared() {
    let t = clean_repo(&[("a", b"old\n")]);
    t.write_file("a", b"compared\n");
    t.write_file("u", b"untracked\n");
    let set = changes(&t);
    t.write_file("a", b"later\n");
    t.write_file("u", b"later\n");
    let repo = t.repo();
    let texts = set.texts(&repo, 0).unwrap();
    assert!(matches!(&texts.old, SideText::Text(x) if x.bytes() == b"old\n"));
    assert!(matches!(&texts.new, SideText::Text(x) if x.bytes() == b"compared\n"));
    let texts = set.texts(&repo, 1).unwrap();
    assert_eq!(texts.old, SideText::Absent);
    assert!(matches!(&texts.new, SideText::Text(x) if x.bytes() == b"untracked\n"));
}

// ---- refresh and the stat cache -----------------------------------------------

fn text_ptr(set: &ChangeSet, repo: &Repo, path: &str) -> *const u8 {
    let i = set
        .files()
        .iter()
        .position(|c| c.path.as_bytes() == path.as_bytes())
        .unwrap();
    match set.texts(repo, i).unwrap().new {
        SideText::Text(text) => text.bytes().as_ptr(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn worktree_refresh_paths_reads_only_the_reported_paths() {
    let t = clean_repo(&[("a", b"a\n"), ("b", b"b\n"), ("c", b"c\n"), ("d/e", b"e\n")]);
    t.write_file("a", b"a1\n");
    t.write_file("b", b"b1\n");
    let wt = main_worktree(&t);
    let cancel = no_cancel();
    let first = wt.changes(Base::Head, &Limits::default(), &cancel).unwrap();
    assert_eq!(first.files().len(), 2);
    t.write_file("a", b"a2\n");
    t.write_file("c", b"c1\n");
    let reads = hook::reads();
    let set = read_only(&t, || {
        wt.refresh(&first, Refresh::Paths(&[rp("a")]), &cancel)
            .unwrap()
    });
    assert_eq!(hook::reads(), reads + 1, "only `a` is read");
    let l = lines(&set);
    assert_eq!(l.len(), 2, "{l:#?}");
    assert!(l[0].contains(&b7(b"a2\n")), "{l:#?}");
    assert!(l[1].contains(&b7(b"b1\n")), "{l:#?}");
    // `b` is shared with the previous change set, not read again.
    let repo = t.repo();
    assert_eq!(text_ptr(&first, &repo, "b"), text_ptr(&set, &repo, "b"));
    // `c` wasn't reported; a directory stands for everything beneath it.
    t.write_file("d/new", b"n\n");
    t.remove("d/e");
    let set = wt
        .refresh(&set, Refresh::Paths(&[rp("c"), rp("d")]), &cancel)
        .unwrap();
    let paths: Vec<String> = set
        .files()
        .iter()
        .map(|c| String::from_utf8(c.path.as_bytes().to_vec()).unwrap())
        .collect();
    assert_eq!(paths, ["a", "b", "c", "d/e", "d/new"]);
    // The same as comparing everything.
    assert_eq!(
        lines(&set),
        lines(&wt.changes(Base::Head, &Limits::default(), &cancel).unwrap())
    );
}

#[test]
fn worktree_refresh_paths_compares_everything_when_the_index_changed() {
    let t = clean_repo(&[("a", b"a\n"), ("b", b"b\n")]);
    let wt = main_worktree(&t);
    let cancel = no_cancel();
    let first = wt.changes(Base::Head, &Limits::default(), &cancel).unwrap();
    t.write_file("b", b"b1\n");
    t.write_index(t.common_dir(), &[t.add("a"), t.add("b")]);
    let set = wt
        .refresh(&first, Refresh::Paths(&[rp("a")]), &cancel)
        .unwrap();
    assert_eq!(
        lines(&set),
        [format!(
            "M b f:{} -> f:{} text(+1,-1)",
            b7(b"b\n"),
            b7(b"b1\n")
        )]
    );
}

#[test]
fn worktree_refresh_rejects_other_change_sets() {
    let t = clean_repo(&[("a", b"a\n")]);
    let wt = main_worktree(&t);
    let cancel = no_cancel();
    let head = t
        .repo()
        .resolve(&RefName::new("refs/heads/main").unwrap())
        .unwrap()
        .unwrap();
    let trees = t
        .repo()
        .diff_trees(head, head, &Limits::default(), &cancel)
        .unwrap();
    let err = wt.refresh(&trees, Refresh::Full, &cancel).unwrap_err();
    assert!(matches!(err, GitError::Io(e) if e.kind() == std::io::ErrorKind::InvalidInput));
    t.set_ref("refs/heads/linked", head);
    let root = t.base().join("linked");
    t.add_worktree("linked", &root, TestHead::Branch("refs/heads/linked"));
    let linked = t
        .repo()
        .worktree(&WorktreeId::Linked("linked".into()))
        .unwrap()
        .changes(Base::Head, &Limits::default(), &cancel)
        .unwrap();
    assert!(wt.refresh(&linked, Refresh::Full, &cancel).is_err());
}

#[test]
fn worktree_stat_cache_reuses_contents_a_change_set_still_holds() {
    let t = clean_repo(&[("a", b"a\n")]);
    t.write_file("u", b"untracked\n");
    t.backdate("u");
    let wt = main_worktree(&t);
    let cancel = no_cancel();
    let reads = hook::reads();
    let first = wt.changes(Base::Head, &Limits::default(), &cancel).unwrap();
    assert_eq!(hook::reads(), reads + 1);
    let second = wt.refresh(&first, Refresh::Full, &cancel).unwrap();
    assert_eq!(hook::reads(), reads + 1, "unchanged stat: not read again");
    assert_eq!(lines(&first), lines(&second));
    drop((first, second));
    let _ = wt.changes(Base::Head, &Limits::default(), &cancel).unwrap();
    assert_eq!(hook::reads(), reads + 2, "no change set holds the bytes");
    // A file changed in the last second is always read.
    t.write_file("u", b"untracked\n");
    let a = wt.changes(Base::Head, &Limits::default(), &cancel).unwrap();
    let _b = wt.changes(Base::Head, &Limits::default(), &cancel).unwrap();
    assert_eq!(hook::reads(), reads + 4);
    drop(a);
}

#[test]
fn worktree_stat_cache_keeps_no_reference_to_contents() {
    // ADR 0035: an `Arc<[u8]>` keeps its bytes in the allocation of its
    // counts, so even a `Weak` in the stat cache would keep every read file
    // allocated after the change set is dropped.
    let t = clean_repo(&[("a", b"a\n")]);
    t.write_file("a", b"a changed\n");
    t.backdate("a");
    t.write_file("u", &b"untracked\n".repeat(1000));
    t.backdate("u");
    let wt = main_worktree(&t);
    let cancel = no_cancel();
    let first = wt.changes(Base::Head, &Limits::default(), &cancel).unwrap();
    let kept: Vec<&Arc<[u8]>> = first.retained().iter().flatten().collect();
    assert_eq!(kept.len(), 2);
    for bytes in kept {
        assert_eq!(Arc::weak_count(bytes), 0, "the stat cache holds no Weak");
        // The change set's retained list and its records for refresh.
        assert_eq!(Arc::strong_count(bytes), 2, "only the change set holds it");
    }

    // A refresh still shares the bytes the change set it is given kept.
    let reads = hook::reads();
    let second = wt.refresh(&first, Refresh::Full, &cancel).unwrap();
    assert_eq!(hook::reads(), reads, "both hit the stat cache");
    for (a, b) in first.retained().iter().zip(second.retained()) {
        assert!(Arc::ptr_eq(a.as_ref().unwrap(), b.as_ref().unwrap()));
    }
    assert_eq!(lines(&first), lines(&second));
    drop(first);
    for bytes in second.retained().iter().flatten() {
        assert_eq!(Arc::weak_count(bytes), 0);
        assert_eq!(Arc::strong_count(bytes), 2);
    }

    // Without a change set that kept them, the bytes are read again.
    drop(second);
    let _third = wt.changes(Base::Head, &Limits::default(), &cancel).unwrap();
    assert_eq!(hook::reads(), reads + 2);
}

#[test]
fn worktree_stat_cache_hit_equal_to_the_index_needs_no_bytes() {
    // A touched file with the content of its index blob: its stat never
    // matches the index again (the index isn't written), but the stat cache
    // knows its OID and the object database has the content.
    let t = clean_repo(&[("a", b"a\n")]);
    t.write_file("a", b"a\n");
    t.set_mtime(
        "a",
        std::time::SystemTime::now() - std::time::Duration::from_secs(7200),
    );
    let wt = main_worktree(&t);
    let cancel = no_cancel();
    let reads = hook::reads();
    let first = wt.changes(Base::Head, &Limits::default(), &cancel).unwrap();
    assert_eq!(hook::reads(), reads + 1, "the stat differs: read once");
    drop(first);
    let second = wt.changes(Base::Head, &Limits::default(), &cancel).unwrap();
    assert_eq!(
        hook::reads(),
        reads + 1,
        "a stat cache hit, no change set held"
    );
    assert_eq!(lines(&second), Vec::<String>::new());
}

// ---- cache-tree: the base tree's subtrees the index describes ---------------

const CACHE_FILES: &[(&str, &[u8])] = &[
    ("README", b"r\n"),
    ("a/x", b"x\n"),
    ("a/y", b"y\n"),
    ("a/deep/z", b"z\n"),
    ("b/w", b"w\n"),
];

/// A clean worktree of `CACHE_FILES`, HEAD's tree returned.
fn cache_repo() -> (TestRepo, Oid) {
    let t = clean_repo(CACHE_FILES);
    let tree = t.tree(CACHE_FILES);
    (t, tree)
}

/// Writes the index of `entries`, with the cache-tree of `tree` (minus the
/// `invalid` directories) or without one, and returns the comparison's
/// lines and how many base-tree objects it read.
fn with_cache_tree(
    t: &TestRepo,
    entries: &[IndexEntry],
    cache: Option<(Oid, &[&str])>,
    base: Base,
) -> (Vec<String>, usize) {
    let options = crate::testing::IndexOptions {
        cache_tree: cache,
        ..Default::default()
    };
    t.write_index_with(t.common_dir(), entries, &options);
    let before = hook::trees_read();
    let set = changes_with(t, base, &Limits::default());
    (lines(&set), hook::trees_read() - before)
}

fn entries(t: &TestRepo) -> Vec<IndexEntry> {
    CACHE_FILES.iter().map(|(p, _)| t.add(p)).collect()
}

#[test]
fn cache_tree_valid_root_reads_no_base_tree() {
    let (t, tree) = cache_repo();
    let e = entries(&t);
    let (plain, read) = with_cache_tree(&t, &e, None, Base::Head);
    assert_eq!(plain, Vec::<String>::new());
    assert_eq!(read, 4, "root, a, a/deep, b");
    let (cached, read) = with_cache_tree(&t, &e, Some((tree, &[])), Base::Head);
    assert_eq!(cached, plain);
    assert_eq!(read, 0, "the root's node names HEAD's tree");
}

#[test]
fn cache_tree_skip_still_sees_a_worktree_change_beneath() {
    // The index (and so its cache-tree) is unchanged; the file is not.
    let (t, tree) = cache_repo();
    let e = entries(&t);
    t.write_file("a/deep/z", b"changed\n");
    let (plain, _) = with_cache_tree(&t, &e, None, Base::Head);
    assert_eq!(plain.len(), 1, "{plain:?}");
    let (cached, read) = with_cache_tree(&t, &e, Some((tree, &[])), Base::Head);
    assert_eq!(cached, plain);
    assert_eq!(read, 0);
}

#[test]
fn cache_tree_invalidated_directories_are_read() {
    // `git add a/x` with new content: git invalidates `a` and the root.
    let (t, _) = cache_repo();
    t.write_file("a/x", b"staged\n");
    t.backdate("a/x");
    let e = entries(&t);
    let staged: Vec<(&str, &[u8])> = CACHE_FILES
        .iter()
        .map(|&(p, c)| {
            if p == "a/x" {
                (p, &b"staged\n"[..])
            } else {
                (p, c)
            }
        })
        .collect();
    let index_tree = t.tree(&staged);
    let (plain, _) = with_cache_tree(&t, &e, None, Base::Head);
    assert_eq!(plain.len(), 1, "{plain:?}");
    let (cached, read) = with_cache_tree(&t, &e, Some((index_tree, &["", "a"])), Base::Head);
    assert_eq!(cached, plain);
    // The root and `a` are read; `a/deep` and `b` are still valid and equal.
    assert_eq!(read, 2);
}

#[test]
fn cache_tree_of_another_tree_matches_only_equal_subtrees() {
    // Base::Commit of an older commit whose `b` differs: `b` is read, `a`
    // (same subtree as the index) is not.
    let (t, tree) = cache_repo();
    let older: Vec<(&str, &[u8])> = CACHE_FILES
        .iter()
        .map(|&(p, c)| {
            if p == "b/w" {
                (p, &b"old\n"[..])
            } else {
                (p, c)
            }
        })
        .collect();
    let old = t.commit(t.tree(&older), &[], "old");
    let e = entries(&t);
    let (plain, _) = with_cache_tree(&t, &e, None, Base::Commit(old));
    assert_eq!(plain.len(), 1, "{plain:?}");
    let (cached, read) = with_cache_tree(&t, &e, Some((tree, &[])), Base::Commit(old));
    assert_eq!(cached, plain);
    assert_eq!(read, 2, "the root and b");
}

#[test]
fn cache_tree_with_a_conflict_beneath_is_not_trusted() {
    // A stale node that still counts `a`'s entries, while `a/y` is in
    // conflict: the entries beneath aren't plain, so `a` is read.
    let (t, tree) = cache_repo();
    let mut e = entries(&t);
    let y = e.iter().position(|x| x.path == b"a/y").unwrap();
    let ours = IndexEntry {
        stage: 2,
        ..e[y].clone()
    };
    e[y].stage = 3;
    e.push(ours);
    let (plain, _) = with_cache_tree(&t, &e, None, Base::Head);
    let (cached, read) = with_cache_tree(&t, &e, Some((tree, &[])), Base::Head);
    assert_eq!(cached, plain);
    assert!(plain.iter().any(|l| l.contains("a/y")), "{plain:?}");
    assert!(read >= 2, "the root and a: {read}");
}

/// A worktree large enough for the parallel stat pass (ADR 0038): the
/// result is the same, in path order, every time.
#[test]
fn worktree_large_parallel_passes_are_deterministic() {
    let n = 2 * STAT_CHUNK + 500;
    let files: Vec<(String, Vec<u8>)> = (0..n)
        .map(|i| {
            (
                format!("p{}/src/d{}/f{i:05}.ts", i % 13, i % 7),
                format!("// {i}\n").into_bytes(),
            )
        })
        .collect();
    let mut refs: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_slice()))
        .collect();
    refs.push((".gitignore", b"build/\n"));
    let t = clean_repo(&refs);
    // Changes spread over every chunk of the stat pass, untracked files in
    // many directories, an ignored directory and a nested repository.
    let mut expected = Vec::new();
    for i in (0..n).step_by(n / 6) {
        t.write_file(&files[i].0, b"changed\n");
        expected.push(files[i].0.clone());
    }
    t.remove(&files[n - 1].0);
    expected.push(files[n - 1].0.clone());
    for i in 0..13 {
        let path = format!("p{i}/src/d{}/new{i}.ts", i % 7);
        t.write_file(&path, b"new\n");
        expected.push(path);
        t.write_file(&format!("p{i}/build/out.js"), b"ignored\n");
    }
    t.write_file("p3/vendor/.git/HEAD", b"ref: refs/heads/main\n");
    expected.push("p3/vendor".into());
    expected.sort();
    let mut previous = None;
    for _ in 0..3 {
        let set = changes(&t);
        let paths: Vec<String> = set
            .files()
            .iter()
            .map(|c| String::from_utf8(c.path.as_bytes().to_vec()).unwrap())
            .collect();
        assert_eq!(paths, expected);
        let lines = lines(&set);
        if let Some(previous) = &previous {
            assert_eq!(&lines, previous);
        }
        previous = Some(lines);
    }
}

#[test]
fn worktree_compare_phases_are_timed_in_order() {
    let t = clean_repo(&[("a", b"a\n"), ("d/b", b"b\n")]);
    t.append_config("[core]\n\tignoreCase = true\n");
    t.write_file("u", b"untracked\n");
    let wt = main_worktree(&t);
    let cancel = no_cancel();
    let first = wt.changes(Base::Head, &Limits::default(), &cancel).unwrap();
    let names: Vec<&str> = crate::testing::compare_phases()
        .iter()
        .map(|(name, _)| *name)
        .collect();
    let all = [
        "open",
        "index",
        "base_tree",
        "setup",
        "stat",
        "fold",
        "untracked",
        "attributes",
        "classify",
        "assemble",
        "finish",
    ];
    assert_eq!(names, all);
    // A refresh of no path walks nothing: no `fold` or `untracked`.
    let _ = wt.refresh(&first, Refresh::Paths(&[]), &cancel).unwrap();
    let names: Vec<&str> = crate::testing::compare_phases()
        .iter()
        .map(|(name, _)| *name)
        .collect();
    let expected: Vec<&str> = all
        .into_iter()
        .filter(|n| *n != "fold" && *n != "untracked")
        .collect();
    assert_eq!(names, expected);
}

#[test]
fn worktree_stat_mismatches_of_a_clean_worktree_are_none() {
    // What bench's `git/changes_clean_100k/stat_*` count: none for files
    // whose stat the index recorded, also after every file was read (an
    // access that changed ctime would make each comparison read them all).
    let files: Vec<(String, Vec<u8>)> = (0..40)
        .map(|i| {
            (
                format!("p{}/src/d{}/f{i}.ts", i % 3, i % 5),
                format!("// {i}\n").into_bytes(),
            )
        })
        .collect();
    let refs: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_slice()))
        .collect();
    let t = clean_repo(&refs);
    t.append_config("[core]\n\tignoreCase = true\n");
    let expected = crate::testing::StatMismatches {
        entries: 40,
        ..Default::default()
    };
    let wt = main_worktree(&t);
    assert_eq!(crate::testing::stat_mismatches(&wt).unwrap(), expected);
    let root = lectern_platform::SafeRoot::open(t.root()).unwrap();
    for (path, _) in &files {
        let rel = lectern_platform::RelPath::new(path.as_bytes()).unwrap();
        assert!(matches!(
            root.read(rel, 1 << 20),
            Ok(lectern_platform::Read::File { .. })
        ));
    }
    let wt = main_worktree(&t);
    assert_eq!(crate::testing::stat_mismatches(&wt).unwrap(), expected);
    let reads = crate::testing::worktree_reads();
    assert_eq!(lines(&changes(&t)), Vec::<String>::new());
    assert_eq!(
        crate::testing::worktree_reads(),
        reads,
        "a clean worktree reads nothing"
    );

    // A changed file is counted by field.
    t.write_file(&files[0].0, b"// changed, longer\n");
    t.backdate(&files[0].0);
    let wt = main_worktree(&t);
    let after = crate::testing::stat_mismatches(&wt).unwrap();
    assert_eq!((after.unmatched, after.size, after.racy), (1, 1, 0));
}

#[test]
fn worktree_fingerprint_follows_contents_not_truncation() {
    let t = clean_repo(&[("a", b"a\n"), ("b", b"b\n")]);
    t.write_file("a", b"a1\n");
    t.write_file("b", b"b1\n");
    let one = changes(&t).fingerprint();
    assert_eq!(changes(&t).fingerprint(), one);
    let listed = changes_with(
        &t,
        Base::Head,
        &Limits {
            total_files: 1,
            retained_bytes: 0,
            ..Limits::default()
        },
    );
    assert_eq!(listed.totals().listed, 1);
    assert_eq!(listed.fingerprint(), one, "truncation doesn't change it");
    t.write_file("b", b"b2\n");
    assert_ne!(changes(&t).fingerprint(), one);
    assert_eq!(one.to_hex().len(), 64);
}

// ---- read_only_* ----------------------------------------------------------------

#[test]
fn read_only_racy_entries_are_not_refreshed() {
    // Entries as new as the index: git would refresh them in the index.
    let t = TestRepo::new();
    t.write_file("a", b"a\n");
    t.write_file("b", b"b\n");
    commit_files(&t, &[("a", b"a\n"), ("b", b"b\n")]);
    t.write_index(t.common_dir(), &[t.add("a"), t.add("b")]);
    t.write_file("b", b"b1\n");
    t.write_file("u", b"u\n");
    let index = std::fs::read(t.common_dir().join("index")).unwrap();
    let wt = main_worktree(&t);
    let cancel = no_cancel();
    let set = read_only(&t, || {
        let set = wt.changes(Base::Head, &Limits::default(), &cancel).unwrap();
        let set = wt.refresh(&set, Refresh::Full, &cancel).unwrap();
        wt.refresh(&set, Refresh::Paths(&[rp("b"), rp("u")]), &cancel)
            .unwrap()
    });
    assert_eq!(set.files().len(), 2);
    assert_eq!(std::fs::read(t.common_dir().join("index")).unwrap(), index);
}

#[test]
fn read_only_worktree_contents_stay_out_of_the_object_database() {
    let t = clean_repo(&[("a", b"a\n")]);
    t.write_file("a", b"modified content\n");
    t.write_file("u", b"untracked content\n");
    let objects = git_dir_state(&t.common_dir().join("objects"));
    let set = changes(&t);
    assert_eq!(set.files().len(), 2);
    assert_eq!(git_dir_state(&t.common_dir().join("objects")), objects);
    let repo = t.repo();
    for c in set.files() {
        let oid = c.new.as_ref().unwrap().content.oid().unwrap();
        assert!(matches!(
            repo.blob(oid, 1024),
            Err(GitError::ObjectMissing(_))
        ));
    }
}

// ---- canonical_* -----------------------------------------------------------------

fn canonical(t: &TestRepo, path: &str, bytes: &[u8]) -> Option<Oid> {
    read_only(t, || {
        main_worktree(t).canonical_oid(&rp(path), bytes).unwrap()
    })
}

/// The OIDs git recorded for the conversion fixture (`git add` under each
/// `core.autocrlf`): text / eol, `text=auto`, `-text`, ident and
/// working-tree-encoding, from the raw worktree bytes.
#[test]
fn canonical_matches_git_for_the_conversion_fixture() {
    let dir = fixture("worktree_conversions");
    for autocrlf in ["false", "true", "input"] {
        let t = TestRepo::new();
        t.append_config(&format!("[core]\n\tautocrlf = {autocrlf}\n"));
        let paths = t.import_worktree_fixture(&dir);
        let expected = ls_files(&dir.join(format!("expected/autocrlf={autocrlf}/ls-files")));
        assert_eq!(expected.len(), paths.len());
        for path in &paths {
            let raw = std::fs::read(t.root().join(path)).unwrap();
            let oid = canonical(&t, path, &raw).unwrap();
            assert_eq!(
                oid.to_hex(),
                expected[path].1,
                "{path} under core.autocrlf = {autocrlf}"
            );
        }
    }
}

#[test]
fn canonical_lf_and_crlf_with_text_auto_or_eol_give_the_index_oid() {
    let attrs: &[u8] = b"*.auto text=auto\n*.eol eol=lf\n*.txt text eol=lf\n";
    let lf: &[u8] = b"one\ntwo\n";
    let crlf: &[u8] = b"one\r\ntwo\r\n";
    let files: &[(&str, &[u8])] = &[
        (".gitattributes", attrs),
        ("a.auto", lf),
        ("b.eol", lf),
        ("c.txt", lf),
        ("d.none", lf),
        ("e.auto", crlf),
    ];
    let t = clean_repo(files);
    let index = |path: &str| {
        main_worktree(&t)
            .tracked(&EntryLimits::default(), &no_cancel())
            .unwrap()
            .iter()
            .find(|e| e.path == rp(path))
            .unwrap()
            .oid
    };
    for path in ["a.auto", "b.eol", "c.txt"] {
        assert_eq!(canonical(&t, path, lf), Some(index(path)), "{path} LF");
        assert_eq!(canonical(&t, path, crlf), Some(index(path)), "{path} CRLF");
    }
    // No attribute and `core.autocrlf` unset: the bytes as they are.
    assert_eq!(canonical(&t, "d.none", lf), Some(index("d.none")));
    assert_eq!(
        canonical(&t, "d.none", crlf),
        Some(Oid::for_blob(HashKind::Sha1, crlf))
    );
    // `text=auto` leaves CRLF alone when the index blob already has CRs, as
    // git does.
    assert_eq!(canonical(&t, "e.auto", crlf), Some(index("e.auto")));
    // A path not in the index converts the same way.
    assert_eq!(
        canonical(&t, "new.auto", crlf),
        Some(Oid::for_blob(HashKind::Sha1, lf))
    );
}

#[test]
fn canonical_collapses_ident() {
    let stored: &[u8] = b"version $Id$\nplain\n";
    let t = clean_repo(&[(".gitattributes", b"*.c ident\n"), ("x.c", stored)]);
    let expanded: &[u8] = b"version $Id: 0123456789abcdef0123456789abcdef01234567 $\nplain\n";
    let want = Some(Oid::for_blob(HashKind::Sha1, stored));
    assert_eq!(canonical(&t, "x.c", expanded), want);
    assert_eq!(canonical(&t, "x.c", stored), want);
    // Without the attribute the expanded keyword stays.
    assert_eq!(
        canonical(&t, "x.h", expanded),
        Some(Oid::for_blob(HashKind::Sha1, expanded))
    );
}

#[test]
fn canonical_with_a_clean_or_process_filter_is_none() {
    let t = TestRepo::new();
    t.append_config(LFS_CONFIG);
    t.append_config(
        "[filter \"proc\"]\n\tprocess = proc-filter\n[filter \"smudge-only\"]\n\tsmudge = cat\n",
    );
    t.write_file(
        ".gitattributes",
        b"*.bin filter=lfs -text\n*.p filter=proc\n*.s filter=smudge-only\n*.u filter=unknown\n",
    );
    let bytes: &[u8] = b"content\n";
    let raw = Some(Oid::for_blob(HashKind::Sha1, bytes));
    assert_eq!(canonical(&t, "a.bin", bytes), None);
    assert_eq!(canonical(&t, "dir/a.bin", bytes), None);
    assert_eq!(canonical(&t, "a.p", bytes), None);
    // A driver without clean / process, or one that isn't configured,
    // changes nothing on the way in, as in git.
    assert_eq!(canonical(&t, "a.s", bytes), raw);
    assert_eq!(canonical(&t, "a.u", bytes), raw);
    assert_eq!(canonical(&t, "a.txt", bytes), raw);
}

#[test]
fn canonical_uses_the_attribute_files_of_every_directory_level() {
    let lf: &[u8] = b"one\ntwo\n";
    let crlf: &[u8] = b"one\r\ntwo\r\n";
    let t = clean_repo(&[("gone/.gitattributes", b"*.txt text eol=lf\n")]);
    // Untracked, two levels down.
    t.write_file("d/e/.gitattributes", b"*.txt text eol=lf\n");
    // Tracked but deleted from the worktree: the index blob applies, as in
    // git's check-in order.
    t.remove("gone/.gitattributes");
    let want = Some(Oid::for_blob(HashKind::Sha1, lf));
    assert_eq!(canonical(&t, "d/e/a.txt", crlf), want);
    assert_eq!(canonical(&t, "d/e/f/a.txt", crlf), want);
    assert_eq!(canonical(&t, "gone/a.txt", crlf), want);
    // Outside those directories nothing applies.
    let raw = Some(Oid::for_blob(HashKind::Sha1, crlf));
    assert_eq!(canonical(&t, "d/a.txt", crlf), raw);
    assert_eq!(canonical(&t, "a.txt", crlf), raw);
}

#[test]
fn canonical_on_sha256_has_the_full_length() {
    let t = TestRepo::sha256();
    t.write_file(".gitattributes", b"*.txt text eol=lf\n");
    let oid = canonical(&t, "a.txt", b"one\r\n").unwrap();
    assert_eq!(oid, Oid::for_blob(HashKind::Sha256, b"one\n"));
    assert_eq!(oid.to_hex().len(), 64);
}

// ---- no_external_programs -------------------------------------------------------

/// Repository configuration names every external program git could run,
/// each a script that writes a marker; after every worktree and tree
/// comparison the marker doesn't exist. Under nextest (one process per
/// test) a fake `git` and `sh` that record their calls also go first on
/// PATH.
#[test]
#[cfg(unix)]
fn no_external_programs() {
    use std::os::unix::fs::PermissionsExt;

    let t = TestRepo::new();
    let marker = t.base().join("ran");
    let bin = t.base().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = |name: &str| {
        let path = bin.join(name);
        std::fs::write(
            &path,
            format!("#!/bin/sh\necho \"{name} $*\" >> '{}'\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    };
    let hooks = t.base().join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    for hook in [
        "post-checkout",
        "post-index-change",
        "fsmonitor-watchman",
        "pre-commit",
    ] {
        let path = script(hook);
        std::fs::copy(&path, hooks.join(hook)).unwrap();
    }
    t.append_config(&format!(
        "[core]\n\tfsmonitor = {}\n\thooksPath = {}\n[diff]\n\texternal = {}\n\
         [diff \"x\"]\n\ttextconv = {}\n[filter \"x\"]\n\tclean = {}\n\tsmudge = {}\n\
         \tprocess = {}\n\trequired = true\n[gpg]\n\tprogram = {}\n",
        script("fsmonitor"),
        hooks.display(),
        script("external-diff"),
        script("textconv"),
        script("clean"),
        script("smudge"),
        script("process"),
        script("gpg"),
    ));
    if std::env::var_os("NEXTEST_EXECUTION_MODE").is_some_and(|m| m == "process-per-test") {
        script("git");
        script("sh");
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![bin.clone()];
        paths.extend(std::env::split_paths(&path));
        let joined = std::env::join_paths(paths).unwrap();
        // SAFETY: nextest runs this test alone in its own process, so no
        // other thread reads the environment while it is changed.
        unsafe { std::env::set_var("PATH", joined) };
    }

    let attrs: &[u8] = b"*.t diff=x\n*.f filter=x\n";
    let files: &[(&str, &[u8])] = &[
        (".gitattributes", attrs),
        ("a.t", b"text one\n"),
        ("b.f", b"filtered\n"),
        ("c.txt", b"plain\n"),
    ];
    t.write_file(".gitattributes", attrs);
    for (path, content) in files {
        t.write_file(path, content);
    }
    let base = commit_files(&t, files);
    t.write_index(
        t.common_dir(),
        &files.iter().map(|(p, _)| t.add(p)).collect::<Vec<_>>(),
    );
    t.write_file("a.t", b"text two\n");
    t.write_file("b.f", b"filtered two\n");
    t.write_file("c.txt", b"plain two\n");
    t.write_file("new.t", b"new\n");
    t.write_file("new.f", b"new\n");
    let wt = main_worktree(&t);
    let canonical = read_only(&t, || {
        (
            wt.canonical_oid(&rp("b.f"), b"filtered two\n").unwrap(),
            wt.canonical_oid(&rp("a.t"), b"text two\n").unwrap(),
        )
    });
    assert_eq!(
        canonical,
        (None, Some(Oid::for_blob(HashKind::Sha1, b"text two\n")))
    );

    let repo = t.repo();
    let wt = main_worktree(&t);
    let cancel = no_cancel();
    let set = read_only(&t, || {
        wt.changes(Base::Head, &Limits::default(), &cancel).unwrap()
    });
    assert_eq!(set.files().len(), 5);
    let set = wt.refresh(&set, Refresh::Full, &cancel).unwrap();
    let set = wt
        .refresh(&set, Refresh::Paths(&[rp("a.t"), rp("b.f")]), &cancel)
        .unwrap();
    for i in 0..set.files().len() {
        let texts = set.texts(&repo, i).unwrap();
        if let Detail::Text(diff) = &set.files()[i].detail {
            let _ = rows(diff, &texts, 3);
        }
    }
    let other = commit_files(
        &t,
        &[("a.t", b"x\n"), ("b.f", b"y\n"), (".gitattributes", attrs)],
    );
    let trees = repo
        .diff_trees(base, other, &Limits::default(), &cancel)
        .unwrap();
    for i in 0..trees.files().len() {
        let _ = trees.texts(&repo, i).unwrap();
    }
    assert!(
        !marker.exists(),
        "{}",
        std::fs::read_to_string(&marker).unwrap_or_default()
    );
}
