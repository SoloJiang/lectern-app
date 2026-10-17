use std::path::Path;

use serde_json::{Value as Json, json};

use super::*;
use crate::merge::{Merged, merge};

static TABLE: &[Budget] = &[
    budget("t/a", UNDER_1_MS, State::Active, Target::Time),
    budget("t/later", UNDER_1_MS, State::Planned("test"), Target::Time),
    budget("m/a", &[], State::Planned("test"), Target::Memory),
    budget("p/a", UNDER_1_MS, State::Active, Target::Process),
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
