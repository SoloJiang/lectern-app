use std::path::Path;

use serde_json::{Value as Json, json};

use super::*;
use crate::merge::{Merged, NOTHING_JUDGED, merge};

static TABLE: &[Budget] = &[
    budget("t/a", UNDER_1_MS, State::Active, Target::Time),
    budget("t/later", UNDER_1_MS, State::Planned("test"), Target::Time),
    budget("m/a", &[], State::Planned("test"), Target::Memory),
    // The judging machine's entry (ADR 0052).
    Budget {
        judging_machine: true,
        ..budget("p/a", UNDER_1_MS, State::Active, Target::Process)
    },
    budget(
        "plugin/*/activate",
        UNDER_1_MS,
        State::Active,
        Target::Process,
    ),
    budget(
        "plugin/special/activate",
        UNDER_1_MS,
        State::Active,
        Target::Process,
    ),
    budget("lsp/*/resident", &[], State::Active, Target::Process),
    budget("pkg/size", &[], State::Planned("test"), Target::Package),
];

struct Reports<'a> {
    dir: &'a Path,
    gated: bool,
}

impl Reports<'_> {
    /// Writes `report-<target>.json` with `(id, status)` results.
    fn write(&self, target: Target, results: &[(&str, &str)], extra: Json) {
        let mut json = json!({
            "version": 1,
            "target": target.name(),
            "mode": "measure",
            "gated": self.gated,
            "host": { "model": null, "chip": null, "os_version": null, "cpus": 8 },
            "commit": null,
            "results": results.iter().map(|(id, status)| json!({
                "id": id,
                "status": status,
                "budget": "",
                "source": SOURCE,
                "measured": "",
                "attempts": [],
                "noisy": false,
            })).collect::<Vec<_>>(),
            "errors": [],
        });
        if let (Json::Object(json), Json::Object(extra)) = (&mut json, extra) {
            json.extend(extra);
        }
        std::fs::write(
            self.dir.join(format!("report-{}.json", target.name())),
            serde_json::to_string(&json).unwrap(),
        )
        .unwrap();
    }

    /// The three benchmark targets' reports with every entry, the plugin
    /// and language server lists empty.
    fn complete(&self) {
        self.write(
            Target::Time,
            &[("t/a", "ok"), ("t/later", "planned")],
            json!({}),
        );
        self.write(Target::Memory, &[("m/a", "planned")], json!({}));
        self.process(&[], &[], &[]);
    }

    fn process(&self, plugins: &[&str], servers: &[&str], extra: &[(&str, &str)]) {
        let mut results = vec![("p/a", "ok"), ("plugin/special/activate", "ok")];
        if plugins.is_empty() {
            results.push(("plugin/*/activate", "skipped"));
        }
        if servers.is_empty() {
            results.push(("lsp/*/resident", "skipped"));
        }
        results.extend_from_slice(extra);
        self.write(
            Target::Process,
            &results,
            json!({ "plugins": plugins, "language_servers": servers }),
        );
    }
}

fn run(dir: &Path, require_package: bool) -> Merged {
    merge(dir, TABLE, require_package, Some("abc".into()))
}

fn statuses(merged: &Merged) -> Vec<(String, String)> {
    merged.report["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["id"].as_str().unwrap().to_owned(),
                r["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[test]
fn report_merge_lists_every_budget_once() {
    let dir = tempfile::tempdir().unwrap();
    Reports {
        dir: dir.path(),
        gated: true,
    }
    .complete();
    let merged = run(dir.path(), false);
    assert_eq!(merged.exit, 0, "{}", merged.text);
    let s = statuses(&merged);
    let ids: Vec<&str> = s.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "t/a",
            "t/later",
            "m/a",
            "p/a",
            "plugin/*/activate",
            "plugin/special/activate",
            "lsp/*/resident",
            "pkg/size"
        ]
    );
    // Without report-package.json the package entries are skipped, not
    // missing.
    let package = &merged.report["results"][7];
    assert_eq!(package["status"], "skipped");
    assert_eq!(package["reason"], "未打包");
    assert_eq!(merged.report["commit"], "abc");
    assert_eq!(merged.report["gated"], true);
    assert_eq!(merged.report["errors"], json!([]));
}

#[test]
fn report_merge_requires_the_package_report_when_asked() {
    let dir = tempfile::tempdir().unwrap();
    let reports = Reports {
        dir: dir.path(),
        gated: true,
    };
    reports.complete();
    assert_eq!(run(dir.path(), true).exit, 2);
    reports.write(Target::Package, &[("pkg/size", "planned")], json!({}));
    let merged = run(dir.path(), true);
    assert_eq!(merged.exit, 0, "{}", merged.text);
    assert_eq!(merged.report["results"][7]["status"], "planned");
}

#[test]
fn report_merge_fails_without_a_target_report() {
    for missing in ["time", "memory", "process"] {
        let dir = tempfile::tempdir().unwrap();
        Reports {
            dir: dir.path(),
            gated: false,
        }
        .complete();
        std::fs::remove_file(dir.path().join(format!("report-{missing}.json"))).unwrap();
        let merged = run(dir.path(), false);
        assert_eq!(merged.exit, 2, "{missing}");
        assert!(merged.text.contains(&format!("report-{missing}.json")));
    }
}

#[test]
fn report_merge_fails_on_missing_duplicate_or_misplaced_entries() {
    let dir = tempfile::tempdir().unwrap();
    let reports = Reports {
        dir: dir.path(),
        gated: false,
    };
    reports.complete();
    // Missing.
    reports.write(Target::Time, &[("t/a", "ok")], json!({}));
    let merged = run(dir.path(), false);
    assert_eq!(merged.exit, 2);
    assert!(statuses(&merged).contains(&("t/later".into(), "error".into())));
    // Twice.
    reports.write(
        Target::Time,
        &[("t/a", "ok"), ("t/later", "planned"), ("t/a", "ok")],
        json!({}),
    );
    assert_eq!(run(dir.path(), false).exit, 2);
    // In another target's report.
    reports.write(Target::Time, &[("t/a", "ok")], json!({}));
    reports.write(
        Target::Memory,
        &[("m/a", "planned"), ("t/later", "planned")],
        json!({}),
    );
    assert_eq!(run(dir.path(), false).exit, 2);
    // Not in the table.
    reports.complete();
    reports.write(
        Target::Time,
        &[("t/a", "ok"), ("t/later", "planned"), ("t/b", "ok")],
        json!({}),
    );
    assert_eq!(run(dir.path(), false).exit, 2);
}

#[test]
fn report_merge_expands_plugin_wildcards_over_the_plugin_list() {
    let dir = tempfile::tempdir().unwrap();
    let reports = Reports {
        dir: dir.path(),
        gated: false,
    };
    reports.complete();
    // `special` has its own entry; `terminal` is expanded from the wildcard.
    reports.process(
        &["terminal", "special"],
        &[],
        &[("plugin/terminal/activate", "ok")],
    );
    let merged = run(dir.path(), false);
    assert_eq!(merged.exit, 0, "{}", merged.text);
    let s = statuses(&merged);
    assert!(s.contains(&("plugin/terminal/activate".into(), "ok".into())));
    assert!(s.iter().all(|(id, _)| id != "plugin/*/activate"));
    // A plugin of the list without its entry.
    reports.process(
        &["terminal", "git"],
        &[],
        &[("plugin/terminal/activate", "ok")],
    );
    assert_eq!(run(dir.path(), false).exit, 2);
    // The wildcard entry itself while the list is not empty.
    reports.process(
        &["terminal"],
        &[],
        &[
            ("plugin/terminal/activate", "ok"),
            ("plugin/*/activate", "skipped"),
        ],
    );
    assert_eq!(run(dir.path(), false).exit, 2);
}

#[test]
fn report_merge_expands_language_servers() {
    let dir = tempfile::tempdir().unwrap();
    let reports = Reports {
        dir: dir.path(),
        gated: false,
    };
    reports.complete();
    reports.process(
        &[],
        &["jdtls", "tsserver"],
        &[
            ("lsp/jdtls/resident", "record"),
            ("lsp/tsserver/resident", "record"),
        ],
    );
    let merged = run(dir.path(), false);
    assert_eq!(merged.exit, 0, "{}", merged.text);
    reports.process(
        &[],
        &["jdtls", "tsserver"],
        &[("lsp/jdtls/resident", "record")],
    );
    assert_eq!(run(dir.path(), false).exit, 2);
}

#[test]
fn report_merge_exit_is_the_worst_result() {
    let dir = tempfile::tempdir().unwrap();
    let gated = Reports {
        dir: dir.path(),
        gated: true,
    };
    gated.complete();
    gated.write(
        Target::Time,
        &[("t/a", "over"), ("t/later", "planned")],
        json!({}),
    );
    assert_eq!(run(dir.path(), false).exit, 1);
    gated.write(Target::Memory, &[("m/a", "error")], json!({}));
    assert_eq!(run(dir.path(), false).exit, 2);
    // Over budget off the gating platform does not fail.
    let ungated = Reports {
        dir: dir.path(),
        gated: false,
    };
    ungated.complete();
    ungated.write(
        Target::Time,
        &[("t/a", "over"), ("t/later", "planned")],
        json!({}),
    );
    assert_eq!(run(dir.path(), false).exit, 0);
    // Reports that disagree on gating are an error.
    gated.write(
        Target::Time,
        &[("t/a", "ok"), ("t/later", "planned")],
        json!({}),
    );
    assert_eq!(run(dir.path(), false).exit, 2);
}

#[test]
fn report_merge_counts_filtered_entries_as_present() {
    let dir = tempfile::tempdir().unwrap();
    let reports = Reports {
        dir: dir.path(),
        gated: true,
    };
    reports.complete();
    reports.write(
        Target::Time,
        &[("t/a", "filtered"), ("t/later", "filtered")],
        json!({}),
    );
    let merged = run(dir.path(), false);
    assert_eq!(merged.exit, 0, "{}", merged.text);
    assert_eq!(merged.report["results"][0]["status"], "filtered");
}

#[test]
fn report_merge_passes_on_target_errors() {
    let dir = tempfile::tempdir().unwrap();
    let reports = Reports {
        dir: dir.path(),
        gated: false,
    };
    reports.complete();
    reports.write(
        Target::Memory,
        &[("m/a", "planned")],
        json!({ "errors": ["基准 m/x 不对应本目标的任何预算"] }),
    );
    let merged = run(dir.path(), false);
    assert_eq!(merged.exit, 2);
    assert!(merged.text.contains("m/x"));
}

#[test]
fn report_merge_annotates_failed_budgets() {
    let report = json!({
        "results": [
            {"id": "t/a", "status": "ok", "measured": "median 1 µs", "budget": "< 1 ms"},
            {"id": "t/b", "status": "over", "measured": "median 2 ms", "budget": "median < 1 ms"},
            {"id": "t/c", "status": "error", "measured": "", "budget": "< 1 ms", "reason": "panicked: 100%\nboom"},
        ],
        "errors": ["基准 fake/extra 不对应本目标的任何预算"],
    });
    assert_eq!(
        crate::merge::annotations(&report),
        [
            "::error title=t/b::over: median 2 ms (budget: median < 1 ms)",
            "::error title=t/c::error:  (budget: < 1 ms); panicked: 100%25%0Aboom",
            "::error title=bench::基准 fake/extra 不对应本目标的任何预算",
        ]
    );
}

#[test]
fn report_merge_annotations_stay_within_ten() {
    let results: Vec<Json> = (0..12)
        .map(|i| json!({"id": format!("t/{i}"), "status": "over", "measured": "m", "budget": "b"}))
        .collect();
    let lines = crate::merge::annotations(&json!({"results": results, "errors": []}));
    assert_eq!(lines.len(), 10);
    assert_eq!(
        lines[9],
        "::error title=more failed budgets::t/9, t/10, t/11"
    );
}

#[test]
fn report_merge_notices_group_record_results() {
    let report = json!({
        "results": [
            {"id": "git/changes_clean_100k/stat_all", "status": "record", "measured": "median 310 ms"},
            {"id": "git/changes_clean_100k", "status": "planned", "measured": ""},
            {"id": "cache/zstd_1m/put", "status": "record", "measured": "median 2 ms"},
            {"id": "git/filter_100k/walk", "status": "record", "measured": "median 1.2 s"},
            {"id": "t/a", "status": "ok", "measured": "median 1 µs"},
            {"id": "t/b", "status": "smoke", "measured": "median 5 µs"},
        ],
        "errors": [],
    });
    assert_eq!(
        crate::merge::notices(&report),
        [
            "::notice title=cache record::cache/zstd_1m/put: median 2 ms",
            "::notice title=git record::git/changes_clean_100k/stat_all: median 310 ms%0A\
             git/filter_100k/walk: median 1.2 s",
        ]
    );
}

/// A result's note (`Measured::note`, such as the diagnosis of a round the
/// time box cut) follows its value in the record notice and its reason in
/// the error annotation.
#[test]
fn report_merge_notes_reach_the_annotations() {
    let report = json!({
        "results": [
            {"id": "t/cut", "status": "record", "measured": "1", "note": "cut: 2 s of 1.9 s"},
            {"id": "t/all", "status": "over", "measured": "median 3 s", "budget": "median ≤ 2 s", "note": "cut: 2 s of 1.9 s"},
        ],
        "errors": [],
    });
    assert_eq!(
        crate::merge::notices(&report),
        ["::notice title=t record::t/cut: 1; cut: 2 s of 1.9 s"]
    );
    assert_eq!(
        crate::merge::annotations(&report),
        ["::error title=t/all::over: median 3 s (budget: median ≤ 2 s); cut: 2 s of 1.9 s"]
    );
}

#[test]
fn report_merge_notices_keep_every_record_within_ten() {
    let results: Vec<Json> = (0..12)
        .map(|i| json!({"id": format!("g{i:02}/x"), "status": "record", "measured": "m"}))
        .collect();
    let lines = crate::merge::notices(&json!({"results": results, "errors": []}));
    assert_eq!(lines.len(), 10);
    assert_eq!(lines[0], "::notice title=g00 record::g00/x: m");
    assert_eq!(
        lines[9],
        "::notice title=g09 g10 g11 record::g09/x: m%0Ag10/x: m%0Ag11/x: m"
    );
}

#[test]
fn report_merge_lists_the_slowest_benchmarks_first() {
    let dir = tempfile::tempdir().unwrap();
    let reports = Reports {
        dir: dir.path(),
        gated: false,
    };
    reports.complete();
    reports.write(
        Target::Time,
        &[("t/a", "ok"), ("t/later", "planned")],
        json!({ "benches": [
            {"id": "t/a", "seconds": 1.5, "attempts": 1},
            {"id": "t/slow", "seconds": 30.31, "attempts": 2},
        ] }),
    );
    reports.write(
        Target::Memory,
        &[("m/a", "planned")],
        json!({ "benches": [{"id": "m/x", "seconds": 4.0, "attempts": 1}] }),
    );
    let merged = run(dir.path(), false);
    let ids: Vec<&str> = merged.report["benches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["t/slow", "m/x", "t/a"]);
    assert_eq!(merged.report["benches"][1]["target"], "memory");
    assert!(merged.text.contains("t/slow (time): 30.3 s, retried"));
    let notices = crate::merge::notices(&merged.report);
    assert_eq!(
        notices[0],
        "::notice title=slowest benchmarks::3 benchmarks, 35.8 s in all%0A\
         t/slow (time): 30.3 s, retried%0Am/x (memory): 4.0 s%0At/a (time): 1.5 s"
    );
}

#[test]
fn report_merge_notices_leave_room_for_the_slowest() {
    let results: Vec<Json> = (0..12)
        .map(|i| json!({"id": format!("g{i:02}/x"), "status": "record", "measured": "m"}))
        .collect();
    let benches = [json!({"id": "g00/x", "target": "time", "seconds": 1.0, "attempts": 1})];
    let lines = crate::merge::notices(&json!({"results": results, "benches": benches}));
    assert_eq!(lines.len(), 10);
    assert!(lines[0].starts_with("::notice title=slowest benchmarks::"));
    assert_eq!(
        lines[9],
        "::notice title=g08 g09 g10 g11 record::g08/x: m%0Ag09/x: m%0Ag10/x: m%0Ag11/x: m"
    );
}

/// The judging machine's reports say so in the merged report and its
/// title; a report that does not (or has no `judge`) is an error there
/// (ADR 0052).
#[test]
fn report_merge_keeps_the_judging_machine() {
    let dir = tempfile::tempdir().unwrap();
    let reports = Reports {
        dir: dir.path(),
        gated: true,
    };
    let judge = json!({ "judge": true });
    reports.write(
        Target::Time,
        &[("t/a", "ok"), ("t/later", "planned")],
        judge.clone(),
    );
    reports.write(Target::Memory, &[("m/a", "planned")], judge.clone());
    reports.write(
        Target::Process,
        &[
            ("p/a", "ok"),
            ("plugin/special/activate", "ok"),
            ("plugin/*/activate", "skipped"),
            ("lsp/*/resident", "skipped"),
        ],
        json!({ "plugins": [], "language_servers": [], "judge": true }),
    );
    let merged = run(dir.path(), false);
    assert_eq!(merged.exit, 0, "{}", merged.text);
    assert_eq!(merged.report["judge"], true);
    assert!(
        merged.text.contains("measure, gated, judging machine"),
        "{}",
        merged.text
    );

    reports.write(
        Target::Memory,
        &[("m/a", "planned")],
        json!({ "judge": false }),
    );
    let merged = run(dir.path(), false);
    assert_eq!(merged.exit, 2);
    let errors = merged.report["errors"].to_string();
    assert!(errors.contains("judge"), "{errors}");

    // Without `judge` (an older report) is not the judging machine.
    reports.complete();
    let merged = run(dir.path(), false);
    assert_eq!(merged.exit, 0, "{}", merged.text);
    assert_eq!(merged.report["judge"], false);
}

/// A judging run must run at least one of the judging machine's entries: a
/// filter or plan that leaves none of them judges nothing and must not
/// pass (ADR 0052). Elsewhere it is no error.
#[test]
fn report_merge_judging_run_needs_a_judging_machine_entry() {
    let dir = tempfile::tempdir().unwrap();
    let reports = Reports {
        dir: dir.path(),
        gated: true,
    };
    let write = |status: &str, judge: bool| {
        let extra = json!({ "judge": judge });
        reports.write(
            Target::Time,
            &[("t/a", "ok"), ("t/later", "planned")],
            extra.clone(),
        );
        reports.write(Target::Memory, &[("m/a", "planned")], extra);
        reports.write(
            Target::Process,
            &[
                ("p/a", status),
                ("plugin/special/activate", "ok"),
                ("plugin/*/activate", "skipped"),
                ("lsp/*/resident", "skipped"),
            ],
            json!({ "plugins": [], "language_servers": [], "judge": judge }),
        );
    };
    for status in ["filtered", "planned", "skipped"] {
        write(status, true);
        let merged = run(dir.path(), false);
        assert_eq!(merged.exit, 2, "{status}: {}", merged.text);
        let errors = merged.report["errors"].to_string();
        assert!(errors.contains(NOTHING_JUDGED), "{status}: {errors}");

        write(status, false);
        let merged = run(dir.path(), false);
        assert_eq!(merged.exit, 0, "{status}: {}", merged.text);
    }
    for status in ["ok", "record"] {
        write(status, true);
        let merged = run(dir.path(), false);
        assert_eq!(merged.exit, 0, "{status}: {}", merged.text);
    }
}
