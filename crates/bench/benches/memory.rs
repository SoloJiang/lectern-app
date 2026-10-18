//! In-process memory benchmarks: live bytes counted by `CountingAlloc`,
//! the global allocator of this target only (docs/design/bench.md「基准目标」).
//! `cache/memory` measures the resident size of a child process instead:
//! SQLite's memory is not allocated through `CountingAlloc`.

#[path = "memory/cache.rs"]
mod cache;
#[path = "memory/code_intel.rs"]
mod code_intel;
#[path = "fixtures/code_intel_names.rs"]
mod code_intel_names;
#[path = "fixtures/git_worktree.rs"]
mod git_worktree;

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

const BENCHES: &[Bench] = &[
    cache::MEMORY,
    code_intel::MAP_100K_BYTES,
    code_intel::BATCH_PEAK,
    code_intel::NAMES_100K_BYTES,
    code_intel::RESIDENT_CLOUDPRINT,
    code_intel::RESIDENT_100K,
    GIT_CHANGES_CAPPED_BYTES,
    GIT_RESIDENT,
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
