//! In-process memory benchmarks: live bytes counted by `CountingAlloc`,
//! the global allocator of this target only (docs/design/bench.md「基准目标」).
//! The budgets of this target (`git/changes_capped_bytes`, `git/resident`)
//! are still planned; their benchmarks come with the git work that makes
//! them active.

use std::process::ExitCode;

use lectern_bench::{Bench, CountingAlloc, Target};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

const BENCHES: &[Bench] = &[];

fn main() -> ExitCode {
    lectern_bench::main(Target::Memory, env!("CARGO_TARGET_TMPDIR"), BENCHES)
}
