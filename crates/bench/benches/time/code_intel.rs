//! `code_intel/*` (docs/design/code_intel.md「性能」, ADR 0031). Sources are
//! the language fixtures of `lectern_code_intel::testing`, repeated.

use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use lectern_bench::{Bench, BenchError, Ctx, Measured, Value};
use lectern_code_intel::SystemClock;
use lectern_code_intel::extract::{Deadline, ExtractStatus, Lang, LanguageTable, extract};
use lectern_code_intel::testing::{corpus, watchdog};
use lectern_executor::Executor;

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
