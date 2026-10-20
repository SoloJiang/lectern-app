//! In-process memory benchmarks: live bytes counted by `CountingAlloc`,
//! the global allocator of this target only (docs/design/bench.md「基准目标」).
//! `cache/memory` measures the resident size of a child process instead:
//! SQLite's memory is not allocated through `CountingAlloc`.

#[path = "memory/cache.rs"]
mod cache;
#[path = "memory/code_intel.rs"]
mod code_intel;
#[path = "fixtures/git_worktree.rs"]
mod git_worktree;
#[path = "memory/project.rs"]
mod project;
#[allow(
    dead_code,
    reason = "the index flip is for the time target's project/refresh_index"
)]
#[path = "fixtures/project_repo.rs"]
mod project_repo;

use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use lectern_bench::{Bench, BenchError, CountingAlloc, Ctx, Measured, Target, Value};
use lectern_git::{Limits, WorktreeId};

use git_worktree as worktree;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

const SAMPLES: usize = 3;

/// `git/resident`: this crate's live bytes with a repository of 11
/// worktrees open, the current worktree's handle and one change set of 50
/// edited files held (docs/design/git.md「性能」). Includes the hunks the
/// comparison adds to the process-wide hunk cache; each sample is a new
/// repository with contents new to that cache. The fixture's own bytes are
/// allocated before the count starts.
const GIT_RESIDENT: Bench = Bench {
    id: "git/resident",
    run: git_resident,
};

fn git_resident(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let files = ctx.scale(728, 60);
    let modules = ctx.scale(50, 5);
    let cancel = AtomicBool::new(false);
    let limits = Limits::default();
    let mut samples = Vec::new();
    for round in 0..ctx.scale(SAMPLES, 1) {
        let t = worktree::clean(files, modules, round + 1);
        worktree::edit_modules(&t, modules, round + 1, false);
        let root = t.root().to_owned();
        let before = CountingAlloc::live();
        let discovered = lectern_git::testing::discover(&root, &[])?
            .ok_or_else(|| BenchError::new("no repository"))?;
        let worktrees = discovered.repo.worktrees()?;
        let wt = discovered.repo.worktree(&WorktreeId::Main)?;
        let set = worktree::changes(&wt, &limits, &cancel)?;
        let held = CountingAlloc::live().saturating_sub(before);
        worktree::expect(&set, modules, 0)?;
        if worktrees.len() != 11 {
            return Err(BenchError::new(format!("{} worktrees", worktrees.len())));
        }
        samples.push(held as u64);
        drop((set, wt, worktrees, discovered));
    }
    Ok(vec![Measured::new("git/resident", Value::Bytes(samples))])
}

/// `git/changes_count/*`: a worktree with realistic changes
/// (`worktree::mixed`: modified tracked and untracked files of 10–50 KB,
/// one large untracked file) compared the way the project home counts
/// changes outside sessions (ADR 0035): `retained_bytes = 0` and
/// `total_patch_bytes = 0`, so the change set keeps no content and no
/// hunks. `total_bytes` (recorded) is the live bytes of that change set
/// with what its `Worktree` holds, what the home keeps at idle for
/// incremental refreshes. `per_path_bytes` (≤ 1 KB) is the same divided by
/// the changed paths: the stat cache keeps no file contents allocated and
/// the comparison's parsed index is dropped when the call returns.
const GIT_CHANGES_COUNT: Bench = Bench {
    id: "git/changes_count",
    run: git_changes_count,
};

fn git_changes_count(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (modified, untracked) = ctx.scale((1000, 1000), (10, 10));
    let large = ctx.scale(16 << 20, 64 << 10);
    let limits = Limits {
        retained_bytes: 0,
        total_patch_bytes: 0,
        ..Limits::default()
    };
    let t = worktree::mixed(modified, untracked, large);
    let paths = modified + untracked + 1;
    let cancel = AtomicBool::new(false);
    let mut samples = Vec::new();
    for _ in 0..ctx.scale(SAMPLES, 1) {
        let wt = worktree::main_worktree(&t)?;
        let before = CountingAlloc::live();
        let set = worktree::changes(&wt, &limits, &cancel)?;
        let held = CountingAlloc::live().saturating_sub(before);
        if set.files().len() != paths {
            return Err(BenchError::new(format!(
                "expected {paths} changed files, got {}",
                set.files().len()
            )));
        }
        samples.push(held as u64);
        drop((set, wt));
    }
    let per_path = samples.iter().map(|b| b / paths as u64).collect();
    Ok(vec![
        Measured::new("git/changes_count/total_bytes", Value::Bytes(samples)),
        Measured::new("git/changes_count/per_path_bytes", Value::Bytes(per_path)),
    ])
}

/// `git/changes_capped_bytes`: live bytes a change set of 10 000 untracked
/// files (5000 of them `Listed`) leaves behind, with the stat cache and the
/// parsed index of its `Worktree`. Recorded; its ceiling is the sum of the
/// `Limits` defaults.
const GIT_CHANGES_CAPPED_BYTES: Bench = Bench {
    id: "git/changes_capped_bytes",
    run: git_changes_capped_bytes,
};

fn git_changes_capped_bytes(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let files = ctx.scale(10_000, 60);
    let limits = Limits {
        total_files: ctx.scale(5000, 30),
        ..Limits::default()
    };
    let t = worktree::untracked(files);
    let cancel = AtomicBool::new(false);
    let mut samples = Vec::new();
    for _ in 0..ctx.scale(SAMPLES, 1) {
        let wt = worktree::main_worktree(&t)?;
        let before = CountingAlloc::live();
        let set = worktree::changes(&wt, &limits, &cancel)?;
        let held = CountingAlloc::live().saturating_sub(before);
        worktree::expect(&set, files, files - limits.total_files)?;
        samples.push(held as u64);
        drop((set, wt));
    }
    Ok(vec![Measured::new(
        "git/changes_capped_bytes",
        Value::Bytes(samples),
    )])
}

/// `git/filter_cloudprint_bytes`, `git/filter_100k_bytes`: what the
/// current worktree's fs_watch filter holds (`core.ignoreCase`, as on
/// macOS), with the `Worktree` handle it is built from: the parsed index
/// (`TrackedPaths`, the same `Arc` the handle caches and `project`'s kept
/// handle shares), the case-folded order and the ignore rules. git's
/// share (ADR 0035); fs_watch's and project's resident budgets exclude it
/// (docs/design/git.md「性能」). On the `cloudprint` shape (728 files) and
/// the large repository (100 000).
const GIT_FILTER_CLOUDPRINT_BYTES: Bench = Bench {
    id: "git/filter_cloudprint_bytes",
    run: git_filter_cloudprint_bytes,
};

fn git_filter_cloudprint_bytes(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let t = worktree::shaped(&lectern_project::testing::Shape::cloudprint());
    Ok(vec![Measured::new(
        "git/filter_cloudprint_bytes",
        Value::Bytes(filter_bytes(ctx, &t)?),
    )])
}

const GIT_FILTER_100K_BYTES: Bench = Bench {
    id: "git/filter_100k_bytes",
    run: git_filter_100k_bytes,
};

fn git_filter_100k_bytes(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let t = worktree::large_shared(ctx)?;
    Ok(vec![Measured::new(
        "git/filter_100k_bytes",
        Value::Bytes(filter_bytes(ctx, &t)?),
    )])
}

fn filter_bytes(ctx: &Ctx, t: &lectern_git::testing::TestRepo) -> Result<Vec<u64>, BenchError> {
    let cancel = AtomicBool::new(false);
    let mut samples = Vec::new();
    // Round 0 warms up gix's process-wide lazy state and is not reported.
    for round in 0..=ctx.scale(SAMPLES, 1) {
        let before = CountingAlloc::live();
        let discovered = lectern_git::testing::discover(t.root(), &[])?
            .ok_or_else(|| BenchError::new("no repository"))?;
        let wt = discovered.repo.worktree(&WorktreeId::Main)?;
        let source = wt.watch_source();
        let filter = source
            .filter(&[], &cancel)
            .map_err(|e| BenchError::new(e.0))?;
        let held = CountingAlloc::live().saturating_sub(before);
        if !filter.ignored(b"node_modules", true) {
            return Err(BenchError::new("the filter misses the .gitignore"));
        }
        if round > 0 {
            samples.push(held as u64);
        }
        drop((filter, source, wt, discovered));
    }
    Ok(samples)
}

/// `git/compare_100k/peak_bytes`: one cold `changes(Base::Head)` on the
/// clean large repository (100 000 tracked files, `core.ignoreCase`) with a
/// new `Worktree`; the peak of live bytes during the call (the comparison's
/// own parsed index, its case-folded copy, the walk, the change set),
/// docs/design/git.md「性能」, ADR 0035; and `…/retained_bytes`, what the
/// `Worktree` still holds once the change set is dropped (the live bytes
/// after `drop(set)` less those before the comparison): no parsed index is
/// kept between calls, only the stat cache of what was read.
const GIT_COMPARE_100K: Bench = Bench {
    id: "git/compare_100k",
    run: git_compare_100k,
};

fn git_compare_100k(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let t = worktree::large_shared(ctx)?;
    let cancel = AtomicBool::new(false);
    let limits = Limits::default();
    let (mut peaks, mut retained) = (Vec::new(), Vec::new());
    // Round 0 warms up gix's process-wide lazy state and is not reported.
    for round in 0..=ctx.scale(SAMPLES, 1) {
        let before = CountingAlloc::live();
        let discovered = lectern_git::testing::discover(t.root(), &[])?
            .ok_or_else(|| BenchError::new("no repository"))?;
        let wt = discovered.repo.worktree(&WorktreeId::Main)?;
        let opened = CountingAlloc::live();
        CountingAlloc::reset_peak();
        let set = worktree::changes(&wt, &limits, &cancel)?;
        let peak = CountingAlloc::peak().saturating_sub(before);
        worktree::expect(&set, 0, 0)?;
        drop(set);
        let kept = CountingAlloc::live().saturating_sub(opened);
        if round > 0 {
            peaks.push(peak as u64);
            retained.push(kept as u64);
        }
        drop((wt, discovered));
    }
    Ok(vec![
        Measured::new("git/compare_100k/peak_bytes", Value::Bytes(peaks)),
        Measured::new("git/compare_100k/retained_bytes", Value::Bytes(retained)),
    ])
}

const BENCHES: &[Bench] = &[
    cache::MEMORY,
    code_intel::MAP_100K_BYTES,
    code_intel::BATCH_PEAK,
    GIT_CHANGES_CAPPED_BYTES,
    GIT_CHANGES_COUNT,
    GIT_COMPARE_100K,
    GIT_FILTER_CLOUDPRINT_BYTES,
    GIT_FILTER_100K_BYTES,
    GIT_RESIDENT,
    project::RESIDENT_CLOUDPRINT,
    project::RESIDENT_100K,
];

fn main() -> ExitCode {
    // The child process of `cache/memory`.
    if let Some(root) = std::env::var_os(cache::CHILD) {
        let entries = cache::entries(std::env::args().nth(1));
        return cache::child(root.as_ref(), entries);
    }
    // SAFETY: first thing in the process, before any tree-sitter call: the
    // counting allocator then sees syntax trees (`code_intel/batch_peak`).
    unsafe { lectern_code_intel::testing::route_tree_sitter_allocations() };
    lectern_bench::main(Target::Memory, env!("CARGO_TARGET_TMPDIR"), BENCHES)
}
