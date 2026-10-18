//! `code_intel/*` in bytes (docs/design/code_intel.md「性能」): the map of
//! 100 000 files, and the temporary memory of indexing. Synthetic repositories from
//! `lectern_code_intel::testing::synthetic`.

use std::path::Path;
use std::sync::Arc;

use lectern_bench::{Bench, BenchError, CountingAlloc, Ctx, Measured, Value};
use lectern_cache::{Budgets, Cache, Config};
use lectern_code_intel::extract::LanguageTable;
use lectern_code_intel::map::{BaseMap, IndexedHints};
use lectern_code_intel::testing::{FakeGit, Rig, synthetic};
use lectern_code_intel::{NoGrammarPacks, SystemClock};
use lectern_services::code_intel::TierState;

const SAMPLES: usize = 3;

fn cache(root: &Path) -> Cache {
    Cache::new(Config {
        root: root.to_path_buf(),
        budgets: Budgets::default(),
        ui_thread: None,
    })
}

/// The live bytes of the map of 100 000 files: the base part and the
/// hints, not the file list (it belongs to `project`).
pub const MAP_100K_BYTES: Bench = Bench {
    id: "code_intel/map_100k_bytes",
    run: map_100k_bytes,
};

fn map_100k_bytes(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let n = ctx.scale(100_000, 5_000);
    let files = Arc::new(synthetic::file_list(n));
    let table = LanguageTable::core();
    let mut samples = Vec::new();
    for _ in 0..ctx.scale(SAMPLES, 1) {
        let before = CountingAlloc::live();
        let base = BaseMap::build(files.clone(), &table, &NoGrammarPacks);
        let hints = IndexedHints::new(files.len());
        samples.push(CountingAlloc::live().saturating_sub(before) as u64);
        drop((base, hints));
    }
    Ok(vec![Measured::new(
        "code_intel/map_100k_bytes",
        Value::Bytes(samples),
    )])
}

/// The peak of live bytes while the batch tiers index a repository of
/// 8 KiB files and a few files near the 1 MiB limit, from opening the
/// project until everything is indexed: each batch's reads, the syntax
/// trees (tree-sitter's allocations are routed through the counting
/// allocator), the write transactions' buffers and the map. The worktree
/// comparison is `FakeGit`'s; git's own `TrackedPaths` is measured by
/// `git/*`.
pub const BATCH_PEAK: Bench = Bench {
    id: "code_intel/batch_peak",
    run: batch_peak,
};

fn batch_peak(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let small = ctx.scale(400, 30);
    let large = ctx.scale(4, 1);
    let mut samples = Vec::new();
    for round in 0..ctx.scale(SAMPLES, 1) {
        let git = Arc::new(FakeGit::new());
        for i in 0..small {
            let ext = ["ts", "js", "rs", "py", "go", "java", "tsx"][i % 7];
            let source = synthetic::source(ext, i + round * 10_000, 8 << 10);
            git.add("main", &[(&format!("src/m{}/f{i}.{ext}", i % 9), &source)]);
        }
        for i in 0..large {
            let source = synthetic::source("ts", 900_000 + i + round * 10_000, 900 << 10);
            git.add("main", &[(&format!("big/f{i}.ts"), &source)]);
        }
        git.commit("main");
        let dir = tempfile::tempdir_in(ctx.scratch())?;
        let rig = Rig::new(git, cache(dir.path()), Arc::new(SystemClock));
        rig.open();
        let before = CountingAlloc::live();
        CountingAlloc::reset_peak();
        rig.settle();
        let peak = CountingAlloc::peak().saturating_sub(before);
        let key = rig.git.key();
        let coverage = rig
            .indexer
            .coverage(&key)
            .ok_or_else(|| BenchError::new("the project is not open"))?;
        if [coverage.subproject, coverage.worktree] != [TierState::Done; 2] {
            return Err(BenchError::new(format!("not indexed: {coverage:?}")));
        }
        let extracted = rig.indexer.stats().extractions;
        if extracted != small + large {
            return Err(BenchError::new(format!(
                "{extracted} extractions for {} files",
                small + large
            )));
        }
        samples.push(peak as u64);
        drop(rig);
    }
    Ok(vec![Measured::new(
        "code_intel/batch_peak",
        Value::Bytes(samples),
    )])
}
