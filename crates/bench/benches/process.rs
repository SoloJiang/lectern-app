//! Process-level benchmarks: `app/*`, `plugin/*`, `lsp/*`, `baseline/*`
//! (docs/design/bench.md「进程级基准」). They launch the empty GPUI window
//! (`LECTERN_BENCH_BASELINE`) and the measurement build of `lectern`
//! (`LECTERN_BENCH_APP`, the `bench-probe` feature), and run only on macOS;
//! the runner skips this target elsewhere. The scheduling and what each
//! measurement reads is `lectern_bench::process`; this file gives it the
//! real processes (`std::process` and `platform::process`) and turns what
//! it measured into results.

use std::io;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, SystemTime};

use lectern_bench::budgets::{BUDGETS, Limit, fmt_bytes, lookup};
use lectern_bench::process::{
    ColdStart, Exit, Idle, Launch, Launched, Launcher, Limits, NEVER_QUIESCENT, PluginPair,
    ProcessInfo, Scheduler, purge,
};
use lectern_bench::wire::TimeSource;
use lectern_bench::{Bench, BenchError, Ctx, Measured, Mode, Samples, Target, Value};

/// Benchmarks are matched with budgets by ID prefix, the longest prefix
/// first: `plugin` produces `plugin/*/activate`, `plugin/*/resident` and
/// the per-plugin entries, `plugin/after_first_frame` its own.
const BENCHES: &[Bench] = &[
    Bench {
        id: "app/cold_start",
        run: app_cold_start,
    },
    Bench {
        id: "app/cold_start_purged",
        run: app_cold_start_purged,
    },
    Bench {
        id: "app/idle_empty",
        run: app_idle_empty,
    },
    Bench {
        id: "app/unclassified_children",
        run: app_unclassified_children,
    },
    Bench {
        id: "baseline/empty_window_cold_start",
        run: baseline_cold_start,
    },
    Bench {
        id: "baseline/empty_window_idle",
        run: baseline_idle,
    },
    Bench {
        id: "plugin",
        run: plugins,
    },
    Bench {
        id: "plugin/after_first_frame",
        run: plugin_after_first_frame,
    },
];

fn main() -> ExitCode {
    lectern_bench::main(Target::Process, env!("CARGO_TARGET_TMPDIR"), BENCHES)
}

/// The program named by the environment variable `var` (`just bench` builds
/// it on macOS and sets the variable). Missing: an error when measuring,
/// `skipped` in smoke mode (「状态与异常」).
fn program(ctx: &Ctx, var: &str, built: &str) -> Result<PathBuf, BenchError> {
    match std::env::var_os(var) {
        Some(path) if !path.is_empty() => Ok(PathBuf::from(path)),
        _ if ctx.mode() == Mode::Smoke => Err(BenchError::skip(format!("没有设置 {var}"))),
        _ => Err(BenchError::new(format!(
            "没有设置 {var}（just bench 在 macOS 上构建 {built} 并设置它）"
        ))),
    }
}

/// The `empty_window` example, from `LECTERN_BENCH_BASELINE`.
fn baseline(ctx: &Ctx) -> Result<PathBuf, BenchError> {
    program(ctx, "LECTERN_BENCH_BASELINE", "empty_window")
}

/// The measurement build of `lectern`, from `LECTERN_BENCH_APP`.
fn app(ctx: &Ctx) -> Result<PathBuf, BenchError> {
    program(ctx, "LECTERN_BENCH_APP", "带 bench-probe 的 lectern")
}

/// The scheduler of the benchmarks of `lectern`. A smoke run only checks
/// that the launches work: it does not wait out the quiet time of an idle
/// sample.
fn scheduler(ctx: &Ctx) -> Scheduler<'static> {
    let scheduler = Scheduler::new(&SystemLauncher, &PlatformInfo).judging(ctx.judge());
    if ctx.mode() == Mode::Smoke {
        scheduler.with_limits(Limits {
            settle: Duration::from_millis(200),
            ..Limits::default()
        })
    } else {
        scheduler
    }
}

/// `app/cold_start`: the cold start of `lectern` as `baseline/…` measures
/// the empty window's: 2 warm-up launches, then 20 (ADR 0017). A judging
/// machine's entry (ADR 0052).
fn app_cold_start(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let program = app(ctx)?;
    let cold = scheduler(ctx).cold_start(&program, ctx.scale(2, 0), ctx.scale(20, 1))?;
    let note = cold_start_note(&cold);
    Ok(vec![
        Measured::new("app/cold_start", Value::Time(cold.samples)).with_note(Some(note)),
    ])
}

/// `app/cold_start_purged`: one launch after `sudo -n purge` (ADR 0017),
/// record only. `skipped` where `sudo -n purge` does not work; a smoke run
/// does not purge.
fn app_cold_start_purged(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let program = app(ctx)?;
    if ctx.mode() == Mode::Smoke {
        return Err(BenchError::skip("冒烟模式不清空文件缓存"));
    }
    if !purge(None)? {
        return Err(BenchError::skip(
            "sudo -n purge 不可用（没有 sudo、要密码或 purge 失败）",
        ));
    }
    let cold = scheduler(ctx).cold_start(&program, 0, 1)?;
    let note = cold_start_note(&cold);
    Ok(vec![
        Measured::new("app/cold_start_purged", Value::Time(cold.samples)).with_note(Some(note)),
    ])
}

/// `app/idle_empty`: 3 launches of the whole profile (at most 3 at once),
/// each the main process's `phys_footprint` at a valid idle sample (kernel
/// 「静止」, 5 s after the last `Quiescent`). A launch that was not quiescent
/// 30 s after its first frame fails ADR 0017's budget: the value is then
/// [`NEVER_QUIESCENT`], over every budget.
fn app_idle_empty(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let program = app(ctx)?;
    let runs = scheduler(ctx).idle_footprints(&program, ctx.scale(3, 1))?;
    let total = runs.len();
    let bytes: Vec<u64> = runs
        .into_iter()
        .filter_map(|run| match run {
            Idle::Sample(bytes) => Some(bytes),
            Idle::NotQuiescent => None,
        })
        .collect();
    let (value, note) = if bytes.len() < total {
        (
            vec![NEVER_QUIESCENT],
            format!(
                "{} of {total} launches were not quiescent 30 s after the first frame",
                total - bytes.len()
            ),
        )
    } else {
        let each: Vec<String> = bytes.iter().map(|&b| fmt_bytes(b)).collect();
        (bytes, format!("values {}", each.join(", ")))
    };
    Ok(vec![
        Measured::new("app/idle_empty", Value::Bytes(value)).with_note(Some(note)),
    ])
}

/// `app/unclassified_children`: record only; the descendants of the main
/// process that no `Child` report accounts for (ADR 0018).
fn app_unclassified_children(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let program = app(ctx)?;
    let found = scheduler(ctx).unclassified_children(&program)?;
    Ok(vec![
        Measured::new("app/unclassified_children", Value::Bytes(vec![found.bytes]))
            .with_note(Some(format!("{} processes", found.processes))),
    ])
}

/// `plugin/after_first_frame`: 3 launches of the whole profile, one at a
/// time; the activation times of the plugins with an `AfterFirstFrame`
/// condition added up.
fn plugin_after_first_frame(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let program = app(ctx)?;
    let sums = scheduler(ctx).after_first_frame(&program, ctx.scale(3, 1))?;
    let note = format!("sums {}", microseconds(&sums.0));
    Ok(vec![
        Measured::new("plugin/after_first_frame", Value::Time(sums)).with_note(Some(note)),
    ])
}

/// `plugin/<id>/activate` and `plugin/<id>/resident` for each plugin of the
/// profile: a pair of launches each, the plugin's dependency closure
/// without it and with it. One pair; two more when its activation time is
/// over the budget (「进程级基准」). One at a time: the activation times are
/// timings.
fn plugins(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let program = app(ctx)?;
    let scheduler = scheduler(ctx);
    let ids: Vec<String> = scheduler
        .plugin_list(&program)?
        .into_iter()
        .map(|plugin| plugin.id)
        .collect();
    if ids.is_empty() {
        return Err(BenchError::new("Plugins 的应答里没有插件"));
    }
    let measure = ctx.mode() == Mode::Measure;
    let mut results = Vec::new();
    for id in &ids {
        let limit = activate_limit(id);
        let pairs = scheduler.plugin(&program, id, |activate| {
            measure
                && limit
                    .as_ref()
                    .is_some_and(|limit| limit.exceeded_by(activate))
        })?;
        results.push(
            Measured::new(
                format!("plugin/{id}/activate"),
                Value::Time(Samples(pairs.iter().map(|pair| pair.activate).collect())),
            )
            .with_note(Some(format!(
                "{} pair(s), activation {}",
                pairs.len(),
                microseconds(&pairs.iter().map(|pair| pair.activate).collect::<Vec<_>>())
            ))),
        );
        results.push(
            Measured::new(
                format!("plugin/{id}/resident"),
                Value::Bytes(pairs.iter().map(PluginPair::resident).collect()),
            )
            .with_note(Some(format!(
                "{} pair(s), with minus without {}",
                pairs.len(),
                signed(&pairs)
            ))),
        );
    }
    ctx.report_plugins(ids);
    Ok(results)
}

/// The upper bound of `plugin/<id>/activate`'s median, from the budget table.
fn activate_limit(id: &str) -> Option<ActivateLimit> {
    let budget = lookup(BUDGETS, &format!("plugin/{id}/activate"))?;
    budget.checks.iter().find_map(|check| match check.limit {
        Limit::TimeBelow(limit) => Some(ActivateLimit {
            limit,
            inclusive: false,
        }),
        Limit::TimeAtMost(limit) => Some(ActivateLimit {
            limit,
            inclusive: true,
        }),
        _ => None,
    })
}

struct ActivateLimit {
    limit: Duration,
    inclusive: bool,
}

impl ActivateLimit {
    fn exceeded_by(&self, time: Duration) -> bool {
        if self.inclusive {
            time > self.limit
        } else {
            time >= self.limit
        }
    }
}

/// Durations in µs, in order, for a note.
fn microseconds(times: &[Duration]) -> String {
    let each: Vec<String> = times
        .iter()
        .map(|t| format!("{:.1} µs", t.as_secs_f64() * 1e6))
        .collect();
    each.join(", ")
}

/// A pair's footprint difference with its sign, for a note.
fn signed(pairs: &[PluginPair]) -> String {
    let each: Vec<String> = pairs
        .iter()
        .map(|pair| {
            let difference = pair.difference();
            let sign = if difference < 0 { "-" } else { "+" };
            format!("{sign}{}", fmt_bytes(difference.unsigned_abs() as u64))
        })
        .collect();
    each.join(", ")
}

/// `baseline/empty_window_cold_start`: 2 warm-up launches, then 20, each
/// from the process's creation to the first frame's time: its
/// `presentedTime`, or the moment its presented handler ran where the
/// machine gives none (ADR 0052); on the judging machine such a launch is
/// an error. The note gives what ADR 0017's rule for `app/cold_start`
/// would see, the sources and every sample.
fn baseline_cold_start(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let program = baseline(ctx)?;
    let scheduler = Scheduler::new(&SystemLauncher, &PlatformInfo).judging(ctx.judge());
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
    let scheduler = Scheduler::new(&SystemLauncher, &PlatformInfo).judging(ctx.judge());
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
