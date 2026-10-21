//! The 100 000-file virtual repository of the `code_intel/*` and
//! `plugin/code-intel/*` benchmarks (index entries and stored synthetic
//! units, no worktree file). Written once and kept between runs under
//! `Ctx::persistent()/code_intel` (`VirtualTemplate::cached`: keyed by
//! everything its content depends on, checked before use), so the `time` and
//! `memory` targets of a run, and later runs, share it; within a target
//! process it is also shared read-only (`Ctx::shared`, ADR 0036). Each
//! benchmark, or each sample that needs a fresh one, opens its own copy: a
//! new `FakeGit` with the entries staged and a cache over a copy of the
//! written files, so nothing a rig writes (units, access times, evictions,
//! quota state) reaches the kept one or another benchmark.

use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use lectern_bench::{BenchError, Ctx};
use lectern_cache::{Budgets, Cache};
use lectern_code_intel::testing::{FakeGit, VirtualTemplate};

/// The popular name of `code_intel/goto_popular`: one file in
/// [`POPULAR_EVERY`] also defines it.
pub const POPULAR: &str = "fetchShared";
pub const POPULAR_EVERY: usize = 17;

/// The name of `code_intel/named_50k`: every other file defines it (some
/// 45 000 source files of 100 000).
pub const COMMON: &str = "id";
pub const COMMON_EVERY: usize = 2;

/// Both are part of the shared repository, so every benchmark's units carry
/// them; nothing else looks them up.
const NAMES: [(&str, usize); 2] = [(POPULAR, POPULAR_EVERY), (COMMON, COMMON_EVERY)];

/// The shared repository: 100 000 files (3000 in smoke mode).
pub fn shared(ctx: &Ctx) -> Result<Rc<VirtualTemplate>, BenchError> {
    let n = ctx.scale(100_000, 3_000);
    ctx.shared(&format!("code_intel::virtual_repo({n}, {NAMES:?})"), || {
        Ok(VirtualTemplate::cached(
            &ctx.persistent().join("code_intel"),
            n,
            &NAMES,
            Budgets::default(),
        )?)
    })
}

/// A copy of its own: the repository, and a cache rooted in `dir/cache`
/// (`dir` under `Ctx::scratch`, removed with it).
pub fn open(template: &VirtualTemplate, dir: &Path) -> Result<(Arc<FakeGit>, Cache), BenchError> {
    Ok(template.open(&dir.join("cache"), Budgets::default())?)
}
