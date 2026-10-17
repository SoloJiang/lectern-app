//! `code_intel/*` (docs/design/code_intel.md「性能」). Sources are the
//! language fixtures of `lectern_code_intel::testing`, repeated.

use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use lectern_bench::{Bench, BenchError, Ctx, Measured, Value};
use lectern_code_intel::SystemClock;
use lectern_code_intel::extract::{Deadline, ExtractStatus, LanguageTable, extract};
use lectern_code_intel::testing::{corpus, watchdog};
use lectern_executor::Executor;

/// Single-thread extraction of about 1 MiB of each first-release language,
/// one blob per language, with the watchdog running.
pub const EXTRACT_THROUGHPUT: Bench = Bench {
    id: "code_intel/extract_throughput",
    run: extract_throughput,
};

fn extract_throughput(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let len = ctx.scale(1 << 20, 64 << 10);
    let table = LanguageTable::core();
    let mut blobs = Vec::new();
    for lang in table.iter() {
        if !lang.prepare() {
            return Err(BenchError::new(format!("{} does not compile", lang.name())));
        }
        blobs.push((lang.clone(), corpus(lang.name().as_str(), len)));
    }
    let bytes: u64 = blobs.iter().map(|(_, b)| b.len() as u64).sum();
    let executor = Executor::manual();
    let (watchdog, _thread) = watchdog(&executor, Arc::new(SystemClock));
    let cancel = Arc::new(AtomicBool::new(false));
    let run = || {
        let mut failed = None;
        for (lang, blob) in &blobs {
            // A generous deadline: this measures throughput, not the cut-off.
            let deadline = Deadline::at(
                &watchdog,
                Instant::now() + Duration::from_secs(60),
                usize::MAX,
            );
            let extraction = extract(lang, black_box(blob), &deadline, &cancel);
            if extraction.status != ExtractStatus::Done {
                failed = Some((lang.name().clone(), extraction.status));
            }
        }
        failed
    };
    if let Some((lang, status)) = run() {
        watchdog.stop();
        return Err(BenchError::new(format!("{lang}: {status:?}")));
    }
    let time = ctx.time(run);
    watchdog.stop();
    Ok(vec![Measured::new(
        "code_intel/extract_throughput",
        Value::Throughput { bytes, time },
    )])
}
