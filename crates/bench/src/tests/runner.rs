use std::hint::black_box;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::*;
use crate::report::Status;
use crate::runner::{NOT_THE_JUDGE, Run, judge_from_env, run};
use crate::{Bench, BenchError, Ctx, Measured, Mode, Samples, Value};

fn time(ms: u64) -> Value {
    Value::Time(Samples(vec![Duration::from_millis(ms); 5]))
}

fn status<'a>(run: &'a Run<'_>, id: &str) -> &'a crate::report::Entry<'a> {
    run.report
        .as_ref()
        .unwrap()
        .entries
        .iter()
        .find(|e| e.id == id)
        .unwrap_or_else(|| panic!("no entry for {id}"))
}

static ONE: &[Budget] = &[budget("fake/a", UNDER_1_MS, State::Active, Target::Time)];

#[test]
fn runner_retries_once_and_a_passing_retry_is_ok() {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    fn flaky(_: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
        // Over budget the first time, within it the second.
        let ms = if CALLS.fetch_add(1, Ordering::SeqCst) == 0 {
            2
        } else {
            0
        };
        Ok(vec![Measured::new("fake/a", time(ms))])
    }
    let dir = tempfile::tempdir().unwrap();
    let benches = [Bench {
        id: "fake/a",
        run: flaky,
    }];
    let run = run(
        &options(ONE, Target::Time, &["--bench"], dir.path()),
        &benches,
    );
    assert_eq!(run.exit, 0, "{}", run.text);
    let entry = status(&run, "fake/a");
    assert_eq!(entry.status, Status::Ok);
    assert_eq!(entry.attempts.len(), 2, "both attempts are in the report");
    assert_eq!(CALLS.load(Ordering::SeqCst), 2);
    let json = run.report.as_ref().unwrap().to_json();
    assert_eq!(json["results"][0]["attempts"].as_array().unwrap().len(), 2);
}

/// A result's note goes into the report beside its value; a retry's note
/// replaces the first attempt's.
#[test]
fn runner_notes_go_into_the_report() {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    fn noted(_: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
        let call = CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(vec![
            Measured::new("fake/a", time(2)).with_note(Some(format!("attempt {call}"))),
        ])
    }
    let dir = tempfile::tempdir().unwrap();
    let benches = [Bench {
        id: "fake/a",
        run: noted,
    }];
    let run = run(
        &options(ONE, Target::Time, &["--bench"], dir.path()),
        &benches,
    );
    let entry = status(&run, "fake/a");
    assert_eq!(entry.attempts.len(), 2, "over budget: retried");
    assert_eq!(entry.note.as_deref(), Some("attempt 1"));
    let json = run.report.as_ref().unwrap().to_json();
    assert_eq!(json["results"][0]["note"], "attempt 1");
}

#[test]
fn runner_over_budget_twice_exits_1() {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    fn slow(_: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
        CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(vec![Measured::new("fake/a", time(2))])
    }
    let dir = tempfile::tempdir().unwrap();
    let benches = [Bench {
        id: "fake/a",
        run: slow,
    }];
    let run = run(
        &options(ONE, Target::Time, &["--bench"], dir.path()),
        &benches,
    );
    assert_eq!(run.exit, 1, "{}", run.text);
    let entry = status(&run, "fake/a");
    assert_eq!(entry.status, Status::Over);
    assert_eq!(entry.attempts.len(), 2);
    assert_eq!(CALLS.load(Ordering::SeqCst), 2);
}

#[test]
fn runner_smoke_mode_and_other_platforms_never_fail_on_budgets() {
    fn slow(_: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
        Ok(vec![Measured::new("fake/a", time(2))])
    }
    let dir = tempfile::tempdir().unwrap();
    let benches = [Bench {
        id: "fake/a",
        run: slow,
    }];
    // Smoke mode on the gating platform.
    let smoke = run(&options(ONE, Target::Time, &[], dir.path()), &benches);
    assert_eq!(smoke.exit, 0);
    assert_eq!(status(&smoke, "fake/a").status, Status::Smoke);
    // Measure mode elsewhere: reported over budget, not judged, no retry.
    let mut elsewhere = options(ONE, Target::Time, &["--bench"], dir.path());
    elsewhere.apple_silicon = false;
    let measured = run(&elsewhere, &benches);
    assert_eq!(measured.exit, 0);
    let entry = status(&measured, "fake/a");
    assert_eq!(entry.status, Status::Over);
    assert_eq!(entry.attempts.len(), 1);
    assert!(!measured.report.as_ref().unwrap().gated);
}

#[test]
fn runner_errors_and_panics_exit_2_in_every_mode() {
    fn fails(_: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
        Err(BenchError::new("the fixture could not be built"))
    }
    fn panics(_: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
        panic!("boom")
    }
    static TWO: &[Budget] = &[
        budget("fake/a", UNDER_1_MS, State::Active, Target::Time),
        budget("fake/b", UNDER_1_MS, State::Active, Target::Time),
    ];
    let dir = tempfile::tempdir().unwrap();
    let benches = [
        Bench {
            id: "fake/a",
            run: fails,
        },
        Bench {
            id: "fake/b",
            run: panics,
        },
    ];
    for args in [&[][..], &["--bench"][..]] {
        let run = run(&options(TWO, Target::Time, args, dir.path()), &benches);
        assert_eq!(run.exit, 2, "{args:?}");
        let a = status(&run, "fake/a");
        assert_eq!(a.status, Status::Error);
        assert_eq!(a.reason.as_deref(), Some("the fixture could not be built"));
        let b = status(&run, "fake/b");
        assert_eq!(b.status, Status::Error);
        assert!(b.reason.as_deref().unwrap().contains("boom"));
    }
}

#[test]
fn runner_skipping_benchmark_is_skipped_not_an_error() {
    fn skips(_: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
        Err(BenchError::skip("没有设置 LECTERN_BENCH_BASELINE"))
    }
    let dir = tempfile::tempdir().unwrap();
    let benches = [Bench {
        id: "fake/a",
        run: skips,
    }];
    for args in [&[][..], &["--bench"][..]] {
        let run = run(&options(ONE, Target::Time, args, dir.path()), &benches);
        assert_eq!(run.exit, 0, "{args:?}");
        let a = status(&run, "fake/a");
        assert_eq!(a.status, Status::Skipped);
        assert_eq!(a.reason.as_deref(), Some("没有设置 LECTERN_BENCH_BASELINE"));
    }
}

#[test]
fn runner_missing_budget_benchmark_stops_before_running() {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    fn counted(_: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
        CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(vec![Measured::new("fake/a", time(0))])
    }
    static TABLE: &[Budget] = &[
        budget("fake/a", UNDER_1_MS, State::Active, Target::Time),
        budget("fake/b", UNDER_1_MS, State::Active, Target::Time),
        // Another target's budget is not this target's concern.
        budget("other/m", UNDER_1_MS, State::Active, Target::Memory),
    ];
    let dir = tempfile::tempdir().unwrap();
    let benches = [Bench {
        id: "fake/a",
        run: counted,
    }];
    // Smoke mode checks the correspondence too.
    let run = run(&options(TABLE, Target::Time, &[], dir.path()), &benches);
    assert_eq!(run.exit, 2);
    assert_eq!(CALLS.load(Ordering::SeqCst), 0, "nothing ran");
    assert_eq!(status(&run, "fake/b").status, Status::Error);
    assert_eq!(status(&run, "fake/a").status, Status::Skipped);
    let report = run.report.as_ref().unwrap();
    assert!(report.entries.iter().all(|e| e.id != "other/m"));
    assert!(
        report.errors.iter().any(|e| e.contains("fake/b")),
        "{:?}",
        report.errors
    );
    assert!(run.text.contains("fake/b"));
}

#[test]
fn runner_extra_benchmark_stops_before_running() {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    fn counted(_: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
        CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(vec![Measured::new("fake/a", time(0))])
    }
    static TABLE: &[Budget] = &[
        budget("fake/a", UNDER_1_MS, State::Active, Target::Time),
        budget(
            "fake/planned",
            UNDER_1_MS,
            State::Planned("test"),
            Target::Time,
        ),
    ];
    let dir = tempfile::tempdir().unwrap();
    for extra in ["fake/unknown", "fake/planned"] {
        let benches = [
            Bench {
                id: "fake/a",
                run: counted,
            },
            Bench {
                id: extra,
                run: counted,
            },
        ];
        let run = run(
            &options(TABLE, Target::Time, &["--bench"], dir.path()),
            &benches,
        );
        assert_eq!(run.exit, 2, "{extra}");
        let errors = &run.report.as_ref().unwrap().errors;
        assert!(errors.iter().any(|e| e.contains(extra)), "{errors:?}");
    }
    assert_eq!(CALLS.load(Ordering::SeqCst), 0);
}

#[test]
fn runner_results_outside_the_table_are_errors() {
    fn strays(_: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
        Ok(vec![
            Measured::new("fake/a", time(0)),
            Measured::new("fake/a/unknown", time(0)),
            Measured::new("other/m", time(0)),
        ])
    }
    static TABLE: &[Budget] = &[
        budget("fake/a", UNDER_1_MS, State::Active, Target::Time),
        budget("other/m", UNDER_1_MS, State::Active, Target::Memory),
    ];
    let dir = tempfile::tempdir().unwrap();
    let benches = [Bench {
        id: "fake/a",
        run: strays,
    }];
    let run = run(
        &options(TABLE, Target::Time, &["--bench"], dir.path()),
        &benches,
    );
    assert_eq!(run.exit, 2);
    assert_eq!(status(&run, "fake/a").status, Status::Ok);
    let errors = &run.report.as_ref().unwrap().errors;
    assert!(
        errors.iter().any(|e| e.contains("fake/a/unknown")),
        "{errors:?}"
    );
    assert!(errors.iter().any(|e| e.contains("other/m")), "{errors:?}");
}

#[test]
fn runner_bench_flag_selects_the_mode() {
    static MODES: Mutex<Vec<Mode>> = Mutex::new(Vec::new());
    fn record(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
        MODES.lock().unwrap().push(ctx.mode());
        Ok(vec![Measured::new("fake/a", time(0))])
    }
    let dir = tempfile::tempdir().unwrap();
    let benches = [Bench {
        id: "fake/a",
        run: record,
    }];
    let smoke = run(&options(ONE, Target::Time, &[], dir.path()), &benches);
    let measure = run(
        &options(ONE, Target::Time, &["--bench"], dir.path()),
        &benches,
    );
    assert_eq!(*MODES.lock().unwrap(), [Mode::Smoke, Mode::Measure]);
    assert_eq!(smoke.report.unwrap().mode, Mode::Smoke);
    assert_eq!(measure.report.unwrap().mode, Mode::Measure);
}

#[test]
fn runner_filters_by_id_substring() {
    static A: AtomicUsize = AtomicUsize::new(0);
    static B: AtomicUsize = AtomicUsize::new(0);
    fn a(_: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
        A.fetch_add(1, Ordering::SeqCst);
        Ok(vec![Measured::new("fake/a", time(0))])
    }
    fn b(_: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
        B.fetch_add(1, Ordering::SeqCst);
        Ok(vec![Measured::new("other/b", time(0))])
    }
    static TABLE: &[Budget] = &[
        budget("fake/a", UNDER_1_MS, State::Active, Target::Time),
        budget("other/b", UNDER_1_MS, State::Active, Target::Time),
        budget(
            "other/planned",
            UNDER_1_MS,
            State::Planned("test"),
            Target::Time,
        ),
        budget(
            "fake/planned",
            UNDER_1_MS,
            State::Planned("test"),
            Target::Time,
        ),
    ];
    let dir = tempfile::tempdir().unwrap();
    let benches = [
        Bench {
            id: "fake/a",
            run: a,
        },
        Bench {
            id: "other/b",
            run: b,
        },
    ];
    let run = run(
        &options(TABLE, Target::Time, &["fake/"], dir.path()),
        &benches,
    );
    assert_eq!(run.exit, 0, "{}", run.text);
    assert_eq!((A.load(Ordering::SeqCst), B.load(Ordering::SeqCst)), (1, 0));
    assert_eq!(status(&run, "fake/a").status, Status::Smoke);
    assert_eq!(status(&run, "other/b").status, Status::Filtered);
    assert_eq!(status(&run, "other/planned").status, Status::Filtered);
    assert_eq!(status(&run, "fake/planned").status, Status::Planned);
}

#[test]
fn runner_list_prints_the_budgets_without_running() {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    fn counted(_: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
        CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(vec![Measured::new("fake/a", time(0))])
    }
    let dir = tempfile::tempdir().unwrap();
    let benches = [Bench {
        id: "fake/a",
        run: counted,
    }];
    let run = run(
        &options(ONE, Target::Time, &["--list"], dir.path()),
        &benches,
    );
    assert_eq!(run.exit, 0);
    assert!(run.report.is_none());
    assert_eq!(
        run.text,
        "fake/a  median < 1 ms  active  docs/design/bench.md#测试\n"
    );
    assert_eq!(CALLS.load(Ordering::SeqCst), 0);
}

#[test]
fn runner_time_of_constant_work_is_not_zero() {
    static MEDIAN: Mutex<Option<Duration>> = Mutex::new(None);
    fn work(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
        // A computation on a constant input that returns its result: the
        // runner's black_box keeps it from being optimised away.
        let samples = ctx.time(|| {
            (0..black_box(64u64)).fold(0u64, |acc, x| acc.wrapping_mul(31).wrapping_add(x))
        });
        let mut sorted = samples.0.clone();
        sorted.sort();
        *MEDIAN.lock().unwrap() = Some(sorted[sorted.len() / 2]);
        Ok(vec![Measured::new("fake/a", Value::Time(samples))])
    }
    let dir = tempfile::tempdir().unwrap();
    let benches = [Bench {
        id: "fake/a",
        run: work,
    }];
    let mut options = options(ONE, Target::Time, &["--bench"], dir.path());
    options.apple_silicon = false;
    run(&options, &benches);
    assert!(MEDIAN.lock().unwrap().unwrap() > Duration::ZERO);
}

#[test]
fn runner_time_with_setup_calls_setup_once_per_sample() {
    static COUNTS: Mutex<(usize, usize, usize)> = Mutex::new((0, 0, 0));
    fn stateful(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
        let mut prepared = 0;
        let mut runs = 0;
        let mut fresh = true;
        let samples = ctx.time_with_setup(
            || {
                prepared += 1;
                prepared
            },
            |input| {
                runs += 1;
                // Each call meets the input prepared right before it.
                fresh &= input == runs;
                input
            },
        );
        assert!(fresh);
        *COUNTS.lock().unwrap() = (prepared, runs, samples.0.len());
        Ok(vec![Measured::new("fake/a", Value::Time(samples))])
    }
    let dir = tempfile::tempdir().unwrap();
    let benches = [Bench {
        id: "fake/a",
        run: stateful,
    }];
    let mut options = options(ONE, Target::Time, &["--bench"], dir.path());
    options.apple_silicon = false;
    run(&options, &benches);
    let (prepared, runs, samples) = *COUNTS.lock().unwrap();
    assert_eq!(prepared, runs);
    // Warm-up calls plus one call per sample.
    assert!(
        samples >= 5 && runs >= samples + 3,
        "{prepared} {runs} {samples}"
    );
}

#[test]
fn runner_scratch_is_fresh_and_removed() {
    static SEEN: Mutex<Vec<(std::path::PathBuf, bool)>> = Mutex::new(Vec::new());
    fn scratch(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
        let dir = ctx.scratch().to_owned();
        let empty = std::fs::read_dir(&dir)?.next().is_none();
        std::fs::write(dir.join("fixture"), b"x")?;
        SEEN.lock().unwrap().push((dir, empty));
        Ok(vec![Measured::new("fake/a", time(2))])
    }
    let dir = tempfile::tempdir().unwrap();
    let benches = [Bench {
        id: "fake/a",
        run: scratch,
    }];
    run(
        &options(ONE, Target::Time, &["--bench"], dir.path()),
        &benches,
    );
    let seen = SEEN.lock().unwrap();
    assert_eq!(seen.len(), 2, "over budget, so retried");
    for (path, empty) in seen.iter() {
        assert!(*empty, "a retry gets a new fixture directory");
        assert!(path.ends_with("scratch/fake/a"), "{}", path.display());
        assert!(!path.exists());
    }
}

#[test]
fn runner_process_target_is_skipped_off_macos() {
    static TABLE: &[Budget] = &[
        budget("app/x", UNDER_1_MS, State::Active, Target::Process),
        budget(
            "app/later",
            UNDER_1_MS,
            State::Planned("test"),
            Target::Process,
        ),
    ];
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    fn counted(_: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
        CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(vec![Measured::new("app/x", time(0))])
    }
    let dir = tempfile::tempdir().unwrap();
    let benches = [Bench {
        id: "app",
        run: counted,
    }];
    let mut options = options(TABLE, Target::Process, &[], dir.path());
    options.macos = false;
    let run = run(&options, &benches);
    assert_eq!(run.exit, 0);
    assert_eq!(CALLS.load(Ordering::SeqCst), 0);
    let x = status(&run, "app/x");
    assert_eq!(x.status, Status::Skipped);
    assert_eq!(x.reason.as_deref(), Some("只在 macOS 运行"));
    assert_eq!(status(&run, "app/later").status, Status::Planned);
}

#[test]
fn runner_macos_only_budgets_are_skipped_off_macos() {
    static TABLE: &[Budget] = &[
        budget("fake/a", UNDER_1_MS, State::Active, Target::Time),
        Budget {
            macos_only: true,
            ..budget("fake/gpui", UNDER_1_MS, State::Active, Target::Time)
        },
    ];
    fn a(_: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
        Ok(vec![Measured::new("fake/a", time(0))])
    }
    // Off macOS the benchmark is not even compiled: only `fake/a` exists.
    let benches = [Bench {
        id: "fake/a",
        run: a,
    }];
    let dir = tempfile::tempdir().unwrap();
    let mut options = options(TABLE, Target::Time, &[], dir.path());
    options.macos = false;
    let linux = run(&options, &benches);
    assert_eq!(
        linux.exit,
        0,
        "{:?}",
        linux.report.as_ref().map(|r| &r.errors)
    );
    assert_eq!(status(&linux, "fake/a").status, Status::Smoke);
    let gpui = status(&linux, "fake/gpui");
    assert_eq!(gpui.status, Status::Skipped);
    assert_eq!(gpui.reason.as_deref(), Some("只在 macOS 运行"));

    // On macOS it is missing like any other active budget.
    options.macos = true;
    let macos = run(&options, &benches);
    assert_eq!(macos.exit, 2);
    assert_eq!(status(&macos, "fake/gpui").status, Status::Error);
}

static TWO: &[Budget] = &[
    budget("fake/a", UNDER_1_MS, State::Active, Target::Time),
    budget("fake/b", UNDER_1_MS, State::Active, Target::Time),
];

#[test]
fn runner_shares_fixtures_across_benches_and_retries() {
    static BUILDS: AtomicUsize = AtomicUsize::new(0);
    static DROPS: AtomicUsize = AtomicUsize::new(0);
    struct Fixture(u64);
    impl Drop for Fixture {
        fn drop(&mut self) {
            DROPS.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn fixture(ctx: &Ctx) -> Result<std::rc::Rc<Fixture>, BenchError> {
        ctx.shared("fake::fixture", || {
            BUILDS.fetch_add(1, Ordering::SeqCst);
            Ok(Fixture(2))
        })
    }
    fn over(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
        // Over budget both times: retried once with the same fixture.
        Ok(vec![Measured::new("fake/a", time(fixture(ctx)?.0))])
    }
    fn within(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
        assert_eq!(fixture(ctx)?.0, 2);
        assert!(
            ctx.shared("fake::fixture", || Ok(1u8)).is_err(),
            "another type"
        );
        Ok(vec![Measured::new("fake/b", time(0))])
    }
    let dir = tempfile::tempdir().unwrap();
    let benches = [
        Bench {
            id: "fake/a",
            run: over,
        },
        Bench {
            id: "fake/b",
            run: within,
        },
    ];
    let run = run(
        &options(TWO, Target::Time, &["--bench"], dir.path()),
        &benches,
    );
    assert_eq!(run.exit, 1, "{}", run.text);
    assert_eq!(BUILDS.load(Ordering::SeqCst), 1, "built once");
    assert_eq!(DROPS.load(Ordering::SeqCst), 1, "dropped when the run ends");
    let report = run.report.as_ref().unwrap();
    let times: Vec<(&str, u32)> = report
        .benches
        .iter()
        .map(|b| (b.id.as_str(), b.attempts))
        .collect();
    assert_eq!(times, [("fake/a", 2), ("fake/b", 1)]);
    assert!(report.benches.iter().all(|b| b.seconds >= 0.0));
    let json = report.to_json();
    assert_eq!(json["benches"][0]["id"], "fake/a");
    assert_eq!(json["benches"][0]["attempts"], 2);
}

// --- the judging machine (ADR 0052) ----------------------------------------

/// An entry for the judging machine, over its budget at 2 ms.
static FRAME: &[Budget] = &[Budget {
    judging_machine: true,
    ..budget("fake/frame", UNDER_1_MS, State::Active, Target::Time)
}];

#[test]
fn runner_judging_machine_entries_are_recorded_elsewhere() {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    fn over(_: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
        CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(vec![Measured::new("fake/frame", time(2))])
    }
    let dir = tempfile::tempdir().unwrap();
    let benches = [Bench {
        id: "fake/frame",
        run: over,
    }];
    let measured = run(
        &options(FRAME, Target::Time, &["--bench"], dir.path()),
        &benches,
    );
    assert_eq!(measured.exit, 0, "{}", measured.text);
    let entry = status(&measured, "fake/frame");
    assert_eq!(entry.status, Status::Record);
    assert_eq!(entry.reason.as_deref(), Some(NOT_THE_JUDGE));
    assert_eq!(entry.attempts.len(), 1, "a record is not retried");
    assert_eq!(CALLS.load(Ordering::SeqCst), 1);
    let json = measured.report.as_ref().unwrap().to_json();
    assert_eq!(json["judge"], false);
    assert_eq!(json["results"][0]["reason"], NOT_THE_JUDGE);

    // Smoke mode stays smoke, without the reason.
    let smoke = run(&options(FRAME, Target::Time, &[], dir.path()), &benches);
    let entry = status(&smoke, "fake/frame");
    assert_eq!(entry.status, Status::Smoke);
    assert_eq!(entry.reason, None);
}

#[test]
fn runner_judging_machine_entries_still_fail_on_a_value_they_cannot_judge() {
    fn bytes(_: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
        Ok(vec![Measured::new("fake/frame", Value::Bytes(vec![1; 5]))])
    }
    let dir = tempfile::tempdir().unwrap();
    let benches = [Bench {
        id: "fake/frame",
        run: bytes,
    }];
    let run = run(
        &options(FRAME, Target::Time, &["--bench"], dir.path()),
        &benches,
    );
    assert_eq!(run.exit, 2, "{}", run.text);
    assert_eq!(status(&run, "fake/frame").status, Status::Error);
}

#[test]
fn runner_judging_machine_entries_are_judged_with_lectern_bench_judge() {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    fn over(_: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
        CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(vec![Measured::new("fake/frame", time(2))])
    }
    let dir = tempfile::tempdir().unwrap();
    let benches = [Bench {
        id: "fake/frame",
        run: over,
    }];
    let mut options = options(FRAME, Target::Time, &["--bench"], dir.path());
    options.judge = true;
    let run = run(&options, &benches);
    assert_eq!(run.exit, 1, "{}", run.text);
    let entry = status(&run, "fake/frame");
    assert_eq!(entry.status, Status::Over);
    assert_eq!(entry.reason, None);
    assert_eq!(entry.attempts.len(), 2, "over is retried once");
    assert_eq!(CALLS.load(Ordering::SeqCst), 2);
    assert!(
        run.text.contains("measure, gated, judging machine"),
        "{}",
        run.text
    );
    assert_eq!(run.report.as_ref().unwrap().to_json()["judge"], true);
}

/// `LECTERN_BENCH_JUDGE=1` on a run that judges nothing (smoke mode, or not
/// Apple silicon) is an error, so the judging job cannot pass without
/// judging.
#[test]
fn runner_judge_needs_a_gated_run() {
    fn within(_: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
        Ok(vec![Measured::new("fake/frame", time(0))])
    }
    let dir = tempfile::tempdir().unwrap();
    let benches = [Bench {
        id: "fake/frame",
        run: within,
    }];
    let mut smoke = options(FRAME, Target::Time, &[], dir.path());
    smoke.judge = true;
    let mut elsewhere = options(FRAME, Target::Time, &["--bench"], dir.path());
    elsewhere.judge = true;
    elsewhere.apple_silicon = false;
    for options in [smoke, elsewhere] {
        let run = run(&options, &benches);
        assert_eq!(run.exit, 2, "{}", run.text);
        let errors = &run.report.as_ref().unwrap().errors;
        assert!(
            errors.iter().any(|e| e.contains("LECTERN_BENCH_JUDGE=1")),
            "{errors:?}"
        );
    }
}

#[test]
fn runner_judge_env_is_1_or_unset() {
    use std::ffi::OsStr;
    assert_eq!(judge_from_env(None), Ok(false));
    assert_eq!(judge_from_env(Some(OsStr::new(""))), Ok(false));
    assert_eq!(judge_from_env(Some(OsStr::new("1"))), Ok(true));
    for value in ["0", "true", "yes", " 1", "1 "] {
        let error = judge_from_env(Some(OsStr::new(value))).unwrap_err();
        assert!(error.contains("LECTERN_BENCH_JUDGE"), "{error}");
    }
}

#[test]
fn runner_ctx_knows_the_judging_machine() {
    static SEEN: Mutex<Vec<bool>> = Mutex::new(Vec::new());
    fn look(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
        SEEN.lock().unwrap().push(ctx.judge());
        Ok(vec![Measured::new("fake/frame", time(0))])
    }
    let dir = tempfile::tempdir().unwrap();
    let benches = [Bench {
        id: "fake/frame",
        run: look,
    }];
    let mut options = options(FRAME, Target::Time, &["--bench"], dir.path());
    run(&options, &benches);
    options.judge = true;
    run(&options, &benches);
    assert_eq!(*SEEN.lock().unwrap(), [false, true]);
}
