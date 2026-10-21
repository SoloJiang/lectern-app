//! `code_intel/*` (docs/design/code_intel.md「性能」, ADR 0031). Sources are
//! the language fixtures of `lectern_code_intel::testing`, repeated; the
//! store's units are synthetic extractions in a temporary cache.

use std::hint::black_box;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, SystemTime};

use lectern_bench::{Bench, BenchError, Ctx, Measured, Value};
use lectern_cache::testing::ManualClock;
use lectern_cache::{Budgets, Cache, Config, Written};
use lectern_code_intel::extract::{
    Deadline, Def, ExtractStatus, Extraction, Lang, LanguageTable, Visibility, extract,
};
use lectern_code_intel::map::{BaseMap, IndexedHints};
use lectern_code_intel::store::{EXISTS_BATCH, Found, PendingTouches, Store, Version, unit_bytes};
use lectern_code_intel::testing::{corpus, synthetic, watchdog};
use lectern_code_intel::{DefKind, NoGrammarPacks, SystemClock};
use lectern_executor::Executor;
use lectern_services::project::BlobId;

/// The file size of the repository-shaped corpus: the 8 KB average of the
/// research's 100k-file estimate (ADR 0031).
const FILE_LEN: usize = 8 << 10;

/// Single-thread extraction of about 1 MiB of each first-release language,
/// as files of about 8 KiB, with the watchdog running.
pub const EXTRACT_THROUGHPUT: Bench = Bench {
    id: "code_intel/extract_throughput",
    run: extract_throughput,
};

/// One file of about 1 MiB per language: the background size limit, against
/// the 250 ms per-file deadline. Record only.
pub const EXTRACT_1MIB: Bench = Bench {
    id: "code_intel/extract_1mib",
    run: extract_1mib,
};

/// A warm start over 100 000 stored files: the existence checks only (200
/// of 500 OIDs), which make the files known to be indexed. They write
/// nothing.
pub const WARM_100K: Bench = Bench {
    id: "code_intel/warm_100k",
    run: warm_100k,
};

/// The access marking deferred off the warm path: the 100 000 hits marked
/// 1024 at a time, with access times two hours old so that every unit is
/// written back, as on a warm start the next day. Record only.
pub const WARM_100K_TOUCH: Bench = Bench {
    id: "code_intel/warm_100k_touch",
    run: warm_100k_touch,
};

/// Every core language, compiled, with one source of about `len` bytes and
/// the number of times to extract it per sample.
fn sources(len: usize, total: usize) -> Result<Vec<(Lang, Vec<u8>, usize)>, BenchError> {
    let table = LanguageTable::core();
    let mut out = Vec::new();
    for lang in table.iter() {
        if !lang.prepare() {
            return Err(BenchError::new(format!(
                "{} does not compile",
                lang.name().0
            )));
        }
        let source = corpus(&lang.name().0, len);
        let count = total.div_ceil(source.len());
        out.push((lang.clone(), source, count));
    }
    Ok(out)
}

fn measure(
    ctx: &mut Ctx,
    id: &'static str,
    sources: &[(Lang, Vec<u8>, usize)],
) -> Result<Vec<Measured>, BenchError> {
    let bytes: u64 = sources.iter().map(|(_, s, n)| (s.len() * n) as u64).sum();
    let executor = Executor::manual();
    let (watchdog, _thread) = watchdog(&executor, Arc::new(SystemClock));
    let cancel = Arc::new(AtomicBool::new(false));
    let run = || {
        let mut failed = None;
        for (lang, source, count) in sources {
            for _ in 0..*count {
                // A generous deadline: this measures throughput, not the
                // cut-off.
                let deadline = Deadline::at(
                    &watchdog,
                    Instant::now() + Duration::from_secs(60),
                    usize::MAX,
                );
                let extraction = extract(lang, black_box(source), &deadline, &cancel);
                if extraction.status != ExtractStatus::Done {
                    failed = Some((lang.name().clone(), extraction.status));
                }
            }
        }
        failed
    };
    if let Some((lang, status)) = run() {
        watchdog.stop();
        return Err(BenchError::new(format!("{}: {status:?}", lang.0)));
    }
    let time = ctx.time(run);
    watchdog.stop();
    Ok(vec![Measured::new(id, Value::Throughput { bytes, time })])
}

fn extract_throughput(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let total = ctx.scale(1 << 20, 64 << 10);
    let sources = sources(FILE_LEN, total)?;
    measure(ctx, "code_intel/extract_throughput", &sources)
}

fn extract_1mib(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let len = ctx.scale(1 << 20, 64 << 10);
    let sources = sources(len, len)?;
    measure(ctx, "code_intel/extract_1mib", &sources)
}

/// The yield switch comes on while a 1 MiB TypeScript file is being parsed
/// (「交互让路」, ≤ 10 ms): from the switch to the extraction returning
/// `Parked`, stopped at the parse's next progress check with its state
/// kept. The switch is a flag the yield probe reads, as yielding work's
/// probe reads `BackgroundGuard::yield_requested`; it turns on once the
/// probe has been asked 200 times (mid-parse). Each sample is a new
/// extraction on its own thread.
pub const YIELD_STOP: Bench = Bench {
    id: "code_intel/yield_stop",
    run: yield_stop,
};

fn yield_stop(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let len = ctx.scale(1 << 20, 64 << 10);
    let source = synthetic::source("ts", 1, len);
    let table = LanguageTable::core();
    let lang = table
        .get("ts")
        .cloned()
        .ok_or_else(|| BenchError::new("no TypeScript"))?;
    if !lang.prepare() {
        return Err(BenchError::new("TypeScript does not compile"));
    }
    let executor = Executor::manual();
    let (watchdog, _thread) = watchdog(&executor, Arc::new(SystemClock));
    let cancel = Arc::new(AtomicBool::new(false));
    let rounds = match ctx.mode() {
        lectern_bench::Mode::Measure => 30,
        lectern_bench::Mode::Smoke => 1,
    };
    let mut samples = Vec::new();
    for _ in 0..rounds {
        let on = Arc::new(AtomicBool::new(false));
        let checks = Arc::new(AtomicUsize::new(0));
        let slot: lectern_code_intel::extract::ResumeSlot = Arc::default();
        let (o, c) = (on.clone(), checks.clone());
        let deadline = Deadline::at(
            &watchdog,
            Instant::now() + Duration::from_secs(60),
            usize::MAX,
        )
        .yielding(
            Arc::new(move || {
                c.fetch_add(1, Ordering::AcqRel);
                o.load(Ordering::Acquire)
            }),
            slot.clone(),
        );
        let (status, stopped, switched) = std::thread::scope(|s| {
            let worker = s.spawn(|| {
                let extraction = extract(&lang, black_box(&source), &deadline, &cancel);
                (extraction.status, Instant::now())
            });
            while checks.load(Ordering::Acquire) < 200 && !worker.is_finished() {
                std::hint::spin_loop();
            }
            let switched = Instant::now();
            on.store(true, Ordering::Release);
            let (status, stopped) = worker.join().expect("the extraction");
            (status, stopped, switched)
        });
        let kept = slot.lock().is_ok_and(|s| s.is_some());
        if status != ExtractStatus::Parked || !kept {
            watchdog.stop();
            return Err(BenchError::new(format!(
                "not stopped mid-parse: {status:?}"
            )));
        }
        samples.push(stopped.saturating_duration_since(switched));
    }
    watchdog.stop();
    Ok(vec![Measured::new(
        "code_intel/yield_stop",
        Value::Time(lectern_bench::Samples(samples)),
    )])
}

/// The OID of file `i`.
fn oid(i: u64) -> BlobId {
    let mut b = [0u8; 20];
    b[..8].copy_from_slice(&i.to_be_bytes());
    b[19] = 0x5a;
    BlobId::Sha1(b)
}

/// A small extraction (3 definitions, up to 6 reference names): the
/// existence check reads only `ci_ext`.
fn unit(i: u64) -> Extraction {
    let defs = (0..3u32)
        .map(|j| Def {
            name: format!("symbol_{i}_{j}").into(),
            kind: DefKind::Function,
            line: j * 10,
            column: 4,
            byte: u64::from(j) * 400 + 4,
            end_line: j * 10 + 5,
            scope: None,
            visibility: Visibility::Public,
            arity: Some(2),
        })
        .collect();
    let mut refs: Vec<Box<str>> = (0..6)
        .map(|j| format!("symbol_{}_{}", (i * 7 + j) % (i + 1), j % 3).into())
        .collect();
    refs.sort();
    refs.dedup();
    Extraction {
        status: ExtractStatus::Done,
        defs,
        refs,
        specs: Vec::new(),
    }
}

/// A warm start's store: 100 000 files (2000 in smoke mode) stored as units
/// in batches of 64, the scheduler's batch.
struct Warm {
    /// Keeps the store's partition open.
    _cache: Cache,
    clock: ManualClock,
    store: Store,
    version: Version,
    oids: Vec<BlobId>,
}

impl Warm {
    /// The store and every hit of its existence checks.
    fn new(ctx: &Ctx) -> Result<(Warm, Vec<Found>), BenchError> {
        let files = ctx.scale(100_000, 2_000);
        let clock = ManualClock::new(SystemTime::now());
        let cache = Cache::with_clock(
            Config {
                root: ctx.scratch().join("cache"),
                budgets: Budgets::default(),
                ui_thread: None,
            },
            clock.clone(),
        );
        let store = Store::new(&cache.repo(Path::new("/work/monorepo/.git")));
        let table = LanguageTable::core();
        let version = Version::of(table.get("rust").expect("a core language"));
        for first in (0..files).step_by(64) {
            let mut w = store.write();
            for i in first..(first + 64).min(files) {
                w.add(oid(i), &version, &unit(i));
            }
            match w.commit() {
                Written::Committed => {}
                other => return Err(BenchError::debug("fill", other)),
            }
        }
        let warm = Warm {
            _cache: cache,
            clock,
            store,
            version,
            oids: (0..files).map(oid).collect(),
        };
        let hits = warm.check();
        if hits.len() != warm.oids.len() {
            return Err(BenchError::new("a stored unit was not found"));
        }
        Ok((warm, hits))
    }

    /// The warm pass: existence checks of 500 OIDs.
    fn check(&self) -> Vec<Found> {
        let mut found = Vec::with_capacity(self.oids.len());
        for chunk in self.oids.chunks(EXISTS_BATCH) {
            found.extend(
                self.store
                    .exists(&self.version, black_box(chunk))
                    .unwrap_or_default(),
            );
        }
        found
    }
}

/// Rebuilding the blob → path map of 100 000 files after the file list was
/// replaced: the base part (language per entry, by_oid sorted) and the
/// hints carried over by path. Synthetic OIDs; the file list itself belongs
/// to `project`.
pub const MAP_100K: Bench = Bench {
    id: "code_intel/map_100k",
    run: map_100k,
};

fn map_100k(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let n = ctx.scale(100_000, 5_000);
    let old = Arc::new(synthetic::file_list(n));
    let new = Arc::new(synthetic::file_list(n));
    let table = LanguageTable::core();
    let hints = IndexedHints::new(old.len());
    let time = ctx.time(|| {
        let base = BaseMap::build(new.clone(), &table, &NoGrammarPacks);
        let carried = IndexedHints::carried(&hints, &old, &new);
        (base, carried)
    });
    Ok(vec![Measured::new(
        "code_intel/map_100k",
        Value::Time(time),
    )])
}

fn warm_100k(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (warm, _) = Warm::new(ctx)?;
    let time = ctx.time(|| warm.check());
    Ok(vec![Measured::new(
        "code_intel/warm_100k",
        Value::Time(time),
    )])
}

fn warm_100k_touch(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (warm, hits) = Warm::new(ctx)?;
    let generation = warm.store.evictions();
    let time = ctx.time_with_setup(
        || {
            // Every access time is over an hour old: all are written back.
            warm.clock.advance(Duration::from_secs(2 * 3600));
            let mut pending = PendingTouches::new();
            pending.push(generation, &hits);
            pending
        },
        |mut pending| while warm.store.touch_pending(&mut pending) > 0 {},
    );
    if warm.store.evictions() != generation {
        return Err(BenchError::new("units were evicted while marking"));
    }
    Ok(vec![Measured::new(
        "code_intel/warm_100k_touch",
        Value::Time(time),
    )])
}

/// The size of the cache partition after the units of a synthetic
/// 100 000-file repository (8 KiB source files of the language mix, about
/// one in ten not source) are written, in batches of 64 as the batch tiers
/// write them. The partition file and its `-wal`. Record only; also prints
/// the bytes the units declare (「存储表」's estimate) and the evictions.
pub const DISK_100K: Bench = Bench {
    id: "code_intel/disk_100k",
    run: disk_100k,
};

fn disk_100k(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let n = ctx.scale(100_000, 300);
    let dir = tempfile::tempdir_in(ctx.scratch())?;
    let cache = Cache::new(Config {
        root: dir.path().to_path_buf(),
        budgets: Budgets::default(),
        ui_thread: None,
    });
    let store = Store::new(&cache.repo(Path::new("/work/synthetic/.git")));
    let table = LanguageTable::core();
    for lang in table.iter() {
        if !lang.prepare() {
            return Err(BenchError::new(format!(
                "{} does not compile",
                lang.name().0
            )));
        }
    }
    let executor = Executor::manual();
    let (wd, _thread) = watchdog(&executor, Arc::new(SystemClock));
    let threads = std::thread::available_parallelism()
        .map_or(1, |p| p.get())
        .min(4);
    let start = Instant::now();
    let results: Vec<Result<(u64, usize), String>> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|w| {
                let (store, table, wd) = (store.clone(), table.clone(), wd.clone());
                s.spawn(move || -> Result<(u64, usize), String> {
                    let cancel = Arc::new(AtomicBool::new(false));
                    let (mut declared, mut units) = (0u64, 0usize);
                    let mut write = store.write();
                    for i in (w..n).step_by(threads) {
                        let path = synthetic::path(i);
                        let Some(lang) = table.for_path(&path, &NoGrammarPacks) else {
                            continue;
                        };
                        let source = synthetic::source(synthetic::extension(i), i, 8 << 10);
                        let deadline =
                            Deadline::at(&wd, Instant::now() + Duration::from_secs(60), 1 << 20);
                        let extraction = extract(lang, &source, &deadline, &cancel);
                        if extraction.status != ExtractStatus::Done {
                            return Err(format!("{path}: {:?}", extraction.status));
                        }
                        let oid = lectern_services::project::BlobId::Sha1(sha1ish(i));
                        declared += unit_bytes(&extraction);
                        write.add(oid, &Version::of(lang), &extraction);
                        units += 1;
                        if write.len() == 64 {
                            let full = std::mem::replace(&mut write, store.write());
                            if !matches!(full.commit(), Written::Committed) {
                                return Err("a write was dropped".into());
                            }
                        }
                    }
                    if !write.is_empty() && !matches!(write.commit(), Written::Committed) {
                        return Err("a write was dropped".into());
                    }
                    Ok((declared, units))
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|w| w.join().unwrap_or_else(|_| Err("a worker panicked".into())))
            .collect()
    });
    wd.stop();
    let (mut declared, mut units) = (0u64, 0usize);
    for r in results {
        let (d, u) = r.map_err(BenchError::new)?;
        declared += d;
        units += u;
    }
    let bytes = dir_bytes(dir.path())?;
    eprintln!(
        "code_intel/disk_100k: {units} units, {declared} bytes declared, {bytes} on disk, \
         {} evictions, {:.0?}",
        store.evictions(),
        start.elapsed()
    );
    Ok(vec![Measured::new(
        "code_intel/disk_100k",
        Value::Bytes(vec![bytes]),
    )])
}

/// An opened 16 MiB TypeScript file (「大文件」, ADR 0034): from the focus
/// to its window chain written to the end (the `Windowed` marker stored),
/// on a fresh cache each sample: the stream of its OID in 1 MiB ranges,
/// then about 260 windows of 64 KiB, each written as it is done. Record
/// only.
pub const WINDOW_CHAIN_16MIB: Bench = Bench {
    id: "code_intel/window_chain_16mib",
    run: window_chain_16mib,
};

fn window_chain_16mib(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    use lectern_code_intel::testing::{FakeGit, Rig};

    // At most 16 MiB, the largest file opened by windows: the source ends
    // past the length asked for by up to one unit.
    let len = ctx.scale((16 << 20) - (64 << 10), 1_200_000);
    let source = synthetic::source("ts", 16, len);
    let mut samples = Vec::new();
    for _ in 0..ctx.scale(3, 1) {
        let git = Arc::new(FakeGit::new());
        git.add("main", &[("big.ts", &source)]);
        git.commit("main");
        let dir = tempfile::tempdir_in(ctx.scratch())?;
        let cache = Cache::new(Config {
            root: dir.path().to_path_buf(),
            budgets: Budgets::default(),
            ui_thread: None,
        });
        let rig = Rig::new(git, cache, Arc::new(SystemClock));
        rig.open();
        rig.settle();
        let start = Instant::now();
        if !rig.current("big.ts") {
            return Err(BenchError::new("big.ts is not in the map"));
        }
        rig.settle();
        let took = start.elapsed();
        let status = rig.stored_status("big.ts");
        if status != Some(ExtractStatus::Windowed) {
            return Err(BenchError::new(format!("the chain ended {status:?}")));
        }
        samples.push(took);
    }
    Ok(vec![Measured::new(
        "code_intel/window_chain_16mib",
        Value::Time(lectern_bench::Samples(samples)),
    )])
}

/// The dependency tier (「依赖层」, ADR 0033) of a synthetic TS project
/// (`testing::npm::ts_project`): 21 direct dependencies of 12 declaration
/// files of about 20 KiB each (about 5 MiB of closure), a 4 MiB
/// `package-lock.json`, `node_modules` ignored. From opening the project
/// on an empty cache to `coverage.deps` being `Done`. Record only.
pub const DEPS_TS_PROJECT: Bench = Bench {
    id: "code_intel/deps_ts_project",
    run: deps_ts_project,
};

/// The same project with every path table and unit in the cache, opened
/// again: the identity tasks of the open from their start to their end,
/// summed (the manifest read, each direct dependency's link and manifest
/// `lstat`ed, the lockfile streamed, one existence check per table's
/// OIDs, the map built); nothing read in the closure, nothing extracted.
/// The rest of the open (the worktree comparison, tiers 3 and 4) is other
/// budgets'.
pub const DEPS_IDENTITY: Bench = Bench {
    id: "code_intel/deps_identity",
    run: deps_identity,
};

struct DepsProject {
    git: Arc<lectern_code_intel::testing::FakeGit>,
    closure: usize,
}

fn deps_project(ctx: &Ctx) -> DepsProject {
    use lectern_code_intel::testing::FakeGit;
    use lectern_code_intel::testing::npm::{NpmTree, ts_project};

    let direct = ctx.scale(21, 3);
    let files = ctx.scale(12, 3);
    let file_len = ctx.scale(20 << 10, 4 << 10);
    let lockfile = ctx.scale(4 << 20, 64 << 10);
    let git = Arc::new(FakeGit::new());
    git.ignore("node_modules/");
    git.add("main", &[("src/main.ts", b"export const main = 1;\n")]);
    git.commit("main");
    let tree = NpmTree::new(&git.root("main"));
    ts_project(&tree, direct, files, file_len, lockfile);
    DepsProject {
        git,
        closure: direct * files,
    }
}

fn deps_rig(
    git: &Arc<lectern_code_intel::testing::FakeGit>,
    dir: &Path,
) -> lectern_code_intel::testing::Rig {
    let cache = Cache::new(Config {
        root: dir.to_path_buf(),
        budgets: Budgets::default(),
        ui_thread: None,
    });
    lectern_code_intel::testing::Rig::new(git.clone(), cache, Arc::new(SystemClock))
}

fn deps_done(rig: &lectern_code_intel::testing::Rig) -> Result<(), BenchError> {
    use lectern_services::code_intel::TierState;
    let coverage = rig
        .indexer
        .coverage(&rig.git.key())
        .ok_or_else(|| BenchError::new("the project is not open"))?;
    if coverage.deps != TierState::Done {
        return Err(BenchError::new(format!(
            "the tier ended {:?}",
            coverage.deps
        )));
    }
    Ok(())
}

fn deps_ts_project(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let mut samples = Vec::new();
    for _ in 0..ctx.scale(3, 1) {
        let dir = tempfile::tempdir_in(ctx.scratch())?;
        let project = deps_project(ctx);
        let rig = deps_rig(&project.git, dir.path());
        let start = Instant::now();
        rig.open();
        rig.settle();
        let took = start.elapsed();
        deps_done(&rig)?;
        // The closure and the worktree's one file.
        let extracted = rig.indexer.stats().extractions;
        if extracted != project.closure + 1 {
            return Err(BenchError::new(format!(
                "{extracted} extractions for a closure of {}",
                project.closure
            )));
        }
        samples.push(took);
    }
    Ok(vec![Measured::new(
        "code_intel/deps_ts_project",
        Value::Time(lectern_bench::Samples(samples)),
    )])
}

fn deps_identity(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let dir = tempfile::tempdir_in(ctx.scratch())?;
    let project = deps_project(ctx);
    {
        let rig = deps_rig(&project.git, dir.path());
        rig.open();
        rig.settle();
        deps_done(&rig)?;
    }
    let mut samples = Vec::new();
    for _ in 0..ctx.scale(10, 1) {
        let rig = deps_rig(&project.git, dir.path());
        rig.open();
        rig.settle();
        deps_done(&rig)?;
        let extracted = rig.indexer.stats().extractions;
        if extracted != 0 {
            return Err(BenchError::new(format!("{extracted} extractions")));
        }
        // The identity tasks of this open (the project's open itself,
        // its worktree comparison and tiers 3 and 4 are other budgets').
        let runs = rig.indexer.deps_identity_times(&rig.git.key());
        if runs.is_empty() {
            return Err(BenchError::new("no identity task ran"));
        }
        samples.push(runs.iter().sum());
    }
    Ok(vec![Measured::new(
        "code_intel/deps_identity",
        Value::Time(lectern_bench::Samples(samples)),
    )])
}

/// 「规范内容 OID 的批量核对」: a checkout leaves 1000 tracked files (about
/// 8 KiB each) of a 10 000-file repository with CRLF (`core.autocrlf =
/// true`, the index has LF); fs_watch reports them modified and the
/// reconcile reads them again until the overlay is installed (unchanged:
/// every file converts to its index content). One `canonical_oids` per
/// 256 files and about 2 MiB, four index parses where one `canonical_oid`
/// per file made 1000. A real repository through the production git
/// adapter (`testing::RepoRig`), the batch tiers paused. Record only.
pub const REREAD_CRLF_1000: Bench = Bench {
    id: "code_intel/reread_crlf_1000",
    run: reread_crlf_1000,
};

fn reread_crlf_1000(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    use lectern_code_intel::testing::RepoRig;
    use lectern_git::testing::{TestHead, TestRepo};
    use lectern_services::project::{FileKind, FileList};

    let (total, crlf) = ctx.scale((10_000, 1000), (300, 30));
    let repo = TestRepo::new();
    repo.append_config("[core]\n\tautocrlf = true\n");
    let mut files: Vec<(String, Vec<u8>)> = (0..total)
        .map(|i| {
            let path = format!("pkg{:02}/src/f{i:05}.ts", i % 50);
            let content = if i < crlf {
                synthetic::source("ts", i, FILE_LEN)
            } else {
                format!("export const v{i} = {i};\n").into_bytes()
            };
            (path, content)
        })
        .collect();
    files.sort();
    if files.iter().any(|(_, c)| c.contains(&b'\r')) {
        return Err(BenchError::new("the committed content has CR"));
    }
    for (path, content) in &files {
        repo.write_file(path, content);
        repo.backdate(path);
    }
    let entries: Vec<_> = files.iter().map(|(path, _)| repo.add(path)).collect();
    repo.write_index(repo.common_dir(), &entries);
    let tree: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_slice()))
        .collect();
    let commit = repo.commit(repo.tree(&tree), &[], "init");
    repo.set_ref("refs/heads/main", commit);
    repo.set_head(TestHead::Branch("refs/heads/main"));
    let mut list = FileList::builder();
    for entry in &entries {
        let path = std::str::from_utf8(&entry.path).map_err(|e| BenchError::new(e.to_string()))?;
        let oid = BlobId::from_bytes(entry.oid.as_bytes())
            .ok_or_else(|| BenchError::new("an OID of 20 or 32 bytes"))?;
        list.push_with_oid(path, FileKind::File, oid);
    }

    let dir = tempfile::tempdir_in(ctx.scratch())?;
    let cache = Cache::new(Config {
        root: dir.path().to_path_buf(),
        budgets: Budgets::default(),
        ui_thread: None,
    });
    let common = repo.common_dir().canonicalize()?;
    let root = repo.root().canonicalize()?;
    let rig = RepoRig::new(
        lectern_git::testing::discover,
        &common,
        &root,
        Arc::new(list.build()),
        cache,
        Arc::new(SystemClock),
    );
    rig.pause();
    rig.open();
    rig.settle();
    let overlay = |rig: &RepoRig| {
        rig.indexer
            .map(rig.key())
            .map_or(usize::MAX, |m| m.overlay.entries().len())
    };
    if overlay(&rig) != 0 {
        return Err(BenchError::new("the worktree is not clean after opening"));
    }
    // The checkout: CRLF in the worktree, the same content in git's terms.
    let checked_out: Vec<&str> = files
        .iter()
        .filter(|(_, c)| c.len() > 1024)
        .map(|(p, c)| {
            let crlf = String::from_utf8_lossy(c).replace('\n', "\r\n");
            repo.write_file(p, crlf.as_bytes());
            p.as_str()
        })
        .collect();
    if checked_out.len() != crlf {
        return Err(BenchError::new("not every large file was checked out"));
    }
    let before = rig.indexer.stats().rereads;
    let time = ctx.time(|| {
        rig.modified(&checked_out);
        rig.settle();
    });
    let samples = rig.indexer.stats().rereads - before;
    if samples == 0 || !samples.is_multiple_of(crlf) || overlay(&rig) != 0 {
        return Err(BenchError::new(format!(
            "{samples} files read again, overlay of {}: not the checkout's",
            overlay(&rig)
        )));
    }
    Ok(vec![Measured::new(
        "code_intel/reread_crlf_1000",
        Value::Time(time),
    )])
}

/// A distinct SHA-1-sized OID per file.
fn sha1ish(i: usize) -> [u8; 20] {
    let mut b = [0u8; 20];
    b[..8].copy_from_slice(&(i as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15).to_be_bytes());
    b[8..16].copy_from_slice(&(i as u64).to_be_bytes());
    b[19] = 0x3c;
    b
}

/// Bytes of every file under `dir`.
fn dir_bytes(dir: &Path) -> Result<u64, BenchError> {
    let mut total = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        total += if meta.is_dir() {
            dir_bytes(&entry.path())?
        } else {
            meta.len()
        };
    }
    Ok(total)
}
