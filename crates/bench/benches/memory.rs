//! In-process memory benchmarks: live bytes counted by `CountingAlloc`,
//! the global allocator of this target only (docs/design/bench.md「基准目标」).
//! `cache/memory` measures the resident size of a child process instead:
//! SQLite's memory is not allocated through `CountingAlloc`. The other
//! budgets of this target (`git/changes_capped_bytes`, `git/resident`) are
//! still planned; their benchmarks come with the git work that makes them
//! active.

#[path = "memory/cache.rs"]
mod cache;

use std::process::ExitCode;

use lectern_bench::{Bench, CountingAlloc, Target};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

const BENCHES: &[Bench] = &[cache::MEMORY];

fn main() -> ExitCode {
    // The child process of `cache/memory`.
    if let Some(root) = std::env::var_os(cache::CHILD) {
        let entries = cache::entries(std::env::args().nth(1));
        return cache::child(root.as_ref(), entries);
    }
    lectern_bench::main(Target::Memory, env!("CARGO_TARGET_TMPDIR"), BENCHES)
}
