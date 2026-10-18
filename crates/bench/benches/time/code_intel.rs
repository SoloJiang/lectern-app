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
use lectern_code_intel::store::{EXISTS_BATCH, Store, Version};
use lectern_code_intel::testing::{corpus, watchdog};
use lectern_code_intel::{DefKind, SystemClock};
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

/// The existence check of a warm start over 100 000 files, all stored: 200
/// checks of 500 OIDs, each marking its units as used. The access times are
/// two hours old at every sample, so the touches are written back (24 times
/// in the calls, 4096 units each), as on a warm start the next day.
pub const WARM_100K: Bench = Bench {
    id: "code_intel/warm_100k",
    run: warm_100k,
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
    }
}

fn warm_100k(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
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
    // Batches of 64 units, the scheduler's batch.
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
    let oids: Vec<BlobId> = (0..files).map(oid).collect();
    let check = || {
        let mut found = 0;
        for chunk in oids.chunks(EXISTS_BATCH) {
            found += store
                .exists(&version, black_box(chunk))
                .map_or(0, |f| f.len());
        }
        found
    };
    if check() != oids.len() {
        return Err(BenchError::new("a stored unit was not found"));
    }
    let time = ctx.time_with_setup(
        || clock.advance(Duration::from_secs(2 * 3600)),
        |()| check(),
    );
    Ok(vec![Measured::new(
        "code_intel/warm_100k",
        Value::Time(time),
    )])
}
