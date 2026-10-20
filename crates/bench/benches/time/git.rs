//! `git/*` (docs/design/git.md「性能」), on repositories built by
//! `lectern_git::testing::TestRepo` in the `cloudprint-miniapps` shape: 728
//! tracked files, 11 worktrees each with its own index; the `*_100k` ones
//! on the project design's large repository (`git_worktree::large`).

use std::hint::black_box;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use lectern_bench::{Bench, BenchError, Ctx, Measured, Value};
use lectern_git::testing::{self, IndexEntry, TestHead, TestRepo};
use lectern_git::{Base, EntryLimits, GitError, Limits, Mode, Oid, Refresh, RepoPath, WorktreeId};

use crate::git_worktree as worktree;

const FILES: usize = 728;
const WORKTREES: usize = 11;

/// 728 tracked files in 25 packages, the main worktree plus 10 linked ones.
fn cloudprint(files: usize) -> TestRepo {
    let t = TestRepo::new();
    let blob = t.blob(b"export const x = 1;\n");
    let entries: Vec<IndexEntry> = (0..files)
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
    for i in 0..WORKTREES - 1 {
        let branch = format!("refs/heads/wt{i}");
        t.set_ref(&branch, commit);
        let admin = t.add_worktree(
            &format!("wt{i}"),
            &t.base().join(format!("wt{i}")),
            TestHead::Branch(&branch),
        );
        t.write_index(&admin, &entries);
    }
    t
}

/// What opening a project reads from git; returns the worktree and file
/// counts.
fn open(root: &Path, cancel: &AtomicBool) -> Result<(usize, usize), GitError> {
    let Some(discovered) = testing::discover(root, &[])? else {
        return Ok((0, 0));
    };
    let worktrees = discovered.repo.worktrees()?;
    let main = discovered.repo.worktree(&WorktreeId::Main)?;
    let tracked = main.tracked(&EntryLimits::default(), cancel)?;
    Ok((worktrees.len(), tracked.len()))
}

/// `discover` + `worktrees` + `worktree(Main)` + `tracked`. Discovery reads
/// only the repository's own configuration (`testing::discover`); the
/// production path also reads the system and user files.
pub const OPEN: Bench = Bench {
    id: "git/open",
    run: open_bench,
};

fn open_bench(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let t = cloudprint(FILES);
    let cancel = AtomicBool::new(false);
    let counts = open(t.root(), &cancel)?;
    if counts != (WORKTREES, FILES) {
        return Err(BenchError::new(format!(
            "expected {WORKTREES} worktrees and {FILES} files, got {counts:?}"
        )));
    }
    let time = ctx.time(|| open(black_box(t.root()), &cancel));
    Ok(vec![Measured::new("git/open", Value::Time(time))])
}

/// Open one gix handle on the common directory and drop it: paid by every
/// top-level call.
pub const OPEN_HANDLE: Bench = Bench {
    id: "git/open_handle",
    run: open_handle,
};

fn open_handle(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let t = cloudprint(FILES);
    testing::open_handle(t.common_dir())?;
    let time = ctx.time(|| testing::open_handle(black_box(t.common_dir())));
    Ok(vec![Measured::new("git/open_handle", Value::Time(time))])
}

/// Contents never diffed before in this process: the hunk cache is global
/// and keyed by blob OID, so every cold round, retries included, needs new
/// contents. Round 0 belongs to the warm benchmark.
static ROUND: AtomicUsize = AtomicUsize::new(1);

/// A 300-line file; `edited` changes 10 of its lines.
fn file(n: usize, round: usize, edited: bool) -> Vec<u8> {
    let mut out = Vec::new();
    for line in 0..300 {
        let text = if edited && line % 30 == 7 {
            format!("    let changed_{n}_{line} = compute({round}, {line});\n")
        } else {
            format!("    let value_{n}_{line} = compute({round}, {line});\n")
        };
        out.extend_from_slice(text.as_bytes());
    }
    out
}

/// Trees of 728 files of which `changed` differ between old and new.
struct DiffFixture {
    t: TestRepo,
    unchanged: Vec<(String, Oid)>,
    changed: usize,
}

impl DiffFixture {
    fn new(ctx: &Ctx) -> DiffFixture {
        let changed = ctx.scale(50, 5);
        let t = TestRepo::new();
        let unchanged = (0..ctx.scale(FILES - 50, 50))
            .map(|i| {
                (
                    format!("packages/p{:02}/src/file{i:03}.ts", i % 25),
                    t.blob(&file(1000 + i, 0, false)),
                )
            })
            .collect();
        DiffFixture {
            t,
            unchanged,
            changed,
        }
    }

    /// The old and new trees of `round`.
    fn trees(&self, round: usize) -> (Oid, Oid) {
        let changed: Vec<(String, Oid, Oid)> = (0..self.changed)
            .map(|n| {
                (
                    format!("src/module{n:02}.rs"),
                    self.t.blob(&file(n, round, false)),
                    self.t.blob(&file(n, round, true)),
                )
            })
            .collect();
        let side = |new: bool| {
            let mut entries: Vec<(&str, Mode, Oid)> = self
                .unchanged
                .iter()
                .map(|(p, o)| (p.as_str(), Mode::File, *o))
                .collect();
            entries.extend(changed.iter().map(|(p, old, edited)| {
                (p.as_str(), Mode::File, if new { *edited } else { *old })
            }));
            self.t.tree_entries(&entries)
        };
        (side(false), side(true))
    }

    fn check(&self, set: &lectern_git::ChangeSet) -> Result<(), BenchError> {
        let lines = self.changed as u64 * 10;
        if set.files().len() != self.changed || set.totals().added != lines {
            return Err(BenchError::new(format!(
                "expected {} changed files with {} added lines, got {} and {}",
                self.changed,
                lines,
                set.files().len(),
                set.totals().added
            )));
        }
        Ok(())
    }
}

/// The whole `diff_trees` (trees, attributes, rename detection, blobs,
/// hunks) of 50 files of 300 lines with 10 changed lines each, every hunk
/// a cache miss. More than the hunks alone: it includes reading the trees.
pub const DIFF_50_COLD: Bench = Bench {
    id: "git/diff_50_cold",
    run: diff_50_cold,
};

fn diff_50_cold(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let fixture = DiffFixture::new(ctx);
    let repo = fixture.t.repo();
    let cancel = AtomicBool::new(false);
    let limits = Limits::default();
    let (old, new) = fixture.trees(ROUND.fetch_add(1, Ordering::Relaxed));
    fixture.check(&*repo.diff_trees(old, new, &limits, &cancel)?)?;
    let time = ctx.time_with_setup(
        || fixture.trees(ROUND.fetch_add(1, Ordering::Relaxed)),
        |(old, new)| repo.diff_trees(old, new, &limits, &cancel),
    );
    Ok(vec![Measured::new("git/diff_50_cold", Value::Time(time))])
}

/// The same `diff_trees` again: every hunk from the cache.
pub const DIFF_50_WARM: Bench = Bench {
    id: "git/diff_50_warm",
    run: diff_50_warm,
};

fn diff_50_warm(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let fixture = DiffFixture::new(ctx);
    let repo = fixture.t.repo();
    let cancel = AtomicBool::new(false);
    let limits = Limits::default();
    let (old, new) = fixture.trees(0);
    fixture.check(&*repo.diff_trees(old, new, &limits, &cancel)?)?;
    let time = ctx.time(|| repo.diff_trees(black_box(old), black_box(new), &limits, &cancel));
    Ok(vec![Measured::new("git/diff_50_warm", Value::Time(time))])
}

/// `Worktree::changes(Base::Head)` on a clean worktree of 728 tracked files
/// in 11 worktrees: every file skipped by stat, plus the untracked walk.
/// The same `Worktree` each time (its parsed index is reused).
pub const CHANGES_CLEAN: Bench = Bench {
    id: "git/changes_clean",
    run: changes_clean,
};

fn changes_clean(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let t = worktree::clean(ctx.scale(FILES, 60), ctx.scale(50, 5), 0);
    let wt = worktree::main_worktree(&t)?;
    let cancel = AtomicBool::new(false);
    let limits = Limits::default();
    worktree::expect(&*worktree::changes(&wt, &limits, &cancel)?, 0, 0)?;
    let time = ctx.time(|| wt.changes(Base::Head, &limits, &cancel));
    Ok(vec![Measured::new("git/changes_clean", Value::Time(time))])
}

/// The same with 50 files of about 300 lines edited in 10 lines: those 50
/// are read through `SafeRoot`, converted and hashed every time (their
/// stat differs from the index and no earlier change set is held); the
/// hunks come from the cache after the first call.
pub const CHANGES_50: Bench = Bench {
    id: "git/changes_50",
    run: changes_50,
};

fn changes_50(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let modules = ctx.scale(50, 5);
    let t = worktree::clean(ctx.scale(FILES, 60), modules, 0);
    worktree::edit_modules(&t, modules, 0, false);
    let wt = worktree::main_worktree(&t)?;
    let cancel = AtomicBool::new(false);
    let limits = Limits::default();
    worktree::expect(&*worktree::changes(&wt, &limits, &cancel)?, modules, 0)?;
    let time = ctx.time(|| wt.changes(Base::Head, &limits, &cancel));
    Ok(vec![Measured::new("git/changes_50", Value::Time(time))])
}

/// The 50-file worktree with its change set, the edits more than a second
/// old (so a refresh holding the change set takes them from the stat cache).
struct Refreshing {
    t: TestRepo,
    wt: lectern_git::Worktree,
    prev: std::sync::Arc<lectern_git::ChangeSet>,
    modules: usize,
}

impl Refreshing {
    fn new(ctx: &Ctx) -> Result<Refreshing, BenchError> {
        let modules = ctx.scale(50, 5);
        let t = worktree::clean(ctx.scale(FILES, 60), modules, 0);
        worktree::edit_modules(&t, modules, 0, true);
        let wt = worktree::main_worktree(&t)?;
        let prev = worktree::changes(&wt, &Limits::default(), &AtomicBool::new(false))?;
        worktree::expect(&prev, modules, 0)?;
        Ok(Refreshing {
            t,
            wt,
            prev,
            modules,
        })
    }
}

/// `refresh(Full)` of the 50-file change set, holding it: everything is
/// compared again, the edited files from the stat cache.
pub const REFRESH_FULL: Bench = Bench {
    id: "git/refresh_full",
    run: refresh_full,
};

fn refresh_full(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let r = Refreshing::new(ctx)?;
    let cancel = AtomicBool::new(false);
    worktree::expect(
        &*r.wt.refresh(&r.prev, Refresh::Full, &cancel)?,
        r.modules,
        0,
    )?;
    let time = ctx.time(|| r.wt.refresh(&r.prev, Refresh::Full, &cancel));
    Ok(vec![Measured::new("git/refresh_full", Value::Time(time))])
}

/// `refresh(Paths)` of one file edited just before each sample, from the
/// previous change set.
pub const REFRESH_ONE: Bench = Bench {
    id: "git/refresh_one",
    run: refresh_one,
};

fn refresh_one(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    use std::cell::{Cell, RefCell};

    let r = Refreshing::new(ctx)?;
    let cancel = AtomicBool::new(false);
    let path = RepoPath::new(worktree::module_path(0).as_bytes())
        .ok_or_else(|| BenchError::new("module path"))?;
    let held = RefCell::new(std::sync::Arc::clone(&r.prev));
    let round = Cell::new(0usize);
    let time = ctx.time_with_setup(
        || {
            round.set(round.get() + 1);
            let mut content = worktree::module(0, 0, true);
            content.extend_from_slice(format!("// round {}\n", round.get()).as_bytes());
            r.t.write_file(&worktree::module_path(0), &content);
        },
        |()| -> Result<(), GitError> {
            let next = r.wt.refresh(
                &held.borrow(),
                Refresh::Paths(std::slice::from_ref(&path)),
                &cancel,
            )?;
            *held.borrow_mut() = next;
            Ok(())
        },
    );
    worktree::expect(&held.borrow(), r.modules, 0)?;
    Ok(vec![Measured::new("git/refresh_one", Value::Time(time))])
}

/// 10 000 untracked files, past the total limit of 5000 files: the rest
/// are `Listed`.
pub const CHANGES_CAPPED: Bench = Bench {
    id: "git/changes_capped",
    run: changes_capped,
};

fn changes_capped(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let files = ctx.scale(10_000, 60);
    let limits = Limits {
        total_files: ctx.scale(5000, 30),
        ..Limits::default()
    };
    let t = worktree::untracked(files);
    let wt = worktree::main_worktree(&t)?;
    let cancel = AtomicBool::new(false);
    let listed = files - limits.total_files;
    worktree::expect(&*worktree::changes(&wt, &limits, &cancel)?, files, listed)?;
    let time = ctx.time(|| wt.changes(Base::Head, &limits, &cancel));
    Ok(vec![Measured::new("git/changes_capped", Value::Time(time))])
}

/// The large repository (`worktree::large`, 100 000 tracked files) for the
/// `git/*_100k` benchmarks, shared by them; a few thousand files in smoke
/// mode.
fn large(ctx: &Ctx) -> Result<std::rc::Rc<TestRepo>, BenchError> {
    worktree::large_shared(ctx)
}

/// `git/open` on the large repository: what opening it reads from git
/// before the file list is built, the index of 100 000 entries parsed
/// every time (a new `Worktree`, nothing cached). Part of the project
/// design's `project/open_100k/files`.
pub const OPEN_100K: Bench = Bench {
    id: "git/open_100k",
    run: open_100k,
};

fn open_100k(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let t = large(ctx)?;
    let files = ctx.scale(100_000, 2_000);
    let cancel = AtomicBool::new(false);
    let counts = open(t.root(), &cancel)?;
    if counts != (1, files) {
        return Err(BenchError::new(format!(
            "expected 1 worktree and {files} files, got {counts:?}"
        )));
    }
    let time = ctx.time(|| open(black_box(t.root()), &cancel));
    Ok(vec![Measured::new("git/open_100k", Value::Time(time))])
}

/// Whether the budget `id` is active. `git/filter_100k` is planned while
/// its macOS gap is open (git design「与设计的差异」); its benchmark
/// produces it again, unchanged, once the fix makes it active.
fn active(id: &str) -> bool {
    use lectern_bench::budgets::{BUDGETS, State, lookup};
    lookup(BUDGETS, id).is_some_and(|b| b.state == State::Active)
}

/// The tracked paths of `large(ctx)`.
fn large_paths(ctx: &Ctx) -> Vec<String> {
    let (files, manifests) = ctx.scale((100_000, 2_000), (2_000, 40));
    lectern_project::testing::Shape::monorepo(files, manifests).paths
}

fn rel(path: &str) -> Result<lectern_platform::RelPath<'_>, BenchError> {
    lectern_platform::RelPath::new(path.as_bytes())
        .ok_or_else(|| BenchError::new(format!("not a relative path: {path}")))
}

/// Every directory of the worktree listed through `SafeRoot::list`,
/// descending into directories except `.git`, as the untracked walk does
/// (without its ignore rules and index lookups). Returns the entries seen.
fn walk(root: &lectern_platform::SafeRoot) -> Result<usize, BenchError> {
    let mut seen = 0;
    let mut dirs: Vec<Vec<u8>> = vec![Vec::new()];
    while let Some(dir) = dirs.pop() {
        let listed = if dir.is_empty() {
            root.list(None)
        } else {
            let path = lectern_platform::RelPath::new(&dir)
                .ok_or_else(|| BenchError::new("not a relative path"))?;
            root.list(Some(path))
        }
        .map_err(|e| BenchError::new(format!("{e:?}")))?;
        for entry in listed {
            seen += 1;
            if entry.kind == lectern_platform::FileKind::Dir && entry.name != b".git" {
                let mut child = dir.clone();
                if !child.is_empty() {
                    child.push(b'/');
                }
                child.extend_from_slice(&entry.name);
                dirs.push(child);
            }
        }
    }
    Ok(seen)
}

/// The fs_watch filter rebuilt after the index changed (fs_watch design
/// 「路径分类」): `WorktreeSource::filter` on a `Worktree` whose index is
/// not parsed yet, so the index is parsed, the case-folded order sorted
/// (`core.ignoreCase`), the untracked walk lists every directory and the
/// `.gitignore` files are read. The filter of each sample is dropped with
/// its `Worktree`, untimed. Planned for now (`active`); always recorded:
/// `git/filter_100k/walk`, one `walk` of the same worktree.
pub const FILTER_100K: Bench = Bench {
    id: "git/filter_100k",
    run: filter_100k,
};

fn filter_100k(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let t = large(ctx)?;
    let root = lectern_platform::SafeRoot::open(t.root())?;
    let mut out = Vec::new();
    if walk(&root)? < large_paths(ctx).len() {
        return Err(BenchError::new("the walk missed files"));
    }
    // Record-only diagnostic: one sample, warmed by the check above.
    let time = ctx.once(|| walk(black_box(&root)));
    out.push(Measured::new("git/filter_100k/walk", Value::Time(time)));
    if active("git/filter_100k") {
        out.push(filter_100k_full(ctx, &t)?);
    }
    Ok(out)
}

fn filter_100k_full(ctx: &mut Ctx, t: &TestRepo) -> Result<Measured, BenchError> {
    use std::cell::Cell;

    let repo = t.repo();
    let cancel = AtomicBool::new(false);
    let filter = repo
        .worktree(&WorktreeId::Main)?
        .watch_source()
        .filter(&[], &cancel)
        .map_err(|e| BenchError::new(e.0))?;
    if !filter.ignored(b"node_modules", true) || filter.rule_files().len() != 1 {
        return Err(BenchError::new("the filter misses the .gitignore"));
    }
    drop(filter);
    let failed = Cell::new(false);
    let time = ctx.time_with_setup(
        || repo.worktree(&WorktreeId::Main),
        |wt| {
            let built = wt.map(|wt| (wt.watch_source().filter(&[], &cancel), wt));
            if !matches!(built, Ok((Ok(_), _))) {
                failed.set(true);
            }
            built
        },
    );
    if failed.get() {
        return Err(BenchError::new("building a filter failed"));
    }
    Ok(Measured::new("git/filter_100k", Value::Time(time)))
}

/// `git/changes_clean` on the large repository: 100 000 tracked files
/// skipped by stat, plus the untracked walk. Cold: a new `Worktree` for
/// each sample (made untimed), so every comparison parses the index, as
/// every one does once a `Worktree` keeps no parsed index between calls
/// (ADR 0035, git design step 4). Always recorded besides, through the
/// platform calls the comparison makes:
/// `git/changes_clean_100k/stat_all`, one `SafeRoot::stat` of every
/// tracked path; and counts: `…/reads`, the files the first comparison
/// read (the untracked walk's `.gitignore` included), and `…/stat_*`, the
/// index entries whose stat differs from `SafeRoot::stat` per field
/// (`testing::stat_mismatches`) before any read. The sampled read of
/// every tracked path (`read_all`, 14.6 s a sample on macOS CI) is gone:
/// the counts tell whether and why the comparison reads.
pub const CHANGES_CLEAN_100K: Bench = Bench {
    id: "git/changes_clean_100k",
    run: changes_clean_100k,
};

fn changes_clean_100k(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let t = large(ctx)?;
    let paths = large_paths(ctx);
    let root = lectern_platform::SafeRoot::open(t.root())?;
    let stat_all = || -> Result<(), BenchError> {
        for path in &paths {
            black_box(
                root.stat(rel(path)?)
                    .map_err(|e| BenchError::new(format!("{e:?}")))?,
            );
        }
        Ok(())
    };
    let repo = t.repo();
    let cancel = AtomicBool::new(false);
    let limits = Limits::default();
    // Before any read: how the index stats compare, field by field, and
    // how many files the first comparison reads (the `.gitignore` of the
    // untracked walk included).
    let stat = testing::stat_mismatches(&repo.worktree(&WorktreeId::Main)?)?;
    let wt = repo.worktree(&WorktreeId::Main)?;
    let before = testing::worktree_reads();
    worktree::expect(&*worktree::changes(&wt, &limits, &cancel)?, 0, 0)?;
    let read = testing::worktree_reads() - before;
    drop(wt);
    // One more cold comparison (a new `Worktree`), timed by phase; the
    // first one warmed the page cache. `phase_rest` is what the phases
    // don't cover: dropping the comparison's temporary state, the call.
    let wt = repo.worktree(&WorktreeId::Main)?;
    let started = std::time::Instant::now();
    let set = worktree::changes(&wt, &limits, &cancel)?;
    let total = started.elapsed();
    worktree::expect(&set, 0, 0)?;
    drop((set, wt));
    let phases = testing::compare_phases();
    let covered: std::time::Duration = phases.iter().map(|(_, d)| *d).sum();
    let mut timed: Vec<(String, std::time::Duration)> = phases
        .into_iter()
        .map(|(name, d)| (format!("git/changes_clean_100k/phase_{name}"), d))
        .collect();
    timed.push((
        "git/changes_clean_100k/phase_rest".to_owned(),
        total.saturating_sub(covered),
    ));
    let mut out = vec![Measured::new(
        "git/changes_clean_100k/reads",
        Value::Count(read as u64),
    )];
    for (name, n) in [
        ("racy", stat.racy),
        ("unmatched", stat.unmatched),
        ("mtime", stat.mtime),
        ("size", stat.size),
        ("ctime", stat.ctime),
        ("ino", stat.ino),
        ("owner", stat.owner),
    ] {
        out.push(Measured::new(
            format!("git/changes_clean_100k/stat_{name}"),
            Value::Count(n as u64),
        ));
    }
    // One sample each, as `Ctx::once` takes for a diagnostic; built by hand
    // because the one call gives all twelve values.
    out.extend(
        timed
            .into_iter()
            .map(|(id, d)| Measured::new(id, Value::Time(lectern_bench::Samples(vec![d])))),
    );
    stat_all()?;
    out.push(Measured::new(
        "git/changes_clean_100k/stat_all",
        // Record-only diagnostic: one sample, warmed by the call above.
        Value::Time(ctx.once(stat_all)),
    ));
    if active("git/changes_clean_100k") {
        out.push(changes_clean_100k_full(ctx, &t)?);
    }
    Ok(out)
}

fn changes_clean_100k_full(ctx: &mut Ctx, t: &TestRepo) -> Result<Measured, BenchError> {
    use std::cell::Cell;

    let repo = t.repo();
    let cancel = AtomicBool::new(false);
    let limits = Limits::default();
    let wt = repo.worktree(&WorktreeId::Main)?;
    worktree::expect(&*worktree::changes(&wt, &limits, &cancel)?, 0, 0)?;
    drop(wt);
    let failed = Cell::new(false);
    let time = ctx.time_with_setup(
        || repo.worktree(&WorktreeId::Main),
        |wt| {
            let set = wt.map(|wt| (wt.changes(Base::Head, &limits, &cancel), wt));
            if !matches!(set, Ok((Ok(_), _))) {
                failed.set(true);
            }
            set
        },
    );
    if failed.get() {
        return Err(BenchError::new("a comparison failed"));
    }
    Ok(Measured::new("git/changes_clean_100k", Value::Time(time)))
}

/// `git/refresh_one` on the large repository, recorded: `refresh(Paths)`
/// of one tracked path from the clean change set, as for one fs_watch
/// event. The path is unchanged (the shared repository stays read-only),
/// so the cost is what every such refresh pays: the index parsed again
/// (no parsed index is kept between calls, ADR 0035), the path's stat,
/// the scoped walk.
pub const REFRESH_ONE_100K: Bench = Bench {
    id: "git/refresh_one_100k",
    run: refresh_one_100k,
};

fn refresh_one_100k(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let t = large(ctx)?;
    // An ordinary file: a `.gitignore` or `.gitattributes` would make it a
    // full comparison.
    let path = large_paths(ctx)
        .into_iter()
        .find(|p| {
            !p.rsplit('/')
                .next()
                .is_some_and(|name| name.starts_with('.'))
        })
        .and_then(|p| RepoPath::new(p.as_bytes()))
        .ok_or_else(|| BenchError::new("no tracked path"))?;
    let wt = worktree::main_worktree(&t)?;
    let cancel = AtomicBool::new(false);
    let prev = worktree::changes(&wt, &Limits::default(), &cancel)?;
    worktree::expect(&prev, 0, 0)?;
    let paths = std::slice::from_ref(&path);
    worktree::expect(&*wt.refresh(&prev, Refresh::Paths(paths), &cancel)?, 0, 0)?;
    let time = ctx.time(|| wt.refresh(&prev, Refresh::Paths(paths), &cancel));
    Ok(vec![Measured::new(
        "git/refresh_one_100k",
        Value::Time(time),
    )])
}

/// `Worktree::canonical_oid` on the large repository, recorded: one call on
/// a new `Worktree` (made untimed), as code_intel's git access makes it
/// for each file whose raw OID differs from the file list's; the index is
/// parsed for the call. `…/batch_100`: `canonical_oids` for 100 paths on a
/// new `Worktree`, one parse for all of them. The bytes are CRLF, so the
/// conversion runs; the repository stays read-only.
pub const CANONICAL_OID_100K: Bench = Bench {
    id: "git/canonical_oid_100k",
    run: canonical_oid_100k,
};

fn canonical_oid_100k(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    use std::cell::Cell;

    let t = large(ctx)?;
    let repo = t.repo();
    let paths: Vec<RepoPath> = large_paths(ctx)
        .iter()
        .filter_map(|p| RepoPath::new(p.as_bytes()))
        .take(100)
        .collect();
    let bytes: &[u8] = b"line one\r\nline two\r\n";
    let items: Vec<(&RepoPath, &[u8])> = paths.iter().map(|p| (p, bytes)).collect();
    let wt = repo.worktree(&WorktreeId::Main)?;
    if wt.canonical_oids(&items)?.len() != paths.len() {
        return Err(BenchError::new("one OID per path"));
    }
    drop(wt);
    let failed = Cell::new(false);
    let one = ctx.time_with_setup(
        || repo.worktree(&WorktreeId::Main),
        |wt| {
            let oid = wt.map(|wt| (wt.canonical_oid(&paths[0], bytes), wt));
            if !matches!(oid, Ok((Ok(Some(_)), _))) {
                failed.set(true);
            }
            oid
        },
    );
    let batch = ctx.time_with_setup(
        || repo.worktree(&WorktreeId::Main),
        |wt| {
            let oids = wt.map(|wt| (wt.canonical_oids(&items), wt));
            if !matches!(&oids, Ok((Ok(v), _)) if v.len() == items.len()) {
                failed.set(true);
            }
            oids
        },
    );
    if failed.get() {
        return Err(BenchError::new("canonical_oid failed"));
    }
    Ok(vec![
        Measured::new("git/canonical_oid_100k", Value::Time(one)),
        Measured::new("git/canonical_oid_100k/batch_100", Value::Time(batch)),
    ])
}
