//! `review/*` (docs/design/review.md「性能」). Step 1: the annotation log's
//! whole-file read, with the fixture of `lectern_review::testing`. The
//! system file cache is warm.

use std::path::Path;

use lectern_bench::{Bench, BenchError, Ctx, Measured, Value};
use lectern_review::testing::{LogLoad, load_log, write_log};

/// Reading a 16 000-record log (about 5.3 MiB) whole and building its
/// index, as opening a project does in the background.
pub const LOG_LOAD_16K: Bench = Bench {
    id: "review/log_load_16k",
    run: log_load_16k,
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
