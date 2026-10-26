use std::time::Duration;

use super::*;
use crate::report::{self, Entry, Report, Status};
use crate::runner::run;
use crate::{Bench, Mode, Samples, Value};

fn us(values: &[u64]) -> Samples {
    Samples(values.iter().map(|v| Duration::from_micros(*v)).collect())
}

static TABLE: &[Budget] = &[
    budget("fake/time", UNDER_1_MS, State::Active, Target::Time),
    budget(
        "fake/throughput",
        &[Check {
            stat: Stat::Median,
            limit: Limit::ThroughputAtLeast(200 << 20),
        }],
        State::Active,
        Target::Time,
    ),
    budget(
        "fake/ratio",
        &[
            Check {
                stat: Stat::Median,
                limit: Limit::RatioAtMost(3.0),
            },
            Check {
                stat: Stat::Median,
                limit: Limit::TimeBelow(Duration::from_micros(10)),
            },
        ],
        State::Active,
        Target::Time,
    ),
    budget(
        "fake/bytes",
        &[Check {
            stat: Stat::Median,
            limit: Limit::BytesBelow(60 << 20),
        }],
        State::Active,
        Target::Time,
    ),
    budget(
        "fake/frames",
        &[
            Check {
                stat: Stat::Max,
                limit: Limit::TimeAtMost(Duration::from_micros(8300)),
            },
            Check {
                stat: Stat::Max,
                limit: Limit::RefreshMultipleAtMost(1.5),
            },
        ],
        State::Active,
        Target::Time,
    ),
    budget("fake/record", &[], State::Active, Target::Time),
    budget("fake/noisy", UNDER_1_MS, State::Active, Target::Time),
    budget("fake/error", UNDER_1_MS, State::Active, Target::Time),
    budget("fake/skipped", UNDER_1_MS, State::Active, Target::Time),
    budget("fake/filtered", UNDER_1_MS, State::Active, Target::Time),
    budget(
        "fake/planned",
        UNDER_1_MS,
        State::Planned("fake 第 2 步"),
        Target::Time,
    ),
    budget("fake/smoke", UNDER_1_MS, State::Active, Target::Time),
];

fn entry(
    index: usize,
    status: Status,
    reason: Option<&str>,
    attempts: Vec<Value>,
) -> Entry<'static> {
    Entry {
        attempts,
        ..Entry::new(&TABLE[index], status, reason.map(str::to_owned))
    }
}

/// One entry of each kind: every status and every kind of value.
fn fake_report() -> Report<'static> {
    Report {
        target: Target::Time,
        mode: Mode::Measure,
        gated: true,
        judge: false,
        host: host(),
        commit: Some("0123abc".into()),
        entries: vec![
            // Over budget, then within it on the retry.
            entry(
                0,
                Status::Ok,
                None,
                vec![
                    Value::Time(us(&[1200, 1300, 1250])),
                    Value::Time(us(&[900, 950, 980])),
                ],
            ),
            entry(
                1,
                Status::Over,
                None,
                vec![Value::Throughput {
                    bytes: 50 << 20,
                    time: us(&[300_000, 310_000, 305_000]),
                }],
            ),
            entry(
                2,
                Status::Ok,
                None,
                vec![Value::Ratio {
                    subject: us(&[2, 2, 3]),
                    baseline: us(&[1, 1, 1]),
                }],
            ),
            entry(
                3,
                Status::Ok,
                None,
                vec![Value::Bytes(vec![40 << 20, 41 << 20, 42 << 20])],
            ),
            entry(
                4,
                Status::Ok,
                None,
                vec![Value::Frames {
                    work: us(&[4000, 5000, 4500]),
                    intervals: us(&[16_700, 16_600]),
                    refresh: Duration::from_nanos(16_666_667),
                }],
            ),
            entry(
                5,
                Status::Record,
                None,
                vec![Value::Time(us(&[10, 11, 12]))],
            ),
            entry(
                6,
                Status::Ok,
                None,
                vec![Value::Time(us(&[100, 300, 500, 200, 600]))],
            ),
            entry(7, Status::Error, Some("panic：boom"), vec![]),
            entry(8, Status::Skipped, Some("只在 macOS 运行"), vec![]),
            entry(9, Status::Filtered, None, vec![]),
            entry(10, Status::Planned, Some("由 fake 第 2 步 加入"), vec![]),
            entry(11, Status::Smoke, None, vec![Value::Time(us(&[1500]))]),
        ],
        errors: vec!["基准 fake/extra 不对应本目标的任何预算".into()],
        plugins: Vec::new(),
        language_servers: Vec::new(),
        benches: Vec::new(),
    }
}

fn results(report: &Report<'_>) -> Vec<serde_json::Value> {
    report.to_json()["results"].as_array().unwrap().clone()
}

#[test]
fn report_json() {
    let json = serde_json::to_string_pretty(&fake_report().to_json()).unwrap();
    insta::assert_snapshot!(json);
}

#[test]
fn report_text_table() {
    let report = fake_report();
    let text = report::text_table(
        &report::title("time", "measure", true, false),
        &results(&report),
        &report.errors,
    );
    insta::assert_snapshot!(text);
}

#[test]
fn report_markdown_summary() {
    let report = fake_report();
    let markdown = report::markdown(
        &report::title("report", "measure", true, false),
        &results(&report),
        &report.errors,
    );
    insta::assert_snapshot!(markdown);
}

#[test]
fn report_process_lists_plugins_and_language_servers() {
    let report = Report {
        target: Target::Process,
        plugins: vec!["terminal".into()],
        language_servers: vec!["jdtls".into()],
        entries: Vec::new(),
        errors: Vec::new(),
        ..fake_report()
    };
    let json = report.to_json();
    assert_eq!(json["plugins"], serde_json::json!(["terminal"]));
    assert_eq!(json["language_servers"], serde_json::json!(["jdtls"]));
    // Other targets have neither.
    assert!(fake_report().to_json().get("plugins").is_none());
}

#[test]
fn report_each_target_writes_only_its_own_file() {
    static MEMORY: &[Budget] = &[
        budget("fake/time", UNDER_1_MS, State::Active, Target::Time),
        budget("fake/bytes", &[], State::Planned("test"), Target::Memory),
    ];
    let dir = tempfile::tempdir().unwrap();
    let options = options(MEMORY, Target::Memory, &[], dir.path());
    let benches: [Bench; 0] = [];
    let run = run(&options, &benches);
    assert_eq!(run.exit, 0, "{}", run.text);
    let report = run.report.unwrap();
    report::write(&options.report_dir, Target::Memory, &report).unwrap();
    let files: Vec<String> = std::fs::read_dir(&options.report_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(files, ["report-memory.json"]);
    let json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(options.report_dir.join("report-memory.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(json["target"], "memory");
    // Only this target's budgets, each once.
    let ids: Vec<&str> = json["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["fake/bytes"]);
}
