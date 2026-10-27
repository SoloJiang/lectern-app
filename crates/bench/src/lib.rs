//! Performance budget benchmarks (docs/design/bench.md).
//!
//! A runner for `harness = false` benchmark targets: each target lists its
//! [`Bench`]es and calls [`main`], which samples them, compares every result
//! with its entry in the one budget table ([`budgets::BUDGETS`]), prints a
//! table and writes `target/lectern-bench/report-<target>.json`. The
//! `lectern-bench-report` binary merges the targets' reports into
//! `report.json` and gives `just bench` its exit code.
//!
//! Exit codes: 0 nothing over budget and no error; 1 a result judged over
//! budget (only on Apple silicon macOS, in measure mode); 2 a benchmark
//! failed, or the benchmarks and the budget table disagree.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a tool crate: the runner prints its table and errors for the terminal and CI (docs/design/logging.md「对其他 crate 的要求」)"
)]

mod alloc;
pub mod budgets;
mod ctx;
pub mod merge;
#[cfg(feature = "probe")]
pub mod probe;
pub mod process;
mod report;
pub mod rounds;
mod runner;
pub mod shards;
mod stats;
pub mod wire;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

pub use alloc::CountingAlloc;
pub use budgets::Target;
pub use ctx::{Ctx, SampleConfig};

/// One benchmark. `run` is called on the benchmark process's main thread
/// and may block. It can measure several scenarios and return one result
/// for each; every result ID must be in the budget table.
#[derive(Clone, Copy)]
pub struct Bench {
    /// The prefix of its result IDs, for filtering, such as
    /// "agents/parse_throughput" or "kernel/service_for_each".
    pub id: &'static str,
    pub run: fn(&mut Ctx) -> Result<Vec<Measured>, BenchError>,
}

/// One result.
#[derive(Clone, Debug)]
pub struct Measured {
    /// "project/resolve_package/hit".
    pub id: String,
    pub value: Value,
    /// What the report and its annotations say beside the value, such as
    /// the diagnosis of a round the time box cut ([`rounds`]).
    pub note: Option<String>,
}

impl Measured {
    pub fn new(id: impl Into<String>, value: Value) -> Measured {
        Measured {
            id: id.into(),
            value,
            note: None,
        }
    }

    /// The same with `note` beside the value (None: none).
    pub fn with_note(mut self, note: Option<String>) -> Measured {
        self.note = note;
        self
    }
}

#[derive(Clone, Debug)]
pub enum Value {
    /// The time of one operation (already divided by the batch size).
    Time(Samples),
    /// Each operation processes `bytes` bytes; the statistic is
    /// `bytes / time`.
    Throughput { bytes: u64, time: Samples },
    /// Subject and baseline sampled alternately; the ratio is taken with the
    /// same statistic, and the subject's time can also have an absolute
    /// limit.
    Ratio { subject: Samples, baseline: Samples },
    /// Bytes: the counting allocator's live bytes, or `phys_footprint`.
    Bytes(Vec<u64>),
    /// A count observed once, such as the files one operation read. Record
    /// only: no check applies to it.
    Count(u64),
    /// A scroll, input or animation: each frame's work
    /// (`completed - requested`) and the interval between consecutive
    /// frames' `started`.
    Frames {
        work: Samples,
        intervals: Samples,
        refresh: Duration,
    },
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Samples(pub Vec<Duration>);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// `cargo bench`: sampled as「平台与噪声」describes; compared with the
    /// budgets on the gating platform.
    Measure,
    /// `cargo test --benches`: each benchmark runs once on small inputs,
    /// only to check that it runs. Budgets are not compared.
    Smoke,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Measure => "measure",
            Mode::Smoke => "smoke",
        }
    }
}

/// A benchmark failed: its fixture could not be built, or the operation
/// under test returned an error. Recorded as `error`, exit code 2; or, from
/// [`BenchError::skip`], `skipped`.
#[derive(Debug)]
pub struct BenchError {
    message: String,
    skip: bool,
}

impl BenchError {
    pub fn new(message: impl Into<String>) -> BenchError {
        BenchError {
            message: message.into(),
            skip: false,
        }
    }

    /// The benchmark cannot run here and that is not a failure, such as a
    /// process benchmark without its program in smoke mode or
    /// `app/cold_start_purged` without `sudo -n purge`
    /// (docs/design/bench.md「状态与异常」): its entries are `skipped`, with
    /// `reason`.
    pub fn skip(reason: impl Into<String>) -> BenchError {
        BenchError {
            message: reason.into(),
            skip: true,
        }
    }

    pub fn is_skip(&self) -> bool {
        self.skip
    }

    /// For errors that only implement `Debug`.
    pub fn debug(context: &str, error: impl std::fmt::Debug) -> BenchError {
        BenchError::new(format!("{context}: {error:?}"))
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl<E: std::error::Error> From<E> for BenchError {
    fn from(error: E) -> BenchError {
        BenchError::new(error.to_string())
    }
}

/// The entry point of a benchmark target. `tmp_dir` is the target's
/// `env!("CARGO_TARGET_TMPDIR")`: scratch directories go under
/// `<tmp_dir>/lectern-bench/<id>/`, the report into
/// `<target dir>/lectern-bench/report-<target>.json`.
///
/// On macOS it first sets the calling thread's QoS to user-interactive,
/// and exits with 2 when that fails.
///
/// Arguments: `--bench` (passed by `cargo bench`) selects measure mode,
/// otherwise smoke; `--list` lists the target's budgets; `--shard <name>`
/// limits the run to one shard's budgets ([`shards`], the others are
/// `sharded`; an unknown name exits with 2 before anything runs); anything
/// else is a substring filter on IDs. Checks the benchmarks against the budget table,
/// runs them in ID order, prints the table, writes the report and returns
/// the exit code.
///
/// `LECTERN_BENCH_JUDGE=1` marks the judging machine (ADR 0052); any value
/// other than `1` or unset exits with 2.
pub fn main(target: Target, tmp_dir: &str, benches: &[Bench]) -> ExitCode {
    // The benchmark thread on the performance cores; without it the numbers
    // cannot be trusted (「数据流与线程」).
    #[cfg(target_os = "macos")]
    if let Err(error) = lectern_platform::thread::set_user_interactive() {
        eprintln!("lectern-bench: could not set the user-interactive QoS: {error}");
        return ExitCode::from(2);
    }
    // A value that is neither `1` nor unset fails the run without a report,
    // so the merge fails too (ADR 0052).
    let judge = match runner::judge_from_env(std::env::var_os(runner::JUDGE_VAR).as_deref()) {
        Ok(judge) => judge,
        Err(message) => {
            eprintln!("lectern-bench: {message}");
            return ExitCode::from(2);
        }
    };
    let tmp = PathBuf::from(tmp_dir);
    let report_dir = tmp
        .parent()
        .map_or_else(|| tmp.join("lectern-bench"), |t| t.join("lectern-bench"));
    let options = runner::Options {
        target,
        budgets: budgets::BUDGETS,
        args: std::env::args().skip(1).collect(),
        apple_silicon: cfg!(all(target_os = "macos", target_arch = "aarch64")),
        macos: cfg!(target_os = "macos"),
        scratch_root: tmp.join("lectern-bench"),
        fixtures_dir: report_dir.join("fixtures"),
        report_dir,
        config: SampleConfig::default(),
        host: report::Host::current(),
        commit: std::env::var("GITHUB_SHA").ok(),
        judge,
        shards: shards::SHARDS,
    };
    let run = runner::run(&options, benches);
    if run.exit == 2 && run.report.is_none() {
        // A bad command line, not a table.
        eprint!("{}", run.text);
        return ExitCode::from(2);
    }
    print!("{}", run.text);
    if let Some(report) = &run.report
        && let Err(error) = report::write(&options.report_dir, target, report)
    {
        eprintln!("lectern-bench: could not write the report: {error}");
        return ExitCode::from(2);
    }
    ExitCode::from(run.exit)
}

#[cfg(test)]
mod tests;
