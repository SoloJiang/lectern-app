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
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, SystemTime};

use lectern_bench::budgets::{BUDGETS, Limit, fmt_bytes, lookup};
use lectern_bench::process::{
    ColdStart, Exit, Idle, Launch, Launched, Launcher, Limits, NEVER_QUIESCENT, PluginPair,
    ProcessInfo, Scheduler, Session, purge,
};
use lectern_bench::wire::{Milestone, OpenTarget, TimeSource};
use lectern_bench::{Bench, BenchError, Ctx, Measured, Mode, Samples, Target, Value};
use lectern_project::testing::Shape;

#[allow(dead_code, reason = "each target uses its own subset of the fixtures")]
#[path = "fixtures/git_worktree.rs"]
mod git_worktree;
#[path = "fixtures/reader_files.rs"]
mod reader_files;
#[path = "fixtures/review_session.rs"]
mod review_session;

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
        id: "app/idle_monorepo",
        run: app_idle_monorepo,
    },
    Bench {
        id: "app/idle_100k",
        run: app_idle_100k,
    },
    Bench {
        id: "app/open_project",
        run: app_open_project,
    },
    Bench {
        id: "app/open_diff",
        run: app_open_diff,
    },
    Bench {
        id: "app/open_file",
        run: app_open_file,
    },
    Bench {
        id: "app/nav_back",
        run: app_nav_back,
    },
    Bench {
        id: "app/enter_edit",
        run: app_enter_edit,
    },
    Bench {
        id: "app/files_open",
        run: app_files_open,
    },
    Bench {
        id: "app/files_expand",
        run: app_files_expand,
    },
    Bench {
        id: "app/switch_worktree",
        run: app_switch_worktree,
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
    Ok(vec![idle_measured("app/idle_empty", runs, None)])
}

/// An idle result: the footprints, or [`NEVER_QUIESCENT`] when a launch was
/// not quiescent in time.
fn idle_measured(id: &str, runs: Vec<Idle<u64>>, fixture: Option<&str>) -> Measured {
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
                "{} of {total} launches were not quiescent 30 s after the first frame or the open",
                total - bytes.len()
            ),
        )
    } else {
        let each: Vec<String> = bytes.iter().map(|&b| fmt_bytes(b)).collect();
        (bytes, format!("values {}", each.join(", ")))
    };
    let note = match fixture {
        Some(fixture) => format!("{fixture}; {note}"),
        None => note,
    };
    Measured::new(id, Value::Bytes(value)).with_note(Some(note))
}

/// The wait for an `Open` milestone (「进程级基准」: 30 s or 1.5 times the
/// budget, whichever is longer; every budget here is under 20 s).
const OPEN_LIMIT: Duration = Duration::from_secs(30);

/// Waits for an idle sample with nothing in it: the launch (or what it
/// opened) has settled. Not quiescent in time is an error.
fn settle(session: &mut Session<'_>, id: &str) -> Result<(), BenchError> {
    match session.idle(|_| Ok(()))? {
        Idle::Sample(()) => Ok(()),
        Idle::NotQuiescent => Err(BenchError::new(format!(
            "{id}：启动或打开之后 30 秒内没有静止"
        ))),
    }
}

/// `app/idle_monorepo` and `app/idle_100k`: 3 launches of the whole
/// profile, one at a time; each opens `root` once the empty window has
/// settled, then takes the main process's footprint at a valid idle
/// sample. No pausable background work runs in the desktop profile yet
/// (`code_intel`, #17, is not assembled), so nothing is paused (bench
/// design「与设计的差异」).
fn idle_after_open(
    ctx: &Ctx,
    id: &str,
    root: &std::path::Path,
) -> Result<Vec<Idle<u64>>, BenchError> {
    let program = app(ctx)?;
    let scheduler = scheduler(ctx);
    let mut runs = Vec::new();
    for _ in 0..ctx.scale(3, 1) {
        let mut session = scheduler.session(&program, None)?;
        settle(&mut session, id)?;
        session.open(
            OpenTarget::Project(root.to_owned()),
            Milestone::ProjectHome,
            OPEN_LIMIT,
        )?;
        runs.push(session.idle(|s| s.footprint())?);
        session.quit()?;
    }
    Ok(runs)
}

/// `app/idle_monorepo`: the `cloudprint` shape (728 files, 11 worktrees,
/// about 25 manifests) open.
fn app_idle_monorepo(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let repo = git_worktree::shaped(&Shape::cloudprint());
    let runs = idle_after_open(ctx, "app/idle_monorepo", repo.root())?;
    Ok(vec![idle_measured(
        "app/idle_monorepo",
        runs,
        Some("cloudprint shape"),
    )])
}

/// `app/idle_100k`: the 100 000-file shape of the project design (2 000
/// manifests) open; 2 000 files in a smoke run.
fn app_idle_100k(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (files, manifests) = ctx.scale((100_000, 2_000), (2_000, 40));
    let repo = git_worktree::large(files, manifests);
    let runs = idle_after_open(ctx, "app/idle_100k", repo.root())?;
    Ok(vec![idle_measured(
        "app/idle_100k",
        runs,
        Some(&format!("{files} files, {manifests} manifests")),
    )])
}

/// `app/open_project`: 5 launches (「端到端」), one at a time. Each waits
/// for the empty window to settle, then sends `Open(Project)` of the
/// `cloudprint` shape: from the request to the `ProjectHome` milestone's
/// frame.
fn app_open_project(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let program = app(ctx)?;
    let repo = git_worktree::shaped(&Shape::cloudprint());
    let scheduler = scheduler(ctx);
    let mut times = Vec::new();
    let mut inside = Vec::new();
    for _ in 0..ctx.scale(5, 1) {
        let mut session = scheduler.session(&program, None)?;
        settle(&mut session, "app/open_project")?;
        times.push(session.open(
            OpenTarget::Project(repo.root().to_owned()),
            Milestone::ProjectHome,
            OPEN_LIMIT,
        )?);
        inside.push(session.quit_reading_home(|home| {
            logged(home, "home milestone reported", "since_opened_us")
        })?);
    }
    let note = format!(
        "cloudprint shape; times {}; in the app (since_opened_us) {}",
        microseconds(&times),
        logged_values(&inside)
    );
    Ok(vec![
        Measured::new("app/open_project", Value::Time(Samples(times))).with_note(Some(note)),
    ])
}

/// `app/open_diff`: 5 launches, one at a time, each with a new `HOME` that
/// holds the fixture's Claude Code session (empty caches). Each opens the
/// project and lets it settle (the session is discovered and placed), then
/// sends `Open(Diff)` of the session: from the request to the `DiffShown`
/// milestone's frame, the first file's rows drawn.
fn app_open_diff(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let program = app(ctx)?;
    let (files, modules, turns) = ctx.scale((200, 50, 20), (20, 5, 2));
    let fixture = review_session::ReviewSession::new(files, modules, turns, SystemTime::now());
    let scheduler = scheduler(ctx);
    let mut times = Vec::new();
    let mut inside = Vec::new();
    for _ in 0..ctx.scale(5, 1) {
        let mut session = scheduler.session_in_home(&program, &|home| fixture.write_home(home))?;
        settle(&mut session, "app/open_diff")?;
        session.open(
            OpenTarget::Project(fixture.root().to_owned()),
            Milestone::ProjectHome,
            OPEN_LIMIT,
        )?;
        settle(&mut session, "app/open_diff")?;
        times.push(session.open(
            OpenTarget::Diff {
                session: fixture.session(),
            },
            Milestone::DiffShown,
            OPEN_LIMIT,
        )?);
        inside.push(session.quit_reading_home(|home| logged(home, "diff shown", "took_us"))?);
    }
    let note = format!(
        "{files} files, {modules} rewritten in {turns} turns of one session; times {}; in the app (took_us) {}",
        microseconds(&times),
        logged_values(&inside)
    );
    Ok(vec![
        Measured::new("app/open_diff", Value::Time(Samples(times))).with_note(Some(note)),
    ])
}

/// `app/switch_worktree`: 5 launches, one at a time. Each opens the
/// `cloudprint` shape, a repository with 11 worktrees, lets it settle, and
/// switches to one linked worktree untimed: that shows the Worktrees screen,
/// and the first show is not part of a switch. Once settled it sends
/// `Open(Worktree)` of another: from the request to the `WorktreeSwitched`
/// milestone's frame, the screen drawn with the settled snapshot of the new
/// worktree (docs/design/worktrees.md「性能」).
/// The roots are canonical, as `project` reports them.
fn app_switch_worktree(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let program = app(ctx)?;
    let (repo, roots) = git_worktree::shaped_linked(&Shape::cloudprint(), 10)?;
    let scheduler = scheduler(ctx);
    let mut times = Vec::new();
    for _ in 0..ctx.scale(5, 1) {
        let mut session = scheduler.session(&program, None)?;
        settle(&mut session, "app/switch_worktree")?;
        session.open(
            OpenTarget::Project(repo.root().to_owned()),
            Milestone::ProjectHome,
            OPEN_LIMIT,
        )?;
        settle(&mut session, "app/switch_worktree")?;
        session.open(
            OpenTarget::Worktree(roots[1].clone()),
            Milestone::WorktreeSwitched,
            OPEN_LIMIT,
        )?;
        settle(&mut session, "app/switch_worktree")?;
        times.push(session.open(
            OpenTarget::Worktree(roots[0].clone()),
            Milestone::WorktreeSwitched,
            OPEN_LIMIT,
        )?);
        session.quit()?;
    }
    let note = format!(
        "cloudprint shape, 11 worktrees; times {}",
        microseconds(&times)
    );
    Ok(vec![
        Measured::new("app/switch_worktree", Value::Time(Samples(times))).with_note(Some(note)),
    ])
}

/// A launch of the whole profile that has opened `root` and settled: the
/// state every end-to-end file benchmark starts from (the empty window
/// settles first, as in `app/open_diff`).
fn launch_with_project<'a>(
    scheduler: &'a Scheduler<'a>,
    program: &Path,
    root: &Path,
    id: &str,
) -> Result<Session<'a>, BenchError> {
    let mut session = scheduler.session(program, None)?;
    settle(&mut session, id)?;
    session.open(
        OpenTarget::Project(root.to_owned()),
        Milestone::ProjectHome,
        OPEN_LIMIT,
    )?;
    settle(&mut session, id)?;
    Ok(session)
}

/// The middle sample of `times` (the larger of the two in the middle for an
/// even count), as the runner's median.
fn median(times: &[Duration]) -> Duration {
    let mut sorted = times.to_vec();
    sorted.sort();
    sorted.get(sorted.len() / 2).copied().unwrap_or_default()
}

/// `app/open_file`: for each scenario of ADR 0034 and ADR 0042, 5 launches,
/// one at a time. Each opens the fixture project and lets it settle, opens a
/// small file of the scenario's language (the first use of a language in
/// the run compiles its highlight query, outside the budget: ADR 0064; its
/// time is `app/open_file/first`, record only), lets it settle, closes the
/// reading view with Esc and lets it settle, then
/// sends `Open(File)` of the scenario's file (as `Reader::open` takes it,
/// at the scenario's byte offset): from the request to the
/// `FileHighlighted` milestone's frame, the first screen drawn and
/// highlighted. The scenarios are an ordinary file of the `cloudprint`
/// shape, 4 MiB and 64 MiB of TypeScript and the middle of a 4 MiB
/// `package-lock.json` at a byte offset, 4 MiB of JavaScript in one line,
/// and the old side of a 4 MiB file the review's diff of the worktree's
/// changes shows (its blob shared by the review, opened at the changed line).
/// The budget applies to every scenario, so the result is the samples of
/// the one with the largest median; the note gives each scenario's median.
fn app_open_file(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let program = app(ctx)?;
    let repo = reader_files::ReaderRepo::cloudprint()?;
    let scenarios = repo.large(ctx);
    let changed =
        reader_files::Changed::new(ctx.scale(reader_files::CHANGED_LEN, reader_files::MIB))?;
    let scheduler = scheduler(ctx);
    let launches = ctx.scale(5, 1);
    let mut results: Vec<(String, Vec<Duration>)> = Vec::new();
    let mut firsts: Vec<(String, Vec<Duration>)> = Vec::new();
    for scenario in &scenarios {
        let mut times = Vec::new();
        let mut first = Vec::new();
        for _ in 0..launches {
            let mut session =
                launch_with_project(&scheduler, &program, repo.root(), "app/open_file")?;
            // The language's first use in the run compiles its highlight
            // query, which the budget does not count (ADR 0064): a small
            // file of the same language is opened first, its time recorded
            // as `app/open_file/first`. Esc closes the reading view, so the
            // timed open starts from the project as the first one did (the
            // first screen starts when the request arrives).
            first.push(session.open(
                OpenTarget::File {
                    path: scenario.warm.clone(),
                    byte: None,
                    old: None,
                },
                Milestone::FileHighlighted,
                OPEN_LIMIT,
            )?);
            settle(&mut session, "app/open_file")?;
            session.request(OpenTarget::Key("escape".to_owned()))?;
            settle(&mut session, "app/open_file")?;
            times.push(session.open(
                OpenTarget::File {
                    path: scenario.path.clone(),
                    byte: scenario.byte,
                    old: None,
                },
                Milestone::FileHighlighted,
                OPEN_LIMIT,
            )?);
            session.quit()?;
        }
        results.push((
            format!("{} ({} bytes)", scenario.name, scenario.bytes),
            times,
        ));
        firsts.push((format!("{} warm-up", scenario.name), first));
    }
    // The old side: the review shows the diff of the changes first (it
    // shares both sides' text with the reader), then the hunk's old line is
    // opened by its blob and byte offset.
    let mut times = Vec::new();
    for _ in 0..launches {
        let mut session =
            launch_with_project(&scheduler, &program, changed.root(), "app/open_file")?;
        session.open(OpenTarget::Changes, Milestone::DiffShown, OPEN_LIMIT)?;
        settle(&mut session, "app/open_file")?;
        times.push(session.open(
            OpenTarget::File {
                path: changed.path(),
                byte: Some(changed.byte),
                old: Some(changed.oid.clone()),
            },
            Milestone::FileHighlighted,
            OPEN_LIMIT,
        )?);
        session.quit()?;
    }
    results.push((format!("old side ({} bytes)", changed.bytes), times));
    let (note, worst) = slowest(results)?;
    let (first_note, first) = slowest(firsts)?;
    Ok(vec![
        Measured::new("app/open_file", Value::Time(Samples(worst)))
            .with_note(Some(format!("the slowest scenario's samples; {note}"))),
        Measured::new("app/open_file/first", Value::Time(Samples(first))).with_note(Some(format!(
            "the first open of a language in a run, record only (ADR 0064); {first_note}"
        ))),
    ])
}

/// Each scenario's median in a note, and the samples of the scenario with
/// the largest median.
fn slowest(results: Vec<(String, Vec<Duration>)>) -> Result<(String, Vec<Duration>), BenchError> {
    let note = results
        .iter()
        .map(|(name, times)| {
            format!(
                "{name} median {:.1} µs, times {}",
                median(times).as_secs_f64() * 1e6,
                microseconds(times)
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let (_, worst) = results
        .into_iter()
        .max_by_key(|(_, times)| median(times))
        .ok_or_else(|| BenchError::new("no scenario"))?;
    Ok((note, worst))
}

/// `app/nav_back`: 5 launches, one at a time. Each opens the `cloudprint`
/// shape and lets it settle, opens one ordinary file and then another
/// (each settles), and sends the key ⌘[ to the focus: from the request to
/// the `FileHighlighted` milestone's frame, the first file shown again from
/// the history (a file of 1 MiB or less: from the LRU, without reading it).
fn app_nav_back(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let program = app(ctx)?;
    let repo = reader_files::ReaderRepo::cloudprint()?;
    let (first, second) = repo.pair();
    let scheduler = scheduler(ctx);
    let mut times = Vec::new();
    for _ in 0..ctx.scale(5, 1) {
        let mut session = launch_with_project(&scheduler, &program, repo.root(), "app/nav_back")?;
        for scenario in [&first, &second] {
            session.open(
                OpenTarget::File {
                    path: scenario.path.clone(),
                    byte: scenario.byte,
                    old: None,
                },
                Milestone::FileHighlighted,
                OPEN_LIMIT,
            )?;
            settle(&mut session, "app/nav_back")?;
        }
        times.push(session.open(
            OpenTarget::Key("cmd-[".to_owned()),
            Milestone::FileHighlighted,
            OPEN_LIMIT,
        )?);
        session.quit()?;
    }
    let note = format!(
        "two files of {} bytes; times {}",
        first.bytes,
        microseconds(&times)
    );
    Ok(vec![
        Measured::new("app/nav_back", Value::Time(Samples(times))).with_note(Some(note)),
    ])
}

/// `app/enter_edit`: 5 launches, one at a time. Each opens the `cloudprint`
/// shape, opens a TypeScript file at the edit limit (1 MiB, no long line,
/// ADR 0034) and lets it settle (the whole file is read, so Edit is
/// available), and sends the key ⌘E to the focus: from the request to the
/// `EditReady` milestone's frame, the editor drawn with the focus in it.
fn app_enter_edit(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let program = app(ctx)?;
    let repo = reader_files::ReaderRepo::cloudprint()?;
    let file = repo.editable();
    let scheduler = scheduler(ctx);
    let mut times = Vec::new();
    for _ in 0..ctx.scale(5, 1) {
        let mut session = launch_with_project(&scheduler, &program, repo.root(), "app/enter_edit")?;
        session.open(
            OpenTarget::File {
                path: file.path.clone(),
                byte: None,
                old: None,
            },
            Milestone::FileHighlighted,
            OPEN_LIMIT,
        )?;
        settle(&mut session, "app/enter_edit")?;
        times.push(session.open(
            OpenTarget::Key("cmd-e".to_owned()),
            Milestone::EditReady,
            OPEN_LIMIT,
        )?);
        session.quit()?;
    }
    let note = format!(
        "a file of {} bytes; times {}",
        file.bytes,
        microseconds(&times)
    );
    Ok(vec![
        Measured::new("app/enter_edit", Value::Time(Samples(times))).with_note(Some(note)),
    ])
}

/// `app/files_open`: 5 launches, one at a time. Each opens the large
/// repository (the 100 000-file shape of `app/idle_100k`) and lets it
/// settle, so the directory table is built, and sends `Open(Files)`: from
/// the request to the `FilesShown` milestone's frame, the panel with the
/// tree's first rows drawn.
fn app_files_open(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let program = app(ctx)?;
    let repo = reader_files::Wide::new(ctx)?;
    let scheduler = scheduler(ctx);
    let mut times = Vec::new();
    for _ in 0..ctx.scale(5, 1) {
        let mut session = launch_with_project(&scheduler, &program, repo.root(), "app/files_open")?;
        times.push(session.open(OpenTarget::Files, Milestone::FilesShown, OPEN_LIMIT)?);
        session.quit()?;
    }
    let note = format!("{} files; times {}", repo.files, microseconds(&times));
    Ok(vec![
        Measured::new("app/files_open", Value::Time(Samples(times))).with_note(Some(note)),
    ])
}

/// `app/files_expand`: 5 launches, one at a time. Each opens the large
/// repository, shows Files (untimed) and selects the first row, the
/// directory of 2 000 files, with the key ↓, lets it settle, and sends the
/// key →: from the request to the `FilesExpanded` milestone's frame, the
/// directory's rows inserted and drawn.
fn app_files_expand(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let program = app(ctx)?;
    let repo = reader_files::Wide::new(ctx)?;
    let scheduler = scheduler(ctx);
    let mut times = Vec::new();
    for _ in 0..ctx.scale(5, 1) {
        let mut session =
            launch_with_project(&scheduler, &program, repo.root(), "app/files_expand")?;
        session.open(OpenTarget::Files, Milestone::FilesShown, OPEN_LIMIT)?;
        settle(&mut session, "app/files_expand")?;
        session.request(OpenTarget::Key("down".to_owned()))?;
        settle(&mut session, "app/files_expand")?;
        times.push(session.open(
            OpenTarget::Key("right".to_owned()),
            Milestone::FilesExpanded,
            OPEN_LIMIT,
        )?);
        session.quit()?;
    }
    let note = format!(
        "{} files, a directory of {}; times {}",
        repo.files,
        repo.children,
        microseconds(&times)
    );
    Ok(vec![
        Measured::new("app/files_expand", Value::Time(Samples(times))).with_note(Some(note)),
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

/// The number `field` of the last record `msg` in the program's log under
/// `home` (`Library/Logs/Lectern/*.log`, one JSON object a line): the app's
/// own time for a milestone, next to the runner's. None when there is none.
fn logged(home: &Path, msg: &str, field: &str) -> Option<u64> {
    let dir = std::fs::read_dir(home.join("Library/Logs/Lectern")).ok()?;
    let mut found = None;
    for entry in dir.flatten() {
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        for line in text.lines() {
            let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if record["msg"] == msg {
                found = record[field].as_u64().or(found);
            }
        }
    }
    found
}

/// [`logged`] values for a note, "-" where the log had none.
fn logged_values(values: &[Option<u64>]) -> String {
    let each: Vec<String> = values
        .iter()
        .map(|v| v.map_or_else(|| "-".to_owned(), |us| format!("{us} µs")))
        .collect();
    each.join(", ")
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
