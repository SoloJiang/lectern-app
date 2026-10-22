//! `log/idle_memory` (docs/design/logging.md「性能」): the live bytes the
//! logger holds after writing 10 000 records and resting 5 s, default
//! configuration; `log/idle_memory/verbose` the same with verbose logging
//! on (record only). Counted by `CountingAlloc`: the buffers, the writer's
//! `BufWriter`s, the rate-limit table and the logger's state; not the
//! writer thread's stack. One sample each: a process installs one logger.

use std::path::Path;
use std::time::Duration;

use lectern_bench::{Bench, BenchError, CountingAlloc, Ctx, Measured, Value};
use lectern_logging::{
    Config, EarlyConfig, Filter, Header, Logging, Value as LogValue, Verbose, debug, info,
    install_early, testing,
};

pub const IDLE_MEMORY: Bench = Bench {
    id: "log/idle_memory",
    run: idle_memory,
};

const RECORDS: u64 = 10_000;

fn flush(logging: &Logging) -> Result<(), BenchError> {
    if testing::flush(logging, Duration::from_secs(5)) {
        Ok(())
    } else {
        Err(BenchError::new("the log writer did not flush within 5 s"))
    }
}

fn idle_memory(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let dir = tempfile::tempdir()?;
    let records = ctx.scale(RECORDS, 500);
    let rest = ctx.scale(Duration::from_secs(5), Duration::from_millis(100));
    let path = Path::new("/Users/bench/src/lectern-app/crates/cache/src/store.rs");
    let config = EarlyConfig {
        dir: Some(dir.path().to_owned()),
        cli_filter: None,
    };
    let header = Header {
        version: "0.0.0",
        build: "bench",
        os: "bench".to_owned(),
        arch: std::env::consts::ARCH,
    };
    let before = CountingAlloc::live();
    let early = install_early(config).map_err(|e| BenchError::debug("install_early", e))?;
    let logging = early.start(Config {
        header,
        on_writer_start: None,
    });
    testing::set_callsite_limit(u32::MAX);
    for i in 0..records {
        info!("idle memory record"; i = i, bytes = 4096_u64, path = LogValue::path(path), took = Duration::from_micros(12));
        if i % 256 == 255 {
            flush(&logging)?;
        }
    }
    flush(&logging)?;
    std::thread::sleep(rest);
    let default = CountingAlloc::live().saturating_sub(before);

    logging.set_verbose(Some(Verbose {
        filter: Filter::parse("debug").0,
        lasts: Duration::from_secs(3600),
    }));
    for i in 0..records {
        debug!("idle memory verbose record"; i = i, bytes = 4096_u64, path = LogValue::path(path), took = Duration::from_micros(12));
        if i % 256 == 255 {
            flush(&logging)?;
        }
    }
    flush(&logging)?;
    std::thread::sleep(rest);
    let verbose = CountingAlloc::live().saturating_sub(before);
    testing::set_callsite_limit(20);
    let status = logging.status();
    if status.dropped != 0 {
        return Err(BenchError::new(format!(
            "{} records dropped",
            status.dropped
        )));
    }
    Ok(vec![
        Measured::new("log/idle_memory", Value::Bytes(vec![default as u64])),
        Measured::new(
            "log/idle_memory/verbose",
            Value::Bytes(vec![verbose as u64]),
        ),
    ])
}
