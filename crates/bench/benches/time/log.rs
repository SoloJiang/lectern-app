//! `log/*` (docs/design/logging.md「性能」): the calling thread's cost of a
//! log call, and `install_early`. One logger is installed per process (a
//! shared fixture) with the filter `info,cache=debug`: the global level is
//! debug, this target (`time`) stays at info, so `trace!` is below the
//! global level and `debug!` is rejected by its target.

use std::hint::black_box;
use std::path::Path;
use std::rc::Rc;
use std::sync::Barrier;
use std::time::{Duration, Instant};

use lectern_bench::{Bench, BenchError, Ctx, Measured, Samples, Value};
use lectern_logging::{
    Config, EarlyConfig, Header, Logging, Value as LogValue, debug, info, install_early, testing,
    trace,
};

/// The process's logger and its directory.
struct Installed {
    _dir: tempfile::TempDir,
    logging: Logging,
}

fn installed(ctx: &Ctx) -> Result<Rc<Installed>, BenchError> {
    ctx.shared("log::installed(info,cache=debug)", || {
        let dir = tempfile::tempdir()?;
        let early = install_early(EarlyConfig {
            dir: Some(dir.path().to_owned()),
            cli_filter: Some("info,cache=debug".to_owned()),
        })
        .map_err(|e| BenchError::debug("install_early", e))?;
        let logging = early.start(Config {
            header: Header {
                version: "0.0.0",
                build: "bench",
                os: "bench".to_owned(),
                arch: std::env::consts::ARCH,
            },
            on_writer_start: None,
        });
        if !testing::flush(&logging, Duration::from_secs(5)) {
            return Err(BenchError::new("the log writer did not start"));
        }
        Ok(Installed { _dir: dir, logging })
    })
}

fn flush(logging: &Logging) -> Result<(), BenchError> {
    if testing::flush(logging, Duration::from_secs(5)) {
        Ok(())
    } else {
        Err(BenchError::new("the log writer did not flush within 5 s"))
    }
}

/// Raises the per-call-site limit while timing the written path, so that
/// one call site writes every time; restored on drop.
struct Unlimited;

impl Unlimited {
    fn new() -> Unlimited {
        testing::set_callsite_limit(u32::MAX);
        Unlimited
    }
}

impl Drop for Unlimited {
    fn drop(&mut self) {
        testing::set_callsite_limit(20);
    }
}

/// A call below the global level: one atomic load.
pub const DISABLED_CALL: Bench = Bench {
    id: "log/disabled_call",
    run: disabled_call,
};

fn disabled_call(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let _logger = installed(ctx)?;
    let time = ctx.time(|| trace!("below the global level"; count = black_box(1_u64)));
    Ok(vec![Measured::new("log/disabled_call", Value::Time(time))])
}

/// A call the global level lets through and its target's level rejects:
/// the call site's cached decision.
pub const FILTERED_CALL: Bench = Bench {
    id: "log/filtered_call",
    run: filtered_call,
};

fn filtered_call(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let _logger = installed(ctx)?;
    let time = ctx.time(|| debug!("rejected by its target"; count = black_box(1_u64)));
    Ok(vec![Measured::new("log/filtered_call", Value::Time(time))])
}

/// Calls per sample of `log/enabled_call`: well below the running buffer.
const GROUP: u32 = 256;

fn written(path: &Path, i: u64) {
    info!("index batch"; files = i, bytes = 4096_u64, path = LogValue::path(path), took = Duration::from_micros(12));
}

/// A written info record with 4 fields, on the calling thread: encoding and
/// the copy into the buffer. Each sample is 256 calls after the writer has
/// emptied the buffer.
pub const ENABLED_CALL: Bench = Bench {
    id: "log/enabled_call",
    run: enabled_call,
};

fn enabled_call(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let logger = installed(ctx)?;
    let _unlimited = Unlimited::new();
    let path = Path::new("/Users/bench/src/lectern-app/crates/cache/src/store.rs");
    let dropped = logger.logging.status().dropped;
    let mut failed = None;
    let groups = ctx.time_with_setup(
        || {
            if let Err(e) = flush(&logger.logging) {
                failed = Some(e);
            }
        },
        |()| {
            for i in 0..u64::from(GROUP) {
                written(black_box(path), i);
            }
        },
    );
    if let Some(e) = failed {
        return Err(e);
    }
    flush(&logger.logging)?;
    if logger.logging.status().dropped != dropped {
        return Err(BenchError::new("records were dropped"));
    }
    let per_call = Samples(groups.0.iter().map(|d| *d / GROUP).collect());
    Ok(vec![Measured::new(
        "log/enabled_call",
        Value::Time(per_call),
    )])
}

/// Threads and calls per thread in one round of `log/contended_call`: 400
/// records, about 80 KiB, below the running buffer.
const THREADS: usize = 4;
const CALLS: usize = 100;

/// Four threads writing at once; each call timed on its own. 25 rounds,
/// 10 000 calls: the budget's「at most 100 above 5 µs」is the p99.
pub const CONTENDED_CALL: Bench = Bench {
    id: "log/contended_call",
    run: contended_call,
};

fn contended_call(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let logger = installed(ctx)?;
    let _unlimited = Unlimited::new();
    let path = Path::new("/Users/bench/src/lectern-app/crates/cache/src/store.rs");
    let rounds = ctx.scale(25, 1);
    let dropped = logger.logging.status().dropped;
    let mut samples = Vec::with_capacity(rounds * THREADS * CALLS);
    for _ in 0..rounds {
        flush(&logger.logging)?;
        let barrier = Barrier::new(THREADS);
        let round: Vec<Vec<Duration>> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..THREADS)
                .map(|_| {
                    s.spawn(|| {
                        let mut times = Vec::with_capacity(CALLS);
                        barrier.wait();
                        for i in 0..CALLS as u64 {
                            let start = Instant::now();
                            written(black_box(path), i);
                            times.push(start.elapsed());
                        }
                        times
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_default())
                .collect()
        });
        samples.extend(round.into_iter().flatten());
    }
    flush(&logger.logging)?;
    if logger.logging.status().dropped != dropped {
        return Err(BenchError::new("records were dropped"));
    }
    Ok(vec![Measured::new(
        "log/contended_call",
        Value::Time(Samples(samples)),
    )])
}

/// A third-party record (as GPUI writes them) whose call site is over its
/// limit: a hash and a few atomics, no encoding.
pub const THIRD_PARTY_LIMITED: Bench = Bench {
    id: "log/third_party_limited",
    run: third_party_limited,
};

fn third_party(n: u64) {
    testing::third_party(
        testing::Level::Warn,
        "gpui::window",
        Some("src/window.rs"),
        Some(42),
        format_args!("window {n} not found"),
    );
}

fn third_party_limited(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let _logger = installed(ctx)?;
    for n in 0..25 {
        third_party(n);
    }
    let time = ctx.time(|| third_party(black_box(7)));
    Ok(vec![Measured::new(
        "log/third_party_limited",
        Value::Time(time),
    )])
}

/// `install_early`'s work: the environment, the filter, the 64 KiB early
/// buffer, the rate-limit table, the global level, `set_logger` and the
/// panic hook (testing::install_early_again; the installed logger stays).
pub const INSTALL_EARLY: Bench = Bench {
    id: "log/install_early",
    run: install_early_cost,
};

fn install_early_cost(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let _logger = installed(ctx)?;
    let dir = ctx.scratch().to_owned();
    let time = ctx.time(|| {
        testing::install_early_again(EarlyConfig {
            dir: Some(black_box(&dir).clone()),
            cli_filter: None,
        })
    });
    Ok(vec![Measured::new("log/install_early", Value::Time(time))])
}
