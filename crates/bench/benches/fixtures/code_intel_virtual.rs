//! The 100 000-file virtual repository of the `code_intel/*` and
//! `plugin/code-intel/*` benchmarks (index entries and stored synthetic
//! units, no worktree file), written once per target process and shared
//! read-only (`Ctx::shared`, ADR 0036). Each benchmark, or each sample that
//! needs a fresh one, opens its own copy: a new `FakeGit` with the entries
//! staged and a cache over a copy of the written files, so nothing a rig
//! writes (units, access times, evictions, quota state) reaches the shared
//! one or another benchmark.

use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use lectern_bench::{BenchError, Ctx};
use lectern_cache::{Budgets, Cache};
use lectern_code_intel::testing::{FakeGit, VirtualTemplate};

/// The popular name of `code_intel/goto_popular`: one file in
/// [`POPULAR_EVERY`] also defines it. Part of the shared repository, so the
/// other benchmarks' units carry it too; nothing else looks it up.
pub const POPULAR: &str = "fetchShared";
pub const POPULAR_EVERY: usize = 17;

/// The shared repository: 100 000 files (3000 in smoke mode).
pub fn shared(ctx: &Ctx) -> Result<Rc<VirtualTemplate>, BenchError> {
    let n = ctx.scale(100_000, 3_000);
    ctx.shared(
        &format!("code_intel::virtual_repo({n}, {POPULAR}/{POPULAR_EVERY})"),
        || {
            Ok(VirtualTemplate::build(
                n,
                Some((POPULAR, POPULAR_EVERY)),
                Budgets::default(),
            )?)
        },
    )
}

/// A copy of its own: the repository, and a cache rooted in `dir/cache`
/// (`dir` under `Ctx::scratch`, removed with it).
pub fn open(template: &VirtualTemplate, dir: &Path) -> Result<(Arc<FakeGit>, Cache), BenchError> {
    Ok(template.open(&dir.join("cache"), Budgets::default())?)
}
