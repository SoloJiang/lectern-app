//! `git/*` (docs/design/git.md「性能」), on repositories built by
//! `lectern_git::testing::TestRepo` in the `cloudprint-miniapps` shape: 728
//! tracked files, 11 worktrees each with its own index.

use std::hint::black_box;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use lectern_bench::{Bench, BenchError, Ctx, Measured, Value};
use lectern_git::testing::{self, IndexEntry, TestHead, TestRepo};
use lectern_git::{EntryLimits, GitError, Limits, Mode, Oid, WorktreeId};

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
