//! `syntax/*` in bytes (docs/design/file_reader.md「性能」): the temporary
//! memory of one window of the chain. tree-sitter's allocations go through
//! the counting allocator (`memory.rs`'s `main`).

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use lectern_bench::{Bench, BenchError, CountingAlloc, Ctx, Measured, Value};
use lectern_syntax::highlight::{Budget, ChainWindow, Outcome, Source};
use lectern_syntax::{Languages, testing};

/// `syntax/window_peak`: the peak of live bytes while one 64 KiB window of
/// the chain is parsed and highlighted (its tree, the query's captures and
/// the tokens), on TypeScript, Rust, JSON and minified JavaScript, a few
/// windows each; the largest peak ≤ 8 MiB. The source is allocated before
/// the count starts.
pub const WINDOW_PEAK: Bench = Bench {
    id: "syntax/window_peak",
    run: window_peak,
};

fn window_peak(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let langs = Languages::core();
    let len = ctx.scale(2 << 20, 256 << 10);
    let starts = ctx.scale(vec![0, 300_000, 900_000, 1_500_000], vec![0, 100_000]);
    let cancel = AtomicBool::new(false);
    let mut samples = Vec::new();
    for (name, make) in [
        ("ts", testing::ts as fn(usize, usize) -> Vec<u8>),
        ("rust", testing::rust),
        ("json", testing::json),
        ("js", testing::minified_js),
    ] {
        let lang = langs
            .get(name)
            .ok_or_else(|| BenchError::new(format!("no language {name}")))?;
        if !lang.prepare() {
            return Err(BenchError::new(format!(
                "the {name} query does not compile"
            )));
        }
        let src: Arc<dyn Source> = Arc::new(make(len, 0));
        for &start in &starts {
            let before = CountingAlloc::live();
            CountingAlloc::reset_peak();
            let mut window = ChainWindow::new(lang, src.clone(), start);
            let budget = Budget {
                budget: Duration::from_secs(60),
                cancel: &cancel,
                yield_probe: None,
            };
            let spans = match window.step(budget) {
                Outcome::Done((spans, _)) => spans,
                other => {
                    return Err(BenchError::new(format!(
                        "{name} window at {start}: {other:?}"
                    )));
                }
            };
            let peak = CountingAlloc::peak().saturating_sub(before);
            drop((spans, window));
            samples.push(peak as u64);
        }
    }
    Ok(vec![Measured::new(
        "syntax/window_peak",
        Value::Bytes(samples),
    )])
}
