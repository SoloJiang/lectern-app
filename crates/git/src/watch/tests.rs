//! `watch_source_*`: the `fs_watch` source (docs/design/fs_watch.md「测试」,
//! the paragraph on `git`).

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use lectern_fs_watch::{FilterRequest, PathFilter, Tracked, WatchPath};

use super::{Env, Filter, config_files};
use crate::handle::ConfigScope;
use crate::testing::{IndexEntry, TestHead, TestRepo};
use crate::{EntryLimits, Mode, Oid, WorktreeId};

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

fn main_worktree(t: &TestRepo) -> crate::Worktree {
    t.repo().worktree(&WorktreeId::Main).unwrap()
}

fn filter(t: &TestRepo) -> Arc<dyn PathFilter> {
    main_worktree(t)
        .watch_source()
        .filter(&FilterRequest::default(), &no_cancel())
        .unwrap()
}

fn wp(path: &str) -> WatchPath {
    WatchPath::new(path.as_bytes()).unwrap()
}

fn rule_files(f: &dyn PathFilter) -> Vec<String> {
    f.rule_files()
        .iter()
        .map(|p| String::from_utf8_lossy(p.as_bytes()).into_owned())
        .collect()
}

/// What fs_watch's classification makes of an untracked path
/// (docs/design/fs_watch.md「路径分类」steps 6 and 8): ignored when a
/// directory above it is, or when it is itself.
fn effectively_ignored(f: &dyn PathFilter, path: &str, is_dir: bool) -> bool {
    let mut end = 0;
    while let Some(slash) = path[end..].find('/') {
        end += slash;
        if f.ignored(&path.as_bytes()[..end], true) {
            return true;
        }
        end += 1;
    }
    f.ignored(path.as_bytes(), is_dir)
}

/// Every file and directory under the worktree, `.git` excluded, sorted,
/// with whether it is a directory.
fn worktree_paths(root: &Path) -> Vec<(String, bool)> {
    fn visit(base: &Path, dir: &Path, out: &mut Vec<(String, bool)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            let is_dir = std::fs::symlink_metadata(&path).unwrap().is_dir();
            out.push((rel, is_dir));
            if is_dir {
                visit(base, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    visit(root, root, &mut out);
    out.sort();
    out
}

/// The worktree of `watch_source_ignores_match_git`: untracked `.gitignore`
/// files at the root and in untracked directories, one in an ignored
/// directory, `info/exclude` and `core.excludesFile`. No two names differ
/// only in case, so it means the same on a case-insensitive file system.
fn ignore_repo(ignore_case: bool) -> TestRepo {
    let t = TestRepo::new();
    t.write_file(
        ".gitignore",
        b"*.log\n!keep.log\n/build/\n/Out/\nnode_modules/\ndocs/**/*.tmp\n\\#hash\nCase.txt\n\
          !un.swp\nsub/deep/*.dat\n",
    );
    t.write_file("sub/.gitignore", b"*.o\n!important.o\n/local\n");
    t.write_file("sub/deep/.gitignore", b"!*.log\n");
    t.write_file("newdir/.gitignore", b"*.gen\n");
    // In an ignored directory: never read. Were it read, `foo` would match.
    t.write_file("node_modules/pkg/.gitignore", b"!index.js\nfoo\n");
    std::fs::create_dir_all(t.common_dir().join("info")).unwrap();
    std::fs::write(
        t.common_dir().join("info/exclude"),
        b"*.secret\n!keep.bak\n",
    )
    .unwrap();
    let excludes = t.base().join("excludes");
    std::fs::write(&excludes, b"*.bak\n*.swp\n").unwrap();
    t.append_config(&format!(
        "[core]\n\texcludesFile = {}\n\tignoreCase = {ignore_case}\n",
        excludes.display()
    ));
    for path in [
        "a.log",
        "keep.log",
        "x.txt",
        "build/out.js",
        "sub/build",
        "docs/a/b/c.tmp",
        "docs/c.tmp",
        "#hash",
        "CASE.TXT",
        "FOO.LOG",
        "OUT/x.js",
        "un.swp",
        "b.swp",
        "a.bak",
        "keep.bak",
        "a.secret",
        "sub/a.o",
        "sub/important.o",
        "sub/B.O",
        "sub/local",
        "sub/x/local",
        "sub/deep/x.log",
        "sub/deep/y.dat",
        "sub/deep/z.o",
        "newdir/a.gen",
        "newdir/b.txt",
        "node_modules/pkg/index.js",
        "node_modules/pkg/foo",
    ] {
        t.write_file(path, path.as_bytes());
    }
    t
}

/// The paths git judges ignored in `ignore_repo` (`git check-ignore
/// --no-index`, which also reports paths under an ignored directory),
/// worked out from gitignore(5): the deepest `.gitignore` with a matching
/// pattern decides and its last matching pattern wins, then `info/exclude`,
/// then `core.excludesFile`; nothing beneath an excluded directory is
/// re-included.
const GIT_IGNORED: &[&str] = &[
    "#hash",
    "a.bak",
    "a.log",
    "a.secret",
    "b.swp",
    "build",
    "build/out.js",
    "docs/a/b/c.tmp",
    "docs/c.tmp",
    "newdir/a.gen",
    "node_modules",
    "node_modules/pkg",
    "node_modules/pkg/.gitignore",
    "node_modules/pkg/foo",
    "node_modules/pkg/index.js",
    "sub/a.o",
    "sub/deep/y.dat",
    "sub/deep/z.o",
    "sub/local",
];

/// With `core.ignoreCase = true` patterns also match other cases.
const GIT_IGNORED_FOLDED: &[&str] = &["CASE.TXT", "FOO.LOG", "OUT", "OUT/x.js", "sub/B.O"];

#[test]
fn watch_source_ignores_match_git() {
    for ignore_case in [false, true] {
        let t = ignore_repo(ignore_case);
        let f = filter(&t);
        let ignored: Vec<String> = worktree_paths(t.root())
            .into_iter()
            .filter(|(path, is_dir)| effectively_ignored(&*f, path, *is_dir))
            .map(|(path, _)| path)
            .collect();
        let mut expected: Vec<&str> = GIT_IGNORED.to_vec();
        if ignore_case {
            expected.extend(GIT_IGNORED_FOLDED);
        }
        expected.sort();
        assert_eq!(ignored, expected, "core.ignoreCase = {ignore_case}");
        // The root's and untracked directories' `.gitignore` files apply;
        // the one under `node_modules/` is never read.
        assert_eq!(
            rule_files(&*f),
            [
                ".gitignore",
                "newdir/.gitignore",
                "sub/.gitignore",
                "sub/deep/.gitignore"
            ]
        );
        assert!(!f.ignored(b"node_modules/pkg/foo", false));
        // `.gitignore` files are matched like any path: not ignored here.
        assert!(!f.ignored(b"sub/.gitignore", false));
    }
}

#[test]
fn watch_source_ignored_only_looks_at_the_path_itself() {
    let t = ignore_repo(false);
    let f = filter(&t);
    // `build/` is ignored as a directory; what is beneath it is fs_watch's
    // business (it checks the parents one by one).
    assert!(f.ignored(b"build", true));
    assert!(
        !f.ignored(b"build", false),
        "`/build/` matches directories only"
    );
    assert!(!f.ignored(b"build/out.js", false));
    assert!(f.ignored(b"deeper/still/x.log", false));
}

#[test]
fn watch_source_tracked_paths_and_shared_index() {
    let t = TestRepo::new();
    let blob = t.blob(b"x\n");
    let mut sparse = IndexEntry::new("sparse/a.txt", Mode::File, blob);
    sparse.skip_worktree = true;
    t.write_index(
        t.common_dir(),
        &[
            IndexEntry::new("README.md", Mode::File, blob),
            IndexEntry::new("docs/Guide.md", Mode::File, blob),
            IndexEntry::new("Sub", Mode::Gitlink, Oid::empty_tree(t.hash())),
            sparse,
        ],
    );
    for ignore_case in [false, true] {
        // Always explicit: on a case-insensitive file system (macOS) gix's
        // init writes `ignorecase = true`, as `git init` does.
        t.append_config(&format!("[core]\n\tignoreCase = {ignore_case}\n"));
        let wt = main_worktree(&t);
        let f = Filter::build(&wt, &[], 20_000, &no_cancel()).unwrap();
        let tracked = wt.tracked(&EntryLimits::default(), &no_cancel()).unwrap();
        assert!(
            Arc::ptr_eq(&f.index.paths, &tracked),
            "the filter shares the worktree's file list"
        );
        assert_eq!(f.tracked(b"README.md"), Tracked::File);
        assert_eq!(f.tracked(b"docs"), Tracked::Dir);
        assert_eq!(f.tracked(b"docs/Guide.md"), Tracked::File);
        assert_eq!(f.tracked(b"Sub"), Tracked::Gitlink);
        assert_eq!(f.tracked(b"sparse/a.txt"), Tracked::File, "skip-worktree");
        assert_eq!(f.tracked(b"doc"), Tracked::No);
        assert_eq!(f.tracked(b"docs/Guide.md/x"), Tracked::No);
        let folded = if ignore_case {
            [Tracked::File, Tracked::Dir, Tracked::File, Tracked::Gitlink]
        } else {
            [Tracked::No; 4]
        };
        assert_eq!(
            [
                f.tracked(b"readme.md"),
                f.tracked(b"DOCS"),
                f.tracked(b"docs/guide.MD"),
                f.tracked(b"sub"),
            ],
            folded,
            "core.ignoreCase = {ignore_case}"
        );
    }
}

#[test]
fn watch_source_tracked_and_skip_worktree_gitignores() {
    let t = TestRepo::new();
    t.write_file("tracked/.gitignore", b"*.t\n");
    let tracked = t.add("tracked/.gitignore");
    // Outside the sparse checkout: not in the worktree, read from the
    // index as git does.
    let mut sparse = IndexEntry::new("sparse/.gitignore", Mode::File, t.blob(b"*.sp\n"));
    sparse.skip_worktree = true;
    t.write_index(t.common_dir(), &[tracked, sparse]);
    let f = filter(&t);
    assert!(f.ignored(b"tracked/a.t", false));
    assert!(f.ignored(b"sparse/a.sp", false));
    assert!(
        !f.ignored(b"a.t", false),
        "rules apply beneath their directory"
    );
    assert_eq!(rule_files(&*f), ["sparse/.gitignore", "tracked/.gitignore"]);
}

#[test]
fn watch_source_explicit_gitignores_beyond_the_walk_limit() {
    let t = TestRepo::new();
    t.write_file(".gitignore", b"ignored/\n");
    for name in ["a1", "a2", "a3"] {
        t.write_file(name, b"x\n");
    }
    t.write_file("ignored/.gitignore", b"*.i\n");
    t.write_file("y/.gitignore", b"*.y\n");
    t.write_file("z/.gitignore", b"*.z\n");
    t.write_index(t.common_dir(), &[t.add("y/.gitignore")]);
    let wt = main_worktree(&t);

    // The walk stops after two untracked entries (`.gitignore`, `a1`), long
    // before `y/` and `z/`. The tracked `y/.gitignore` is read anyway; the
    // untracked `z/.gitignore` only when an event named it.
    let f = Filter::build(&wt, &[], 2, &no_cancel()).unwrap();
    assert!(f.ignored(b"y/a.y", false));
    assert!(!f.ignored(b"z/a.z", false));
    assert_eq!(rule_files(&f), [".gitignore", "y/.gitignore"]);

    // Named explicitly: read despite the limit, except beneath the ignored
    // directory, like git.
    let named = [wp("z/.gitignore"), wp("ignored/.gitignore")];
    let f = Filter::build(&wt, &named, 2, &no_cancel()).unwrap();
    assert!(f.ignored(b"z/a.z", false));
    assert!(!f.ignored(b"ignored/a.i", false));
    assert_eq!(
        rule_files(&f),
        [".gitignore", "y/.gitignore", "z/.gitignore"]
    );

    // Without a limit the walk finds `z/.gitignore` by itself.
    let f = Filter::build(&wt, &[], 20_000, &no_cancel()).unwrap();
    assert!(f.ignored(b"z/a.z", false));
}

#[test]
fn watch_source_explicit_gitignores_in_submodules_and_nested_repositories_are_not_read() {
    let t = TestRepo::new();
    t.write_file("module/.gitignore", b"*.m\n");
    t.write_file("nested/.git/HEAD", b"ref: refs/heads/main\n");
    t.write_file("nested/.gitignore", b"*.n\n");
    t.write_index(
        t.common_dir(),
        &[IndexEntry::new(
            "module",
            Mode::Gitlink,
            Oid::empty_tree(t.hash()),
        )],
    );
    let wt = main_worktree(&t);
    let named = [wp("module/.gitignore"), wp("nested/.gitignore")];
    let f = Filter::build(&wt, &named, 20_000, &no_cancel()).unwrap();
    assert!(!f.ignored(b"module/a.m", false));
    assert!(!f.ignored(b"nested/a.n", false));
    assert!(rule_files(&f).is_empty());
}

#[test]
fn watch_source_rule_files_follow_a_renamed_directory() {
    let t = TestRepo::new();
    t.write_file("old/.gitignore", b"*.o\n");
    let f = filter(&t);
    assert_eq!(rule_files(&*f), ["old/.gitignore"]);
    std::fs::rename(t.root().join("old"), t.root().join("new")).unwrap();
    let f = filter(&t);
    assert_eq!(rule_files(&*f), ["new/.gitignore"]);
    assert!(f.ignored(b"new/a.o", false));
    assert!(!f.ignored(b"old/a.o", false));
}

#[test]
#[cfg(unix)]
fn watch_source_symlinked_gitignore_is_not_read() {
    let t = TestRepo::new();
    let outside = t.base().join("outside-ignore");
    std::fs::write(&outside, b"*\n").unwrap();
    t.symlink(".gitignore", outside.to_str().unwrap());
    let wt = main_worktree(&t);
    let f = Filter::build(&wt, &[wp(".gitignore")], 20_000, &no_cancel()).unwrap();
    assert!(!f.ignored(b"a.txt", false));
    assert!(rule_files(&f).is_empty());
}

#[test]
fn watch_source_cancelled() {
    let t = ignore_repo(false);
    let cancel = AtomicBool::new(true);
    let source = main_worktree(&t).watch_source();
    assert!(source.filter(&FilterRequest::default(), &cancel).is_err());
    assert!(source.config_files(&cancel).is_err());
}

/// The config list with `<base>`, `<common>`, `<home>` and `<xdg>` in place
/// of those directories, sorted as strings.
fn listed(t: &TestRepo, wt: &crate::Worktree, env: &Env) -> Vec<String> {
    let files = config_files(wt, env, ConfigScope::User, &no_cancel()).unwrap();
    let mut names: Vec<(String, &str)> = vec![(t.common_dir().display().to_string(), "<common>")];
    if let Some(xdg) = &env.xdg_config_home {
        names.push((xdg.display().to_string(), "<xdg>"));
    }
    if let Some(home) = &env.home {
        names.push((home.display().to_string(), "<home>"));
    }
    names.push((t.base().display().to_string(), "<base>"));
    let mut out: Vec<String> = files
        .iter()
        .map(|p| {
            let p = p.display().to_string();
            names
                .iter()
                .find_map(|(dir, name)| p.strip_prefix(dir.as_str()).map(|r| format!("{name}{r}")))
                .unwrap_or(p)
        })
        .collect();
    out.sort();
    out
}

fn include_repo() -> (TestRepo, Env) {
    let t = TestRepo::new();
    let base = t.base();
    let home = base.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(base.join("plain.inc"), b"[user]\n\tname = x\n").unwrap();
    std::fs::write(base.join("on.inc"), b"[include]\n\tpath = nested-on.inc\n").unwrap();
    std::fs::write(
        base.join("off.inc"),
        b"[include]\n\tpath = nested-off.inc\n",
    )
    .unwrap();
    t.append_config(&format!(
        "[include]\n\tpath = {base}/plain.inc\n\tpath = missing.inc\n\tpath = ~/home.inc\n\
         [includeIf \"onbranch:main\"]\n\tpath = {base}/on.inc\n\
         [includeIf \"onbranch:other\"]\n\tpath = {base}/off.inc\n\
         [core]\n\texcludesFile = {base}/excludes\n",
        base = base.display()
    ));
    let env = Env {
        home: Some(home),
        xdg_config_home: None,
    };
    (t, env)
}

#[test]
fn watch_source_config_files_list_includes_and_standard_locations() {
    let (t, env) = include_repo();
    let wt = main_worktree(&t);
    assert_eq!(
        listed(&t, &wt, &env),
        [
            "/etc/gitattributes",
            "/etc/gitconfig",
            "<base>/excludes",
            // Loaded on `main`, and what it includes in turn.
            "<base>/nested-on.inc",
            // Not loaded on `main`, listed all the same; its own includes
            // only once it is loaded.
            "<base>/off.inc",
            "<base>/on.inc",
            "<base>/plain.inc",
            "<common>/config",
            "<common>/config.worktree",
            "<common>/info/attributes",
            "<common>/info/exclude",
            // Missing: listed, relative to the including file.
            "<common>/missing.inc",
            "<home>/.config/git/attributes",
            "<home>/.config/git/config",
            "<home>/.gitconfig",
            "<home>/home.inc",
        ]
    );
}

#[test]
fn watch_source_config_files_follow_the_branch() {
    let (t, env) = include_repo();
    t.set_head(TestHead::Branch("refs/heads/other"));
    let wt = main_worktree(&t);
    let files = listed(&t, &wt, &env);
    for listed in ["<base>/on.inc", "<base>/off.inc", "<base>/nested-off.inc"] {
        assert!(files.iter().any(|f| f == listed), "{listed}: {files:#?}");
    }
    assert!(
        !files.iter().any(|f| f == "<base>/nested-on.inc"),
        "{files:#?}"
    );
}

#[test]
fn watch_source_config_files_xdg_defaults() {
    let t = TestRepo::new();
    let env = Env {
        home: Some(t.base().join("home")),
        xdg_config_home: Some(t.base().join("xdg")),
    };
    let admin = t.add_worktree("w", &t.base().join("w"), TestHead::Branch("refs/heads/w"));
    let wt = t.repo().worktree(&WorktreeId::Linked("w".into())).unwrap();
    assert_eq!(admin, t.common_dir().join("worktrees/w"));
    assert_eq!(
        listed(&t, &wt, &env),
        [
            "/etc/gitattributes",
            "/etc/gitconfig",
            "<common>/config",
            "<common>/info/attributes",
            "<common>/info/exclude",
            // The origin gix records, through the worktree's `commondir`
            // (`../..`); fs_watch resolves `..` component by component.
            "<common>/worktrees/w/../../config",
            "<common>/worktrees/w/config.worktree",
            "<home>/.gitconfig",
            "<xdg>/git/attributes",
            "<xdg>/git/config",
            "<xdg>/git/ignore",
        ]
    );
}

#[test]
fn watch_source_config_files_of_the_repository_scope() {
    // `testing::discover` reads only the repository's configuration, so
    // only the repository's files and its includes are listed.
    let (t, _) = include_repo();
    let files = main_worktree(&t)
        .watch_source()
        .config_files(&no_cancel())
        .unwrap();
    assert!(files.iter().all(|f| f.is_absolute()));
    assert!(files.contains(&t.base().join("on.inc")));
    assert!(!files.iter().any(|f| f.starts_with("/etc")), "{files:#?}");
    assert!(!files.contains(&t.base().join("home/home.inc")));
}

/// No git CLI and no program the repository configures runs while the
/// filter and the config list are built. Under nextest (one process per
/// test) a fake `git` that records its calls also goes first on PATH.
#[test]
#[cfg(unix)]
fn watch_source_runs_no_git() {
    use std::os::unix::fs::PermissionsExt;

    let (t, _) = include_repo();
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
    t.append_config(&format!(
        "[core]\n\tfsmonitor = {}\n\tattributesFile = {}/attributes\n",
        script("fsmonitor"),
        t.base().display()
    ));
    if std::env::var_os("NEXTEST_EXECUTION_MODE").is_some_and(|m| m == "process-per-test") {
        script("git");
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![bin.clone()];
        paths.extend(std::env::split_paths(&path));
        let joined = std::env::join_paths(paths).unwrap();
        // SAFETY: nextest runs this test alone in its own process, so no
        // other thread reads the environment while it is changed.
        unsafe { std::env::set_var("PATH", joined) };
    }
    t.write_file(".gitignore", b"*.log\n");
    t.write_file("sub/.gitignore", b"*.o\n");
    let source = main_worktree(&t).watch_source();
    let request = FilterRequest {
        gitignores: &[wp("sub/.gitignore")],
        previous: None,
    };
    let f = source.filter(&request, &no_cancel()).unwrap();
    assert!(f.ignored(b"a.log", false));
    let files = source.config_files(&no_cancel()).unwrap();
    assert!(files.contains(&t.base().join("attributes")));
    assert!(
        !marker.exists(),
        "{}",
        std::fs::read_to_string(&marker).unwrap_or_default()
    );
}

#[test]
fn watch_source_worktree_names() {
    use lectern_fs_watch::WorktreeName;
    assert_eq!(WorktreeName::from(WorktreeId::Main), WorktreeName::Main);
    assert_eq!(
        WorktreeName::from(WorktreeId::Linked("main".into())),
        WorktreeName::Linked("main".into())
    );
    assert_eq!(WorktreeId::from(WorktreeName::Main), WorktreeId::Main);
    assert_eq!(
        WorktreeId::from(WorktreeName::Linked("w".into())),
        WorktreeId::Linked("w".into())
    );
}

// --- the walk reused after an index-only change ---------------------------------

/// What fs_watch asks for after an index-only change: a filter on a new
/// `Worktree` (the index read again), given the filter in place.
fn rebuilt(t: &TestRepo, previous: &Arc<dyn PathFilter>) -> Arc<dyn PathFilter> {
    let request = FilterRequest {
        gitignores: &[],
        previous: Some(&**previous),
    };
    main_worktree(t)
        .watch_source()
        .filter(&request, &no_cancel())
        .unwrap()
}

fn reused(f: &Arc<dyn PathFilter>) -> bool {
    crate::testing::filter_reused(&**f).expect("a git filter")
}

/// Every answer fs_watch can get from `f` about the worktree's paths and a
/// few that don't exist, and its rule files.
fn answers(f: &dyn PathFilter, root: &Path) -> Vec<String> {
    let mut paths = worktree_paths(root);
    for (path, is_dir) in [("absent", true), ("absent/x.gen", false), ("x.o", false)] {
        paths.push((path.to_owned(), is_dir));
    }
    let mut out = vec![format!("rules {:?}", rule_files(f))];
    for (path, is_dir) in paths {
        out.push(format!(
            "{path}: {:?} ignored={} effectively={}",
            f.tracked(path.as_bytes()),
            f.ignored(path.as_bytes(), is_dir),
            effectively_ignored(f, &path, is_dir)
        ));
    }
    out
}

/// `f` answers as a filter built with a full walk on the same worktree.
fn assert_same_as_a_full_build(t: &TestRepo, f: &dyn PathFilter) {
    let full = filter(t);
    assert!(!reused(&full));
    assert_eq!(answers(f, t.root()), answers(&*full, t.root()));
}

/// The rebuild walks, and gives what a full build gives.
fn falls_back(t: &TestRepo, previous: &Arc<dyn PathFilter>) -> Arc<dyn PathFilter> {
    let f = rebuilt(t, previous);
    assert!(!reused(&f), "walked");
    assert_same_as_a_full_build(t, &*f);
    f
}

#[test]
fn watch_source_index_change_reuses_the_walk() {
    for ignore_case in [false, true] {
        let t = ignore_repo(ignore_case);
        t.write_index(t.common_dir(), &[t.add("x.txt"), t.add("sub/a.o")]);
        let first = filter(&t);
        assert!(!reused(&first), "the first build walks");

        // `git add newdir/b.txt` and `git rm --cached x.txt`.
        t.write_index(t.common_dir(), &[t.add("newdir/b.txt"), t.add("sub/a.o")]);
        let reads = crate::compare::hook::reads();
        let second = rebuilt(&t, &first);
        assert!(reused(&second), "core.ignoreCase = {ignore_case}");
        assert_eq!(
            crate::compare::hook::reads(),
            reads,
            "no `.gitignore` read again"
        );
        assert_eq!(second.tracked(b"newdir/b.txt"), Tracked::File);
        assert_eq!(second.tracked(b"x.txt"), Tracked::No);
        assert_same_as_a_full_build(&t, &*second);

        // And again from the reused filter.
        t.write_index(t.common_dir(), &[t.add("sub/a.o")]);
        let third = rebuilt(&t, &second);
        assert!(reused(&third));
        assert_same_as_a_full_build(&t, &*third);
    }
}

#[test]
fn watch_source_reuse_drops_the_rules_of_a_new_nested_repository() {
    let t = TestRepo::new();
    t.write_file("vendor/lib/.gitignore", b"*.gen\n");
    t.write_file("vendor/lib/a.gen", b"x\n");
    t.write_file("a.txt", b"x\n");
    t.write_index(t.common_dir(), &[t.add("a.txt")]);
    let first = filter(&t);
    assert_eq!(rule_files(&*first), ["vendor/lib/.gitignore"]);
    // `git init vendor/lib` (fs_watch drops `.git` paths: no reload), then
    // an index change. The walk wouldn't enter `vendor/lib` now.
    t.write_file("vendor/lib/.git/HEAD", b"ref: refs/heads/main\n");
    t.write_file("b.txt", b"x\n");
    t.write_index(t.common_dir(), &[t.add("a.txt"), t.add("b.txt")]);
    let second = rebuilt(&t, &first);
    assert!(reused(&second));
    assert!(rule_files(&*second).is_empty());
    assert_same_as_a_full_build(&t, &*second);
}

#[test]
fn watch_source_reuse_falls_back_when_submodules_change() {
    let t = TestRepo::new();
    t.write_file("module/.gitignore", b"*.m\n");
    t.write_file("sub/.gitignore", b"*.s\n");
    let gitlink = |path: &str| IndexEntry::new(path, Mode::Gitlink, Oid::empty_tree(t.hash()));
    t.write_index(t.common_dir(), &[gitlink("module")]);
    let first = filter(&t);
    assert_eq!(rule_files(&*first), ["sub/.gitignore"]);
    // The submodule removed: the walk enters `module/` now.
    t.write_index(t.common_dir(), &[]);
    let second = falls_back(&t, &first);
    assert_eq!(
        rule_files(&*second),
        ["module/.gitignore", "sub/.gitignore"]
    );
    // A submodule added where the walk went.
    t.write_index(t.common_dir(), &[gitlink("sub")]);
    let third = falls_back(&t, &second);
    assert_eq!(rule_files(&*third), ["module/.gitignore"]);
}

#[test]
fn watch_source_reuse_falls_back_when_a_skipped_repository_changes() {
    let t = TestRepo::new();
    t.write_file("nested/.git/HEAD", b"ref: refs/heads/main\n");
    t.write_file("nested/.gitignore", b"*.n\n");
    t.write_file("nested/a.txt", b"x\n");
    t.write_file("other/.git/HEAD", b"ref: refs/heads/main\n");
    t.write_file("other/.gitignore", b"*.o\n");
    let first = filter(&t);
    assert!(
        rule_files(&*first).is_empty(),
        "nested repositories aren't entered"
    );
    // An index path beneath `nested/`: the walk enters it now.
    t.write_index(t.common_dir(), &[t.add("nested/a.txt")]);
    let second = falls_back(&t, &first);
    assert_eq!(rule_files(&*second), ["nested/.gitignore"]);
    // `other/.git` removed: no longer a repository, so entered.
    std::fs::remove_dir_all(t.root().join("other/.git")).unwrap();
    t.write_index(t.common_dir(), &[]);
    let third = falls_back(&t, &second);
    assert_eq!(rule_files(&*third), ["other/.gitignore"]);
}

#[test]
fn watch_source_reuse_falls_back_near_the_walk_limit() {
    let t = TestRepo::new();
    for name in ["u1", "u2", "t1", "t2", "t3"] {
        t.write_file(name, b"x\n");
    }
    let tracked = [t.add("t1"), t.add("t2"), t.add("t3")];
    t.write_index(t.common_dir(), &tracked);
    let build = |previous: Option<&Filter>, limit: usize| {
        Filter::rebuild(&main_worktree(&t), &[], previous, limit, &no_cancel()).unwrap()
    };
    // Two untracked entries; the limit is 4.
    let first = build(None, 4);
    // One path untracked: at most 3 entries now, within the limit.
    t.write_index(t.common_dir(), &tracked[1..]);
    let second = build(Some(&first), 4);
    assert!(second.reused);
    // Two more: possibly 5 entries, beyond it. A walk tells.
    t.write_index(t.common_dir(), &[]);
    let third = build(Some(&second), 4);
    assert!(!third.reused);
    // A walk that stopped at its limit isn't reused.
    let capped = build(None, 1);
    t.write_index(t.common_dir(), &tracked);
    assert!(!build(Some(&capped), 1).reused);
}

#[test]
fn watch_source_reuse_falls_back_for_another_configuration_or_worktree() {
    let t = TestRepo::new();
    t.write_file("sub/.gitignore", b"*.s\n");
    t.write_file("a.txt", b"x\n");
    t.append_config("[core]\n\tignoreCase = false\n");
    let first = filter(&t);
    t.append_config("[core]\n\tignoreCase = true\n");
    t.write_index(t.common_dir(), &[t.add("a.txt")]);
    let second = falls_back(&t, &first);

    t.add_worktree("w", &t.base().join("w"), TestHead::Branch("refs/heads/w"));
    let linked = t.repo().worktree(&WorktreeId::Linked("w".into())).unwrap();
    let request = FilterRequest {
        gitignores: &[],
        previous: Some(&*second),
    };
    let other = linked
        .watch_source()
        .filter(&request, &no_cancel())
        .unwrap();
    assert!(
        !reused(&other),
        "the main worktree's walk isn't the linked one's"
    );
    assert!(rule_files(&*other).is_empty());
}

/// fs_watch over the real filter: after an index change (the walk reused),
/// a `.gitignore` added or changed in an untracked directory applies to the
/// events that follow, because its event makes fs_watch load without
/// `previous` (fs_watch design「路径分类」).
#[test]
fn watch_source_events_new_and_changed_gitignore_after_an_index_change() {
    use lectern_fs_watch::testing::{FakeBackend, RecordingSink, manual};
    use lectern_fs_watch::{
        Coalesce, Config, Flags, RepoSpec, WorktreeBatch, WorktreeName, WorktreeSpec,
    };

    let t = TestRepo::new();
    t.write_file(".gitignore", b"*.log\n");
    t.write_file("a.txt", b"x\n");
    t.write_index(t.common_dir(), &[t.add(".gitignore"), t.add("a.txt")]);
    let backend = FakeBackend::new();
    let (watcher, driver) = manual(
        backend.clone(),
        lectern_executor::Executor::manual(),
        Config::default(),
    );
    let repo_sink = RecordingSink::new(None);
    let repo = watcher.watch_repo(
        RepoSpec {
            common_dir: t.common_dir().to_path_buf(),
            coalesce: Coalesce::REPO,
        },
        repo_sink,
    );
    let sink = RecordingSink::<WorktreeBatch>::new(None);
    let _worktree = repo.watch_worktree(
        WorktreeSpec {
            name: WorktreeName::Main,
            root: t.root().to_path_buf(),
            source: main_worktree(&t).watch_source(),
            coalesce: Coalesce::WORKTREE,
        },
        sink.clone(),
    );
    let settle = || {
        driver.run_until_idle();
        driver.advance(std::time::Duration::from_secs(5));
    };
    let delivered = || -> Vec<String> {
        let mut paths: Vec<String> = sink
            .take()
            .into_iter()
            .flat_map(|(batch, _)| batch.paths)
            .map(|p| String::from_utf8_lossy(p.as_bytes()).into_owned())
            .collect();
        paths.sort();
        paths.dedup();
        paths
    };
    let file = Flags::FILE;
    settle();
    delivered();

    // `git add b.txt`: an index-only reload.
    t.write_file("b.txt", b"x\n");
    t.write_index(
        t.common_dir(),
        &[t.add(".gitignore"), t.add("a.txt"), t.add("b.txt")],
    );
    backend.emit(t.common_dir(), &[("index", file)]);
    settle();
    delivered();

    // A new `.gitignore` in an untracked directory, then files beneath it.
    t.write_file("gen/.gitignore", b"*.gen\n");
    backend.emit(t.root(), &[("gen/.gitignore", file | Flags::CREATED)]);
    settle();
    backend.emit(
        t.root(),
        &[
            ("gen/a.gen", file | Flags::CREATED),
            ("gen/b.txt", file | Flags::CREATED),
        ],
    );
    settle();
    assert_eq!(delivered(), ["gen/.gitignore", "gen/b.txt"]);

    // The index again (reused), then the `.gitignore` changed.
    t.write_index(t.common_dir(), &[t.add(".gitignore"), t.add("a.txt")]);
    backend.emit(t.common_dir(), &[("index", file)]);
    settle();
    delivered();
    t.write_file("gen/.gitignore", b"*.txt\n");
    backend.emit(t.root(), &[("gen/.gitignore", file | Flags::MODIFIED)]);
    settle();
    backend.emit(
        t.root(),
        &[
            ("gen/c.gen", file | Flags::CREATED),
            ("gen/d.txt", file | Flags::CREATED),
        ],
    );
    settle();
    assert_eq!(delivered(), ["gen/.gitignore", "gen/c.gen"]);

    // The index again, then a directory holding a `.gitignore` moved in:
    // only the directory is reported, and the walk finds the file.
    t.write_index(t.common_dir(), &[t.add(".gitignore"), t.add("b.txt")]);
    backend.emit(t.common_dir(), &[("index", file)]);
    settle();
    delivered();
    t.write_file("moved/.gitignore", b"*.mv\n");
    backend.emit(t.root(), &[("moved", Flags::DIR | Flags::RENAMED)]);
    settle();
    backend.emit(
        t.root(),
        &[
            ("moved/a.mv", file | Flags::CREATED),
            ("moved/b.txt", file | Flags::CREATED),
        ],
    );
    settle();
    assert_eq!(delivered(), ["moved", "moved/b.txt"]);
}
