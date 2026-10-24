//! Runs one target's benchmarks: checks them against the budget table,
//! runs them in ID order, compares with the budgets, retries once
//! (docs/design/bench.md「进程内基准」).

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::time::Instant;

use crate::budgets::{Budget, State, Target, is_prefix, lookup};
use crate::ctx::{Ctx, Fixtures, SampleConfig};
use crate::report::{self, BenchTime, Entry, Host, Report, Status};
use crate::stats::evaluate;
use crate::{Bench, Measured, Mode, Value};

pub(crate) struct Options<'a> {
    pub target: Target,
    pub budgets: &'a [Budget],
    /// The arguments after the program name.
    pub args: Vec<String>,
    /// The gating platform (`aarch64-apple-darwin`).
    pub apple_silicon: bool,
    /// Process benchmarks run only on macOS.
    pub macos: bool,
    /// `CARGO_TARGET_TMPDIR/lectern-bench`.
    pub scratch_root: PathBuf,
    pub report_dir: PathBuf,
    /// `<target dir>/lectern-bench/fixtures`: kept between runs
    /// ([`Ctx::persistent`]).
    pub fixtures_dir: PathBuf,
    pub config: SampleConfig,
    pub host: Host,
    pub commit: Option<String>,
}

pub(crate) struct Run<'a> {
    pub exit: u8,
    /// None for `--list`.
    pub report: Option<Report<'a>>,
    /// What the target prints.
    pub text: String,
}

struct Args {
    mode: Mode,
    list: bool,
    filters: Vec<String>,
}

fn parse(args: &[String]) -> Args {
    let mut parsed = Args {
        mode: Mode::Smoke,
        list: false,
        filters: Vec::new(),
    };
    for arg in args {
        match arg.as_str() {
            "--bench" => parsed.mode = Mode::Measure,
            "--list" => parsed.list = true,
            // Other flags (libtest's, passed through by cargo) do not apply.
            flag if flag.starts_with('-') => {}
            filter => parsed.filters.push(filter.to_owned()),
        }
    }
    parsed
}

impl Args {
    fn selects(&self, id: &str) -> bool {
        self.filters.is_empty() || self.filters.iter().any(|f| id.contains(f.as_str()))
    }
}

pub(crate) fn run<'a>(options: &Options<'a>, benches: &[Bench]) -> Run<'a> {
    let args = parse(&options.args);
    let own: Vec<&'a Budget> = options
        .budgets
        .iter()
        .filter(|b| b.target == options.target)
        .collect();
    if args.list {
        return Run {
            exit: 0,
            report: None,
            text: list(&own),
        };
    }
    let gated = args.mode == Mode::Measure && options.apple_silicon;
    let mut report = Report {
        target: options.target,
        mode: args.mode,
        gated,
        host: options.host.clone(),
        commit: options.commit.clone(),
        entries: Vec::new(),
        errors: Vec::new(),
        plugins: Vec::new(),
        language_servers: Vec::new(),
        benches: Vec::new(),
    };

    // Entries that need no run: filtered, then planned.
    let mut pending: Vec<&'a Budget> = Vec::new();
    for budget in &own {
        if !args.selects(budget.id) {
            report
                .entries
                .push(Entry::new(budget, Status::Filtered, None));
        } else if let State::Planned(by) = budget.state {
            report.entries.push(Entry::new(
                budget,
                Status::Planned,
                Some(format!("由 {by} 加入")),
            ));
        } else {
            pending.push(budget);
        }
    }

    // Budgets whose benchmark only builds on macOS: skipped elsewhere.
    if !options.macos {
        let (skipped, rest): (Vec<&Budget>, Vec<&Budget>) =
            pending.into_iter().partition(|b| b.macos_only);
        for budget in skipped {
            report.entries.push(Entry::new(
                budget,
                Status::Skipped,
                Some("只在 macOS 运行".into()),
            ));
        }
        pending = rest;
    }

    let (missing, problems) = correspondence(&own, benches, options.macos);
    if !missing.is_empty() || !problems.is_empty() {
        for budget in pending {
            let entry = if missing.contains(budget.id) {
                Entry::new(budget, Status::Error, Some("没有基准产出这一项".into()))
            } else {
                Entry::new(
                    budget,
                    Status::Skipped,
                    Some("预算表与基准不对应，没有运行".into()),
                )
            };
            report.entries.push(entry);
        }
        for id in &missing {
            report.errors.push(format!("预算 {id} 没有对应的基准"));
        }
        report.errors.extend(problems);
        return finish(options, report);
    }

    if options.target == Target::Process && !options.macos {
        for budget in pending {
            report.entries.push(Entry::new(
                budget,
                Status::Skipped,
                Some("只在 macOS 运行".into()),
            ));
        }
        return finish(options, report);
    }

    let mut order: Vec<&Bench> = benches.iter().collect();
    order.sort_by_key(|b| b.id);
    // Shared fixtures live until every benchmark of the target has run.
    let fixtures = Fixtures::default();
    for bench in order {
        let wanted: Vec<&'a Budget> = pending
            .iter()
            .copied()
            .filter(|b| is_prefix(bench.id, b.id))
            .collect();
        if wanted.is_empty() {
            continue;
        }
        let started = Instant::now();
        let attempts = run_bench(
            options,
            &args,
            gated,
            bench,
            &wanted,
            &fixtures,
            &mut report,
        );
        report.benches.push(BenchTime {
            id: bench.id.to_owned(),
            seconds: started.elapsed().as_secs_f64(),
            attempts,
        });
    }
    drop(fixtures);
    // Keep the table's order.
    let position = |id: &str| {
        own.iter()
            .position(|b| b.id == id)
            .or_else(|| {
                lookup(options.budgets, id).and_then(|b| own.iter().position(|o| o.id == b.id))
            })
            .unwrap_or(usize::MAX)
    };
    report.entries.sort_by_key(|e| position(&e.id));
    finish(options, report)
}

/// Active budgets of this target without a benchmark (off macOS, the
/// macOS-only ones are not expected to have one), and benchmarks that match
/// no budget of this target (or only planned ones).
fn correspondence(
    own: &[&Budget],
    benches: &[Bench],
    macos: bool,
) -> (BTreeSet<&'static str>, Vec<String>) {
    let missing = own
        .iter()
        .filter(|b| b.state == State::Active && (macos || !b.macos_only))
        .filter(|b| !benches.iter().any(|bench| is_prefix(bench.id, b.id)))
        .map(|b| b.id)
        .collect();
    let mut problems = Vec::new();
    let mut seen = BTreeSet::new();
    for bench in benches {
        if !seen.insert(bench.id) {
            problems.push(format!("基准 {} 重复", bench.id));
        }
        let matching: Vec<&&Budget> = own.iter().filter(|b| is_prefix(bench.id, b.id)).collect();
        if matching.is_empty() {
            problems.push(format!("基准 {} 不对应本目标的任何预算", bench.id));
        } else if matching.iter().all(|b| b.state != State::Active) {
            problems.push(format!(
                "基准 {} 对应的预算还是 Planned，先在 budgets.rs 里改为 Active",
                bench.id
            ));
        }
    }
    (missing, problems)
}

/// Runs `bench` once (twice when gated and something is over budget) and
/// adds an entry for each budget it should produce. Returns how many
/// attempts ran.
fn run_bench<'a>(
    options: &Options<'a>,
    args: &Args,
    gated: bool,
    bench: &Bench,
    wanted: &[&'a Budget],
    fixtures: &Fixtures,
    report: &mut Report<'a>,
) -> u32 {
    let first = match attempt(options, args.mode, bench, fixtures) {
        Ok(results) => results,
        Err(message) => {
            for budget in wanted {
                report
                    .entries
                    .push(Entry::new(budget, Status::Error, Some(message.clone())));
            }
            return 1;
        }
    };
    let mut entries = collect(options, args, bench, wanted, first, report);
    for entry in &mut entries {
        judge(entry, args.mode);
    }
    let retry = gated && entries.iter().any(|e| e.status == Status::Over);
    if retry {
        match attempt(options, args.mode, bench, fixtures) {
            Ok(results) => {
                let mut again: BTreeMap<String, (Value, Option<String>)> = results
                    .into_iter()
                    .map(|m| (m.id, (m.value, m.note)))
                    .collect();
                for entry in entries.iter_mut().filter(|e| e.status == Status::Over) {
                    match again.remove(&entry.id) {
                        Some((value, note)) => {
                            entry.attempts.push(value);
                            entry.note = note;
                            judge(entry, args.mode);
                        }
                        None => {
                            entry.status = Status::Error;
                            entry.reason = Some("重跑时基准没有产出这一项".into());
                        }
                    }
                }
            }
            Err(message) => {
                for entry in entries.iter_mut().filter(|e| e.status == Status::Over) {
                    entry.status = Status::Error;
                    entry.reason = Some(format!("重跑出错：{message}"));
                }
            }
        }
    }
    report.entries.extend(entries);
    1 + u32::from(retry)
}

/// Matches one attempt's results with the budgets. Unknown, duplicate or
/// misplaced results become report errors; a wanted budget without a result
/// becomes an `error` entry.
fn collect<'a>(
    options: &Options<'a>,
    args: &Args,
    bench: &Bench,
    wanted: &[&'a Budget],
    results: Vec<Measured>,
    report: &mut Report<'a>,
) -> Vec<Entry<'a>> {
    let mut entries: Vec<Entry<'a>> = Vec::new();
    let mut seen = BTreeSet::new();
    for measured in results {
        if !seen.insert(measured.id.clone()) {
            report
                .errors
                .push(format!("基准 {} 两次产出 {}", bench.id, measured.id));
            continue;
        }
        let Some(budget) = lookup(options.budgets, &measured.id) else {
            report.errors.push(format!(
                "基准 {} 产出的 {} 不在预算表里",
                bench.id, measured.id
            ));
            continue;
        };
        if budget.target != options.target {
            report.errors.push(format!(
                "{} 属于 {} 目标，不属于 {}",
                measured.id,
                budget.target.name(),
                options.target.name()
            ));
            continue;
        }
        if !args.selects(budget.id) {
            continue;
        }
        if budget.state != State::Active {
            report.errors.push(format!(
                "{} 的预算还是 Planned，先在 budgets.rs 里改为 Active",
                measured.id
            ));
            continue;
        }
        if !wanted.iter().any(|w| std::ptr::eq(*w, budget)) {
            report
                .errors
                .push(format!("{} 不在基准 {} 的前缀之下", measured.id, bench.id));
            continue;
        }
        entries.push(Entry {
            id: measured.id,
            budget,
            status: Status::Error,
            reason: None,
            note: measured.note,
            attempts: vec![measured.value],
        });
    }
    for budget in wanted {
        if entries.iter().any(|e| std::ptr::eq(e.budget, *budget)) {
            continue;
        }
        let reason = if budget.is_wildcard() {
            "没有可展开的项"
        } else {
            "基准没有产出这一项"
        };
        let status = if budget.is_wildcard() {
            Status::Skipped
        } else {
            Status::Error
        };
        entries.push(Entry::new(budget, status, Some(reason.into())));
    }
    entries
}

/// Sets the status from the last attempt.
fn judge(entry: &mut Entry<'_>, mode: Mode) {
    let Some(value) = entry.attempts.last() else {
        return;
    };
    let mut pass = true;
    for check in entry.budget.checks {
        match evaluate(check, value) {
            Ok(outcome) => pass &= outcome.pass,
            Err(message) => {
                entry.status = Status::Error;
                entry.reason = Some(message);
                return;
            }
        }
    }
    entry.reason = None;
    entry.status = if mode == Mode::Smoke {
        Status::Smoke
    } else if entry.budget.checks.is_empty() {
        Status::Record
    } else if pass {
        Status::Ok
    } else {
        Status::Over
    };
}

/// One run of `bench` in a fresh scratch directory (shared fixtures are
/// reused). A returned error or a panic is `Err`.
fn attempt(
    options: &Options<'_>,
    mode: Mode,
    bench: &Bench,
    fixtures: &Fixtures,
) -> Result<Vec<Measured>, String> {
    let scratch = options.scratch_root.join(bench.id);
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch)
        .map_err(|e| format!("建不了临时目录 {}：{e}", scratch.display()))?;
    let mut ctx = Ctx::new(
        mode,
        options.config,
        scratch.clone(),
        fixtures.clone(),
        options.fixtures_dir.clone(),
    );
    let outcome = catch_unwind(AssertUnwindSafe(|| (bench.run)(&mut ctx)));
    let _ = std::fs::remove_dir_all(&scratch);
    match outcome {
        Ok(Ok(results)) => Ok(results),
        Ok(Err(error)) => Err(error.message().to_owned()),
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "panic".to_owned());
            Err(format!("panic：{message}"))
        }
    }
}

fn finish<'a>(options: &Options<'a>, report: Report<'a>) -> Run<'a> {
    let exit = exit_code(&report);
    let json = report.to_json();
    let results = json["results"].as_array().cloned().unwrap_or_default();
    let text = report::text_table(
        &report::title(options.target.name(), report.mode.name(), report.gated),
        &results,
        &report.errors,
    );
    Run {
        exit,
        report: Some(report),
        text,
    }
}

pub(crate) fn exit_code(report: &Report<'_>) -> u8 {
    if !report.errors.is_empty() || report.entries.iter().any(|e| e.status == Status::Error) {
        2
    } else if report.gated && report.entries.iter().any(|e| e.status == Status::Over) {
        1
    } else {
        0
    }
}

fn list(own: &[&Budget]) -> String {
    let mut out = String::new();
    for budget in own {
        let state = match budget.state {
            State::Active => "active".to_owned(),
            State::Planned(by) => format!("planned ({by})"),
        };
        out.push_str(&format!(
            "{}  {}  {}  {}\n",
            budget.id,
            budget.describe(),
            state,
            budget.source
        ));
    }
    out
}
