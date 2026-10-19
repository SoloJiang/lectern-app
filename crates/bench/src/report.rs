//! Reports: the JSON written to `report-<target>.json`, and the text table
//! and Markdown summary rendered from it (docs/design/bench.md「报告」).
//! The merged report uses the same result objects, so one renderer serves
//! both.

use std::fmt::Write as _;
use std::path::Path;

use serde_json::{Map, Value as Json, json};

use crate::budgets::{Budget, Target};
use crate::stats::{self, Summary, bytes_summary, ratio};
use crate::{Mode, Value};

pub(crate) const VERSION: u64 = 1;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Status {
    Ok,
    Over,
    Record,
    Planned,
    Filtered,
    Smoke,
    Skipped,
    Error,
}

impl Status {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Over => "over",
            Status::Record => "record",
            Status::Planned => "planned",
            Status::Filtered => "filtered",
            Status::Smoke => "smoke",
            Status::Skipped => "skipped",
            Status::Error => "error",
        }
    }
}

/// One result line: a budget (a wildcard budget once per expanded ID) with
/// its status and the attempts measured for it.
#[derive(Clone, Debug)]
pub(crate) struct Entry<'a> {
    pub id: String,
    pub budget: &'a Budget,
    pub status: Status,
    pub reason: Option<String>,
    /// First attempt, and the retry when the first was over budget.
    pub attempts: Vec<Value>,
}

impl<'a> Entry<'a> {
    pub(crate) fn new(budget: &'a Budget, status: Status, reason: Option<String>) -> Entry<'a> {
        Entry {
            id: budget.id.to_owned(),
            budget,
            status,
            reason,
            attempts: Vec::new(),
        }
    }
}

/// The machine. The model, chip and OS version come from
/// `platform::host` once it exists (bench step 2); until then only the CPU
/// count is filled in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Host {
    pub model: Option<String>,
    pub chip: Option<String>,
    pub os_version: Option<String>,
    pub cpus: u32,
}

impl Host {
    pub(crate) fn current() -> Host {
        Host {
            model: None,
            chip: None,
            os_version: None,
            cpus: std::thread::available_parallelism().map_or(1, |n| n.get() as u32),
        }
    }

    pub(crate) fn to_json(&self) -> Json {
        json!({
            "model": self.model,
            "chip": self.chip,
            "os_version": self.os_version,
            "cpus": self.cpus,
        })
    }
}

/// One target's report.
#[derive(Clone, Debug)]
pub(crate) struct Report<'a> {
    pub target: Target,
    pub mode: Mode,
    pub gated: bool,
    pub host: Host,
    pub commit: Option<String>,
    pub entries: Vec<Entry<'a>>,
    /// Problems that belong to no budget: extra benchmarks or results.
    pub errors: Vec<String>,
    /// For `process`: the plugins of this run (the `Plugins` reply of
    /// `app/idle_empty`, without `bench-probe`) and the language servers
    /// that ran, by which the merge expands `plugin/*` and `lsp/*`.
    pub plugins: Vec<String>,
    pub language_servers: Vec<String>,
    /// How long each benchmark that ran took, fixtures and retries included.
    pub benches: Vec<BenchTime>,
}

/// The wall time of one benchmark's run: every attempt, from creating its
/// scratch directory to removing it, so fixture generation counts.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BenchTime {
    pub id: String,
    pub seconds: f64,
    pub attempts: u32,
}

impl Report<'_> {
    pub(crate) fn to_json(&self) -> Json {
        let mut out = Map::new();
        out.insert("version".into(), json!(VERSION));
        out.insert("target".into(), json!(self.target.name()));
        out.insert("mode".into(), json!(self.mode.name()));
        out.insert("gated".into(), json!(self.gated));
        out.insert("host".into(), self.host.to_json());
        out.insert("commit".into(), json!(self.commit));
        if self.target == Target::Process {
            out.insert("plugins".into(), json!(self.plugins));
            out.insert("language_servers".into(), json!(self.language_servers));
        }
        out.insert(
            "results".into(),
            Json::Array(self.entries.iter().map(entry_json).collect()),
        );
        out.insert("errors".into(), json!(self.errors));
        if !self.benches.is_empty() {
            let benches = self
                .benches
                .iter()
                .map(|b| json!({ "id": b.id, "seconds": b.seconds, "attempts": b.attempts }))
                .collect();
            out.insert("benches".into(), Json::Array(benches));
        }
        sorted(Json::Object(out))
    }
}

/// Sorts object keys so the report is byte-identical on every platform:
/// GPUI turns on serde_json's `preserve_order` in macOS builds, which keeps
/// insertion order instead of sorting.
pub(crate) fn sorted(json: Json) -> Json {
    match json {
        Json::Object(map) => {
            let mut entries: Vec<_> = map.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Json::Object(entries.into_iter().map(|(k, v)| (k, sorted(v))).collect())
        }
        Json::Array(items) => Json::Array(items.into_iter().map(sorted).collect()),
        other => other,
    }
}

pub(crate) fn file_name(target: Target) -> String {
    format!("report-{}.json", target.name())
}

pub(crate) fn write(dir: &Path, target: Target, report: &Report<'_>) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let text = serde_json::to_string_pretty(&report.to_json()).map_err(std::io::Error::other)?;
    std::fs::write(dir.join(file_name(target)), text + "\n")
}

fn entry_json(entry: &Entry<'_>) -> Json {
    let mut out = Map::new();
    out.insert("id".into(), json!(entry.id));
    out.insert("status".into(), json!(entry.status.name()));
    if let Some(reason) = &entry.reason {
        out.insert("reason".into(), json!(reason));
    }
    out.insert("budget".into(), json!(entry.budget.describe()));
    out.insert("source".into(), json!(entry.budget.source));
    out.insert("measured".into(), json!(measured(entry)));
    out.insert(
        "attempts".into(),
        Json::Array(entry.attempts.iter().map(value_json).collect()),
    );
    out.insert(
        "noisy".into(),
        json!(entry.attempts.last().is_some_and(Value::noisy)),
    );
    Json::Object(out)
}

/// The measured statistics in words, one part per attempt.
fn measured(entry: &Entry<'_>) -> String {
    entry
        .attempts
        .iter()
        .map(|value| {
            let parts: Vec<String> = entry
                .budget
                .checks
                .iter()
                .filter_map(|check| stats::evaluate(check, value).ok())
                .map(|outcome| outcome.measured)
                .collect();
            if parts.is_empty() {
                stats::describe(value)
            } else {
                parts.join(", ")
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn summary_json(summary: Summary, unit: &str) -> Map<String, Json> {
    let mut out = Map::new();
    out.insert("median".into(), json!(summary.median));
    out.insert("p95".into(), json!(summary.p95));
    out.insert("max".into(), json!(summary.max));
    out.insert("mad".into(), json!(summary.mad));
    out.insert("samples".into(), json!(summary.samples));
    out.insert("unit".into(), json!(unit));
    out
}

fn value_json(value: &Value) -> Json {
    match value {
        Value::Time(s) => Json::Object(summary_json(s.summary(), "s")),
        Value::Throughput { bytes, time } => {
            let summary = time.summary();
            let mut out = summary_json(summary, "s");
            out.insert("bytes".into(), json!(bytes));
            out.insert("throughput".into(), json!(*bytes as f64 / summary.median));
            Json::Object(out)
        }
        Value::Ratio { subject, baseline } => json!({
            "subject": summary_json(subject.summary(), "s"),
            "baseline": summary_json(baseline.summary(), "s"),
            "ratio": ratio(subject, baseline, crate::budgets::Stat::Median),
        }),
        Value::Bytes(b) => Json::Object(summary_json(bytes_summary(b), "B")),
        Value::Frames {
            work,
            intervals,
            refresh,
        } => json!({
            "work": summary_json(work.summary(), "s"),
            "intervals": summary_json(intervals.summary(), "s"),
            "refresh": refresh.as_secs_f64(),
        }),
        Value::Count(n) => json!({ "count": n, "samples": 1, "unit": "count" }),
    }
}

fn str_field<'j>(result: &'j Json, key: &str) -> &'j str {
    result.get(key).and_then(Json::as_str).unwrap_or("")
}

/// The status column: `ok`, `OVER`, `skipped (只在 macOS 运行)`.
fn status_cell(result: &Json) -> String {
    let status = str_field(result, "status");
    let shown = if status == "over" { "OVER" } else { status };
    let noisy = result.get("noisy").and_then(Json::as_bool) == Some(true);
    let mut cell = shown.to_owned();
    if noisy {
        cell.push_str(", noisy");
    }
    match result.get("reason").and_then(Json::as_str) {
        Some(reason) => format!("{cell} ({reason})"),
        None => cell,
    }
}

fn rows(results: &[Json]) -> Vec<[String; 5]> {
    results
        .iter()
        .map(|r| {
            [
                str_field(r, "id").to_owned(),
                str_field(r, "measured").to_owned(),
                str_field(r, "budget").to_owned(),
                status_cell(r),
                str_field(r, "source").to_owned(),
            ]
        })
        .collect()
}

const HEADER: [&str; 5] = ["ID", "Measured", "Budget", "Status", "Source"];

/// The plain-text table printed by each target and by the merge.
pub(crate) fn text_table(title: &str, results: &[Json], errors: &[String]) -> String {
    let rows = rows(results);
    let mut widths = HEADER.map(|h| h.chars().count());
    for row in &rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let line = |cells: [&str; 5]| -> String {
        let mut out = String::new();
        for (i, (cell, w)) in cells.iter().zip(widths).enumerate() {
            if i + 1 == cells.len() {
                out.push_str(cell);
            } else {
                out.push_str(cell);
                out.extend(std::iter::repeat_n(' ', w - cell.chars().count() + 2));
            }
        }
        out.trim_end().to_owned() + "\n"
    };
    let mut out = format!("{title}\n\n");
    out.push_str(&line(HEADER));
    for row in &rows {
        out.push_str(&line([&row[0], &row[1], &row[2], &row[3], &row[4]]));
    }
    for error in errors {
        writeln!(out, "error: {error}").unwrap();
    }
    out
}

/// The Markdown table appended to `GITHUB_STEP_SUMMARY`.
pub(crate) fn markdown(title: &str, results: &[Json], errors: &[String]) -> String {
    let escape = |s: &str| s.replace('|', "\\|");
    let mut out = format!("### {title}\n\n");
    out.push_str("| ID | Measured | Budget | Status | Source |\n");
    out.push_str("|---|---|---|---|---|\n");
    for row in rows(results) {
        writeln!(
            out,
            "| `{}` | {} | {} | {} | {} |",
            row[0],
            escape(&row[1]),
            escape(&row[2]),
            escape(&row[3]),
            row[4]
        )
        .unwrap();
    }
    if !errors.is_empty() {
        out.push('\n');
        for error in errors {
            writeln!(out, "- error: {}", escape(error)).unwrap();
        }
    }
    out
}

/// The title line: "lectern-bench time: smoke, not gated".
pub(crate) fn title(what: &str, mode: &str, gated: bool) -> String {
    format!(
        "lectern-bench {what}: {mode}, {}",
        if gated { "gated" } else { "not gated" }
    )
}
