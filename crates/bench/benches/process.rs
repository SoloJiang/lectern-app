//! Process-level benchmarks: `app/*`, `plugin/*`, `lsp/*`, `baseline/*`
//! (docs/design/bench.md「进程级基准」). They launch the empty GPUI window
//! (and, from bench step 5, `lectern`) and run only on macOS; the runner
//! skips this target elsewhere. The scheduling is `lectern_bench::process`;
//! this file gives it the real processes: `std::process` and
//! `platform::process`.

use std::io;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, SystemTime};

use lectern_bench::budgets::fmt_bytes;
use lectern_bench::process::{ColdStart, Exit, Launch, Launched, Launcher, ProcessInfo, Scheduler};
use lectern_bench::wire::TimeSource;
use lectern_bench::{Bench, BenchError, Ctx, Measured, Mode, Target, Value};

const BENCHES: &[Bench] = &[
    Bench {
        id: "baseline/empty_window_cold_start",
        run: baseline_cold_start,
    },
    Bench {
        id: "baseline/empty_window_idle",
        run: baseline_idle,
    },
];

fn main() -> ExitCode {
    lectern_bench::main(Target::Process, env!("CARGO_TARGET_TMPDIR"), BENCHES)
}

/// The `empty_window` example, from `LECTERN_BENCH_BASELINE` (`just bench`
/// builds it and sets the variable on macOS). Missing: an error when
/// measuring, `skipped` in smoke mode (「状态与异常」).
fn baseline(ctx: &Ctx) -> Result<PathBuf, BenchError> {
    match std::env::var_os("LECTERN_BENCH_BASELINE") {
        Some(path) if !path.is_empty() => Ok(PathBuf::from(path)),
        _ if ctx.mode() == Mode::Smoke => Err(BenchError::skip("没有设置 LECTERN_BENCH_BASELINE")),
        _ => Err(BenchError::new(
            "没有设置 LECTERN_BENCH_BASELINE（just bench 在 macOS 上构建 empty_window 并设置它）",
        )),
    }
}

/// `baseline/empty_window_cold_start`: 2 warm-up launches, then 20, each
/// from the process's creation to the first frame's time: its
/// `presentedTime`, or the moment its presented handler ran where the
/// machine gives none (ADR 0052). The note gives what ADR 0017's rule for
/// `app/cold_start` would see, the sources and every sample.
fn baseline_cold_start(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let program = baseline(ctx)?;
    let scheduler = Scheduler::new(&SystemLauncher, &PlatformInfo);
    let cold = scheduler.cold_start(&program, ctx.scale(2, 0), ctx.scale(20, 1))?;
    let note = cold_start_note(&cold);
    Ok(vec![
        Measured::new(
            "baseline/empty_window_cold_start",
            Value::Time(cold.samples),
        )
        .with_note(Some(note)),
    ])
}

/// How many samples are over 20 ms, and the largest (ADR 0017: at most one
/// of 20 over 20 ms, none over 30 ms); how many first-frame times came from
/// each source; every sample in launch order, in ms.
fn cold_start_note(cold: &ColdStart) -> String {
    let samples = &cold.samples.0;
    let over = samples
        .iter()
        .filter(|&&t| t > Duration::from_millis(20))
        .count();
    let max = samples.iter().max().copied().unwrap_or_default();
    let each: Vec<String> = samples
        .iter()
        .map(|t| format!("{:.2}", t.as_secs_f64() * 1e3))
        .collect();
    format!(
        "{over} of {} over 20 ms, the longest {:.2} ms; {}; samples ms {}",
        samples.len(),
        max.as_secs_f64() * 1e3,
        source_counts(&cold.sources),
        each.join(", ")
    )
}

/// "source presented 18, handler 2" (ADR 0052).
fn source_counts(sources: &[TimeSource]) -> String {
    let count = |source| sources.iter().filter(|&&s| s == source).count();
    format!(
        "source presented {}, handler {}",
        count(TimeSource::Presented),
        count(TimeSource::Handler)
    )
}

/// `baseline/empty_window_idle`: 3 launches (at most 3 at once), each the
/// main process's `phys_footprint` 5 s after the first frame's time.
/// There is no kernel, so no quiescence to wait for (「进程级基准」). The
/// note gives the first frames' sources (ADR 0052) and every value.
fn baseline_idle(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let program = baseline(ctx)?;
    let scheduler = Scheduler::new(&SystemLauncher, &PlatformInfo);
    let runs = scheduler.memory(ctx.scale(3, 1), |scheduler| {
        let mut session = scheduler.session(&program, None)?;
        session.settle_after_first_frame()?;
        let bytes = session.footprint()?;
        let source = session.source();
        session.quit()?;
        Ok((bytes, source))
    })?;
    let (bytes, sources): (Vec<u64>, Vec<TimeSource>) = runs.into_iter().unzip();
    let each: Vec<String> = bytes.iter().map(|&b| fmt_bytes(b)).collect();
    let note = format!(
        "first frame {}; values {}",
        source_counts(&sources),
        each.join(", ")
    );
    Ok(vec![
        Measured::new("baseline/empty_window_idle", Value::Bytes(bytes)).with_note(Some(note)),
    ])
}

/// `std::process`: each program the leader of a new process group, with
/// stdin and stdout closed; stderr stays the runner's, for a crash.
struct SystemLauncher;

impl Launcher for SystemLauncher {
    fn launch(&self, launch: &Launch) -> io::Result<Box<dyn Launched>> {
        let mut command = Command::new(&launch.program);
        for (name, value) in &launch.env {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            };
        }
        let child = command
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()?;
        Ok(Box::new(SystemChild(child)))
    }
}

struct SystemChild(Child);

impl Launched for SystemChild {
    fn pid(&self) -> u32 {
        self.0.id()
    }

    fn try_wait(&mut self) -> io::Result<Option<Exit>> {
        Ok(self.0.try_wait()?.map(|status| Exit {
            code: status.code(),
        }))
    }

    fn kill_group(&mut self) {
        // The group's ID is the leader's pid (`process_group(0)`); the
        // leader is not reaped yet, so the ID still names this group.
        if let Ok(pid) = i32::try_from(self.0.id()) {
            sigkill(-pid);
        }
    }

    fn kill(&mut self, pid: u32) {
        if let Ok(pid) = i32::try_from(pid) {
            sigkill(pid);
        }
    }
}

/// `kill(target, SIGKILL)`: a process, or with a negative `target` the
/// process group `-target`. A target that no longer exists is no error.
fn sigkill(target: i32) {
    // SAFETY: kill(2) takes two integers and touches no memory of this
    // process; its failure (ESRCH for a process that is gone) only leaves
    // `errno` set, which nothing reads.
    unsafe {
        libc::kill(target, libc::SIGKILL);
    }
}

/// `platform::process` (macOS; `Unsupported` elsewhere, where the runner
/// skips this target).
struct PlatformInfo;

impl ProcessInfo for PlatformInfo {
    fn start_time(&self, pid: u32) -> io::Result<SystemTime> {
        lectern_platform::process::start_time(pid)
    }

    fn phys_footprint(&self, pid: u32) -> io::Result<u64> {
        lectern_platform::process::phys_footprint(pid)
    }

    fn children(&self, pid: u32) -> io::Result<Vec<u32>> {
        lectern_platform::process::children(pid)
    }
}
