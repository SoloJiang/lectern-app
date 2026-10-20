//! `code_intel/*` in bytes (docs/design/code_intel.md「性能」): the map of
//! 100 000 files, and the temporary memory of indexing. Synthetic repositories from
//! `lectern_code_intel::testing::synthetic`.

use std::path::Path;
use std::sync::Arc;

use lectern_bench::{Bench, BenchError, CountingAlloc, Ctx, Measured, Value};
use lectern_cache::{Budgets, Cache, Config};
use lectern_code_intel::extract::LanguageTable;
use lectern_code_intel::map::{BaseMap, IndexedHints};
use lectern_code_intel::names::build_table;
use lectern_code_intel::testing::{FakeGit, Rig, packages, synthetic, virtual_repo};
use lectern_code_intel::{NoGrammarPacks, SystemClock};
use lectern_services::code_intel::{Position, Query, QueryKind, TextOrigin, TextRef, TierState};

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

/// The ⌘K name table of 100 000 files (≤ 8 MB): its live bytes once built.
pub const NAMES_100K_BYTES: Bench = Bench {
    id: "code_intel/names_100k_bytes",
    run: names_100k_bytes,
};

fn names_100k_bytes(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let dir = tempfile::tempdir_in(ctx.scratch())?;
    let f = crate::code_intel_names::names(ctx, dir.path())?;
    let mut samples = Vec::new();
    for _ in 0..ctx.scale(SAMPLES, 1) {
        let before = CountingAlloc::live();
        let table = build_table(&f.store, &f.map, &f.table, 1, &|| false)
            .ok_or_else(|| BenchError::new("no table"))?;
        samples.push(CountingAlloc::live().saturating_sub(before) as u64);
        eprintln!("code_intel/names_100k_bytes: {} names", table.len());
        drop(table);
    }
    Ok(vec![Measured::new(
        "code_intel/names_100k_bytes",
        Value::Bytes(samples),
    )])
}

/// A [`resident`] sample's repository, its cache and its number of files.
type Resident = Result<(Arc<FakeGit>, Cache, usize), BenchError>;

/// This crate's resident bytes with a project indexed: a warm start over
/// virtual files (index entries and stored units), one jump made (its
/// grammar compiled) and closed. The map, the hints, tier 5's confirmed
/// set, the compiled grammar, the project's state; not the file list
/// (`project`'s) and not SQLite's own memory (the cache's). Every sample is a
/// new rig over a repository and cache of its own (`fixture`, given a
/// directory under the scratch directory: the repository, its cache, its
/// number of files).
fn resident(
    ctx: &Ctx,
    id: &'static str,
    fixture: &dyn Fn(&Path) -> Resident,
) -> Result<Vec<Measured>, BenchError> {
    let mut samples = Vec::new();
    for _ in 0..ctx.scale(SAMPLES, 1) {
        let dir = tempfile::tempdir_in(ctx.scratch())?;
        let (git, cache, n) = fixture(dir.path())?;
        let path = "packages/pkg000/src/mod00/current.ts";
        let name = synthetic::shared_name();
        let mut text = synthetic::source("ts", 999_999, 16 << 10);
        text.extend_from_slice(
            format!("\nexport function jump(x: number) {{ return {name}(x); }}\n").as_bytes(),
        );
        git.add("main", &[(path, &text)]);
        git.commit("main");
        let rig = Rig::new(git, cache, Arc::new(SystemClock));
        let event = rig.projects.set(rig.git.snapshot(
            1,
            "main",
            packages(&synthetic::package_dirs(n)),
        ));
        // `project`'s subscription comes first: the worktree state is
        // fs_watch's.
        let _project = rig.watch_as_project();
        let before = CountingAlloc::live();
        rig.indexer.project_event(&event);
        rig.settle();
        let at = TextRef {
            worktree: Some(rig.git.worktree_key("main")),
            path: path.into(),
            oid: rig.git.oid(&text),
            origin: TextOrigin::Worktree,
        };
        let call = format!("{name}(x)");
        let offset = text
            .windows(call.len())
            .position(|w| w == call.as_bytes())
            .ok_or_else(|| BenchError::new("no call"))?;
        let line = text[..offset].iter().filter(|b| **b == b'\n').count() as u32;
        let start = text[..offset]
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(0, |i| i + 1);
        let q = rig.indexer.query(Query {
            project: rig.git.key(),
            kind: QueryKind::Definition {
                at,
                position: Position {
                    line,
                    column: (offset - start) as u32,
                },
            },
        });
        rig.settle();
        let r = rig
            .indexer
            .results(q)
            .ok_or_else(|| BenchError::new("no results"))?;
        if r.candidates.is_empty() {
            return Err(BenchError::new(format!("no candidates: {:?}", r.state)));
        }
        drop(r);
        rig.indexer.close(q);
        rig.settle();
        // The jump's text's parse is kept until it goes unused for
        // `PARSE_CACHE_KEEP_FOR` (ADR 0037): what stays resident is measured
        // without it; `code_intel/parse_cache_bytes` measures it.
        rig.indexer.clear_kept_parses();
        let coverage = rig
            .indexer
            .coverage(&rig.git.key())
            .ok_or_else(|| BenchError::new("the project is not open"))?;
        let tiers = [
            coverage.subproject,
            coverage.worktree,
            coverage.other_worktrees,
        ];
        if tiers != [TierState::Done; 3] {
            return Err(BenchError::new(format!("not indexed: {coverage:?}")));
        }
        samples.push(CountingAlloc::live().saturating_sub(before) as u64);
        drop(rig);
    }
    Ok(vec![Measured::new(id, Value::Bytes(samples))])
}

/// The kept parses of current texts at their bounds (ADR 0037): two
/// 100 KB TypeScript texts made the current one in turn, so both parses
/// are kept; their live bytes (the syntax trees through tree-sitter's
/// allocator, the binding models, the reference positions) are what
/// forgetting them frees. Against `PARSE_CACHE_BYTES`; also prints the
/// bytes per source byte the cache's estimate uses.
pub const PARSE_CACHE_BYTES: Bench = Bench {
    id: "code_intel/parse_cache_bytes",
    run: parse_cache_bytes,
};

fn parse_cache_bytes(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let mut samples = Vec::new();
    for round in 0..ctx.scale(SAMPLES, 1) {
        let git = Arc::new(FakeGit::new());
        let dir = tempfile::tempdir_in(ctx.scratch())?;
        let cache = cache(dir.path());
        virtual_repo(&git, &cache, 728, &["main"]);
        let texts: Vec<(String, Vec<u8>)> = (0..lectern_code_intel::query::PARSE_CACHE_ENTRIES)
            .map(|i| {
                let path = format!("packages/pkg000/src/mod00/current{i}.ts");
                (
                    path,
                    synthetic::source("ts", 990_000 + i + 10 * round, 100 << 10),
                )
            })
            .collect();
        for (path, text) in &texts {
            git.add("main", &[(path, text)]);
        }
        git.commit("main");
        let rig = Rig::new(git, cache, Arc::new(SystemClock));
        rig.open_with(packages(&synthetic::package_dirs(728)));
        rig.settle();
        for (path, text) in &texts {
            rig.indexer.set_focus(
                &rig.git.key(),
                Some(&rig.git.worktree_key("main")),
                lectern_services::code_intel::Focus::Current(Some(TextRef {
                    worktree: Some(rig.git.worktree_key("main")),
                    path: path.as_str().into(),
                    oid: rig.git.oid(text),
                    origin: TextOrigin::Worktree,
                })),
            );
            rig.settle();
        }
        let (kept, estimated) = rig.indexer.kept_parses();
        if kept != texts.len() {
            return Err(BenchError::new(format!("{kept} parses kept")));
        }
        let with = CountingAlloc::live();
        rig.indexer.clear_kept_parses();
        let bytes = with.saturating_sub(CountingAlloc::live()) as u64;
        let source: usize = texts.iter().map(|(_, t)| t.len()).sum();
        eprintln!(
            "code_intel/parse_cache_bytes: {bytes} bytes for {source} source bytes ({:.1} per byte), estimated {estimated}",
            bytes as f64 / source as f64
        );
        samples.push(bytes);
        drop(rig);
    }
    Ok(vec![Measured::new(
        "code_intel/parse_cache_bytes",
        Value::Bytes(samples),
    )])
}

/// The cloudprint shape: 728 tracked files, 11 worktrees (≤ 4 MB).
pub const RESIDENT_CLOUDPRINT: Bench = Bench {
    id: "plugin/code-intel/resident_cloudprint",
    run: resident_cloudprint,
};

fn resident_cloudprint(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let n = 728;
    let fixture = |dir: &Path| -> Resident {
        let git = Arc::new(FakeGit::new());
        let mut worktrees = vec!["main".to_owned()];
        for w in 0..10 {
            let name = format!("w{w}");
            git.add_worktree(&name);
            worktrees.push(name);
        }
        let cache = cache(dir);
        let names: Vec<&str> = worktrees.iter().map(String::as_str).collect();
        virtual_repo(&git, &cache, n, &names);
        Ok((git, cache, n))
    };
    resident(ctx, "plugin/code-intel/resident_cloudprint", &fixture)
}

/// 100 000 files (≤ 10 MB): each sample over its own copy of the shared
/// virtual repository (`code_intel_virtual`).
pub const RESIDENT_100K: Bench = Bench {
    id: "plugin/code-intel/resident_100k",
    run: resident_100k,
};

fn resident_100k(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let template = crate::code_intel_virtual::shared(ctx)?;
    let fixture = |dir: &Path| -> Resident {
        let (git, cache) = crate::code_intel_virtual::open(&template, dir)?;
        Ok((git, cache, template.files()))
    };
    resident(ctx, "plugin/code-intel/resident_100k", &fixture)
}
