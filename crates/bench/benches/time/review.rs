//! `review/*` (docs/design/review.md「性能」). Step 1: the annotation log's
//! whole-file read, with the fixture of `lectern_review::testing`. The
//! system file cache is warm. Step 2: one file's change chain.

use std::path::Path;

use lectern_bench::{Bench, BenchError, Ctx, Measured, Value};
use lectern_review::testing::{ChainRun, LogLoad, chain_fixture, load_log, write_log};

/// Reading a 16 000-record log (about 5.3 MiB) whole and building its
/// index, as opening a project does in the background.
pub const LOG_LOAD_16K: Bench = Bench {
    id: "review/log_load_16k",
    run: log_load_16k,
};

/// One file's change chain: 3000 lines, 40 `Replace` steps in 8 turns,
/// each step's `line_diff` and version OID included, the edits' `load`
/// not (they are in memory). Record only.
pub const CHAIN_3000_LINES: Bench = Bench {
    id: "review/chain_3000_lines",
    run: chain_3000_lines,
};

fn log_load_16k(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let dir = ctx.scratch().join("review");
    let repo = Path::new("/work/lectern/.git");
    let records = ctx.scale(16_000, 1_000);
    let bytes = write_log(&dir, repo, records)?;
    let mut wrong: Option<LogLoad> = None;
    let time = ctx.time(|| {
        let load = load_log(&dir, repo);
        if load.records != records || load.bad_lines != 0 || !load.complete {
            wrong.get_or_insert(load);
        }
        load
    });
    if let Some(load) = wrong {
        return Err(BenchError::debug("load_log", load));
    }
    Ok(vec![
        Measured::new("review/log_load_16k", Value::Time(time))
            .with_note(Some(format!("{records} records, {bytes} bytes"))),
    ])
}

fn chain_3000_lines(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (lines, replaces) = (3000, 40);
    let fixture = chain_fixture(lines, replaces);
    let expected = ChainRun {
        applied: replaces,
        aligned: true,
        groups: replaces / 5,
    };
    let mut wrong: Option<ChainRun> = None;
    let time = ctx.time(|| {
        let run = fixture.run();
        if run != expected {
            wrong.get_or_insert(run);
        }
        run
    });
    if let Some(run) = wrong {
        return Err(BenchError::debug("chain", run));
    }
    Ok(vec![
        Measured::new("review/chain_3000_lines", Value::Time(time))
            .with_note(Some(format!("{lines} lines, {replaces} Replace steps"))),
    ])
}
