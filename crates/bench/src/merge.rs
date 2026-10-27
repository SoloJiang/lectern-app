//! `lectern-bench-report`: `merge` merges the targets' reports (of one run,
//! or of the shards of one run) into `report.json` and decides `just
//! bench`'s exit code; `shard` names a shard's benchmark targets
//! (docs/design/bench.md「报告」「分片」).

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::{Map, Value as Json, json};

use crate::budgets::{BUDGETS, Budget, Target};
use crate::report::{self, VERSION};
use crate::shards;

const USAGE: &str = "usage: lectern-bench-report merge [--require package] <dir>...\n       lectern-bench-report shard <name>";

/// The error for a judging run in which none of the judging machine's
/// entries ran (ADR 0052).
pub(crate) const NOTHING_JUDGED: &str = "LECTERN_BENCH_JUDGE=1 的运行里，判定机器的条目一条都没有运行（都被过滤、都是计划中或都跳过）：这次运行不判定";

/// The binary's entry point; `args` without the program name.
pub fn main(args: impl IntoIterator<Item = String>) -> ExitCode {
    let args: Vec<String> = args.into_iter().collect();
    let Some((command, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    match command.as_str() {
        "merge" => {}
        "shard" => return shard_targets(rest),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    }
    let mut require_package = false;
    let mut dirs = Vec::new();
    let mut rest = rest.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--require" => match rest.next().map(String::as_str) {
                Some("package") => require_package = true,
                _ => {
                    eprintln!("{USAGE}");
                    return ExitCode::from(2);
                }
            },
            path if !path.starts_with('-') => dirs.push(PathBuf::from(path)),
            _ => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    if dirs.is_empty() {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let merged = merge(
        &dirs,
        BUDGETS,
        require_package,
        std::env::var("GITHUB_SHA").ok(),
    );
    print!("{}", merged.text);
    if std::env::var_os("GITHUB_ACTIONS").is_some_and(|v| v == "true") {
        for line in annotations(&merged.report) {
            println!("{line}");
        }
        for line in notices(&merged.report) {
            println!("{line}");
        }
    }
    let text = serde_json::to_string_pretty(&merged.report).unwrap_or_default();
    // The merged report goes into the first directory.
    if let Err(error) = std::fs::write(dirs[0].join("report.json"), text + "\n") {
        eprintln!("lectern-bench-report: could not write report.json: {error}");
        return ExitCode::from(2);
    }
    if let Some(summary) = std::env::var_os("GITHUB_STEP_SUMMARY") {
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(summary)
            .and_then(|mut f| f.write_all(merged.markdown.as_bytes()));
        if let Err(error) = written {
            eprintln!("lectern-bench-report: could not append the step summary: {error}");
        }
    }
    ExitCode::from(merged.exit)
}

/// `shard <name>`: the shard's benchmark targets, one per line, for `just
/// bench --shard`.
fn shard_targets(args: &[String]) -> ExitCode {
    let [name] = args else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    match target_names(name) {
        Ok(names) => {
            for name in names {
                println!("{name}");
            }
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("lectern-bench-report: {message}");
            ExitCode::from(2)
        }
    }
}

/// The benchmark target names of the shard `name`.
pub(crate) fn target_names(name: &str) -> Result<Vec<&'static str>, String> {
    let Some(shard) = shards::by_name(name) else {
        let known: Vec<&str> = shards::SHARDS.iter().map(|s| s.name).collect();
        return Err(format!(
            "unknown shard {name:?}, the shards are: {}",
            known.join(", ")
        ));
    };
    Ok(shard.targets.iter().map(|t| t.name()).collect())
}

/// GitHub keeps at most 10 annotations of each level (error, warning,
/// notice) per step.
const MAX_ANNOTATIONS: usize = 10;

/// A workflow command's message, escaped.
fn escape(text: &str) -> String {
    text.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// A workflow command's `title` property, escaped.
fn escape_title(text: &str) -> String {
    escape(text).replace(':', "%3A").replace(',', "%2C")
}

/// `::notice` workflow commands for the record-only results, so their values
/// can be read from the check run's annotations without the job log or the
/// report artifact. One notice per group (the ID's first segment, such as
/// `git`), one line `<id>: <measured>` per result; past 10 groups the rest
/// share the last notice, so no result is dropped. When the report lists
/// benchmark wall times, the first notice names the slowest benchmarks and
/// the groups share the other 9.
pub(crate) fn notices(report: &Json) -> Vec<String> {
    let slowest = slowest(report);
    let mut out: Vec<String> = slowest
        .iter()
        .map(|lines| {
            format!(
                "::notice title=slowest benchmarks::{}",
                escape(&lines.join("\n"))
            )
        })
        .collect();
    out.extend(record_notices(report, MAX_ANNOTATIONS - out.len()));
    out
}

/// How many benchmarks the slowest-benchmarks notice and summary list.
const SLOWEST: usize = 10;

/// `<id> (<target>[, <shard>]): <seconds> s` for the slowest benchmarks of the merged
/// report, slowest first, after a line with the total; None without wall
/// times.
pub(crate) fn slowest(report: &Json) -> Option<Vec<String>> {
    let benches = report["benches"].as_array().filter(|b| !b.is_empty())?;
    let seconds = |b: &Json| b["seconds"].as_f64().unwrap_or(0.0);
    let total: f64 = benches.iter().map(seconds).sum();
    let mut lines = vec![format!("{} benchmarks, {total:.1} s in all", benches.len())];
    lines.extend(benches.iter().take(SLOWEST).map(|b| {
        let retried = if b["attempts"].as_u64().unwrap_or(1) > 1 {
            ", retried"
        } else {
            ""
        };
        // Which shard it ran in, to rearrange the shards by it (「分片」).
        let shard = b["shard"]
            .as_str()
            .map_or_else(String::new, |shard| format!(", {shard}"));
        format!(
            "{} ({}{shard}): {:.1} s{retried}",
            b["id"].as_str().unwrap_or_default(),
            b["target"].as_str().unwrap_or_default(),
            seconds(b)
        )
    }));
    Some(lines)
}

/// The record notices of [`notices`], at most `max`.
fn record_notices(report: &Json, max: usize) -> Vec<String> {
    let mut groups: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for result in report["results"].as_array().into_iter().flatten() {
        if result["status"].as_str() != Some("record") {
            continue;
        }
        let id = result["id"].as_str().unwrap_or_default();
        let measured = result["measured"].as_str().unwrap_or_default();
        let group = id.split('/').next().unwrap_or(id);
        let line = match result["note"].as_str() {
            Some(note) => format!("{id}: {measured}; {note}"),
            None => format!("{id}: {measured}"),
        };
        groups.entry(group).or_default().push(line);
    }
    let mut groups: Vec<(String, Vec<String>)> = groups
        .into_iter()
        .map(|(group, lines)| (group.to_owned(), lines))
        .collect();
    if groups.len() > max {
        let rest = groups.split_off(max.saturating_sub(1));
        let names: Vec<&str> = rest.iter().map(|(group, _)| group.as_str()).collect();
        let title = names.join(" ");
        let lines = rest.iter().flat_map(|(_, lines)| lines.clone()).collect();
        groups.push((title, lines));
    }
    groups
        .into_iter()
        .map(|(group, lines)| {
            format!(
                "::notice title={} record::{}",
                escape_title(&group),
                escape(&lines.join("\n"))
            )
        })
        .collect()
}

/// `::error` workflow commands for every result over budget or in error and
/// every report-level error, so the check run's annotations say which budget
/// failed without the job log.
pub(crate) fn annotations(report: &Json) -> Vec<String> {
    let mut failed: Vec<(String, String)> = Vec::new();
    for result in report["results"].as_array().into_iter().flatten() {
        let status = result["status"].as_str().unwrap_or_default();
        if status != "over" && status != "error" {
            continue;
        }
        let field = |key: &str| result[key].as_str().unwrap_or_default().to_owned();
        let mut body = format!(
            "{status}: {} (budget: {})",
            field("measured"),
            field("budget")
        );
        for key in ["reason", "note"] {
            let text = field(key);
            if !text.is_empty() {
                body.push_str(&format!("; {text}"));
            }
        }
        failed.push((field("id"), body));
    }
    for error in report["errors"].as_array().into_iter().flatten() {
        failed.push((
            "bench".into(),
            error.as_str().unwrap_or_default().to_owned(),
        ));
    }
    let shown = if failed.len() <= MAX_ANNOTATIONS {
        failed.len()
    } else {
        MAX_ANNOTATIONS - 1
    };
    let mut lines: Vec<String> = failed[..shown]
        .iter()
        .map(|(title, body)| format!("::error title={}::{}", escape_title(title), escape(body)))
        .collect();
    if shown < failed.len() {
        let rest: Vec<&str> = failed[shown..].iter().map(|(id, _)| id.as_str()).collect();
        lines.push(format!(
            "::error title=more failed budgets::{}",
            escape(&rest.join(", "))
        ));
    }
    lines
}

pub(crate) struct Merged {
    pub report: Json,
    pub text: String,
    pub markdown: String,
    pub exit: u8,
}

/// One target's report as read from disk.
struct Read {
    target: Target,
    /// The report's name in messages: the file name, with its directory when
    /// several directories are merged.
    label: String,
    json: Json,
}

impl Read {
    /// The shard the report was limited to (`--shard`), if any.
    fn shard(&self) -> Option<&str> {
        self.json["shard"].as_str()
    }

    /// A process report that ran its budgets: no entry is `sharded`.
    fn ran_everything(&self) -> bool {
        !self.json["results"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|r| r["status"].as_str() == Some("sharded"))
    }
}

fn read(dir: &Path, target: Target, several: bool) -> Result<Option<Read>, String> {
    let path = dir.join(report::file_name(target));
    let label = if several {
        path.display().to_string()
    } else {
        report::file_name(target)
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("读不了 {}：{error}", path.display())),
    };
    let json: Json = serde_json::from_str(&text)
        .map_err(|e| format!("{} 不是合法的 JSON：{e}", path.display()))?;
    if json["version"] != json!(VERSION) {
        return Err(format!("{} 的版本不是 {VERSION}", path.display()));
    }
    if json["target"] != json!(target.name()) {
        return Err(format!(
            "{} 的 target 不是 {}",
            path.display(),
            target.name()
        ));
    }
    Ok(Some(Read {
        target,
        label,
        json,
    }))
}

fn strings(json: &Json) -> Vec<String> {
    json.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Every ID the reports must contain once: each budget, with `plugin/*`
/// expanded over `plugins` and `lsp/*` over `language_servers` (a plugin
/// with its own entry uses that entry); an empty list leaves the wildcard
/// entry itself.
pub(crate) fn expected<'a>(
    budgets: &'a [Budget],
    plugins: &[String],
    language_servers: &[String],
) -> Vec<(String, &'a Budget)> {
    let mut out = Vec::new();
    for budget in budgets {
        if !budget.is_wildcard() {
            out.push((budget.id.to_owned(), budget));
            continue;
        }
        let list = if budget.id.starts_with("plugin/") {
            plugins
        } else if budget.id.starts_with("lsp/") {
            language_servers
        } else {
            &[]
        };
        if list.is_empty() {
            out.push((budget.id.to_owned(), budget));
            continue;
        }
        for name in list {
            let id = budget.id.replacen('*', name, 1);
            if budgets.iter().any(|b| b.id == id) {
                continue;
            }
            out.push((id, budget));
        }
    }
    out
}

fn placeholder(id: &str, budget: &Budget, status: &str, reason: &str) -> Json {
    json!({
        "id": id,
        "status": status,
        "reason": reason,
        "budget": budget.describe(),
        "source": budget.source,
        "measured": "",
        "attempts": [],
        "noisy": false,
    })
}

/// Merges the reports in `dirs` (one directory of a plain run, or one per
/// shard). Per ID, the occurrences whose status is `sharded` are dropped
/// and exactly one must be left (「分片」).
pub(crate) fn merge(
    dirs: &[PathBuf],
    budgets: &[Budget],
    require_package: bool,
    commit: Option<String>,
) -> Merged {
    let mut errors: Vec<String> = Vec::new();
    let several = dirs.len() > 1;
    // Directory by directory, so the first report is the first directory's.
    let mut reports: Vec<Read> = Vec::new();
    let mut unreadable: Vec<Target> = Vec::new();
    for dir in dirs {
        for target in Target::ALL {
            match read(dir, target, several) {
                Ok(Some(r)) => reports.push(r),
                Ok(None) => {}
                Err(error) => {
                    errors.push(error);
                    unreadable.push(target);
                }
            }
        }
    }
    // Every target needs at least one report in some directory.
    for target in Target::ALL {
        if reports.iter().any(|r| r.target == target) || unreadable.contains(&target) {
            continue;
        }
        if target == Target::Package {
            if require_package {
                errors.push(format!(
                    "缺少 {}（带 --require package 时必须有）",
                    report::file_name(target)
                ));
            }
        } else {
            errors.push(format!(
                "缺少 {}：这个目标没有编译成功或中途崩溃",
                report::file_name(target)
            ));
        }
    }
    let has_package = reports.iter().any(|r| r.target == Target::Package);

    // Mode and gating must agree across the benchmark reports.
    let benchmarked: Vec<&Read> = reports
        .iter()
        .filter(|r| r.target != Target::Package)
        .collect();
    let mode = benchmarked
        .first()
        .and_then(|r| r.json["mode"].as_str())
        .unwrap_or("smoke")
        .to_owned();
    let gated = benchmarked
        .first()
        .and_then(|r| r.json["gated"].as_bool())
        .unwrap_or(false);
    // A report without `judge` is not from the judging machine (ADR 0052).
    let judged = |r: &Read| r.json["judge"].as_bool().unwrap_or(false);
    let judge = benchmarked.first().is_some_and(|r| judged(r));
    for r in &benchmarked {
        if r.json["mode"].as_str() != Some(&mode)
            || r.json["gated"].as_bool() != Some(gated)
            || judged(r) != judge
        {
            errors.push(format!(
                "{} 的 mode、gated 或 judge 与其他报告不同",
                r.label
            ));
        }
    }
    let host = benchmarked
        .first()
        .map(|r| r.json["host"].clone())
        .unwrap_or(Json::Null);
    // The shards of the run, each with the host of its first report.
    let mut shard_hosts: Vec<Json> = Vec::new();
    for r in &reports {
        if let Some(name) = r.shard()
            && !shard_hosts.iter().any(|s| s["name"] == json!(name))
        {
            shard_hosts.push(json!({ "name": name, "host": r.json["host"] }));
        }
    }

    // The lists come from the one `process` report that ran its budgets;
    // the shards' other `process` reports (there are none in the shard
    // table) would have them `sharded`.
    let process = reports
        .iter()
        .find(|r| r.target == Target::Process && r.ran_everything());
    let plugins = process.map_or_else(Vec::new, |r| strings(&r.json["plugins"]));
    let language_servers = process.map_or_else(Vec::new, |r| strings(&r.json["language_servers"]));

    // Every result, by ID, with the report it came from.
    let mut found: BTreeMap<String, Vec<(usize, Json)>> = BTreeMap::new();
    for (index, r) in reports.iter().enumerate() {
        for error in strings(&r.json["errors"]) {
            errors.push(format!("{}：{error}", r.label));
        }
        for result in r.json["results"].as_array().into_iter().flatten() {
            let id = result["id"].as_str().unwrap_or("").to_owned();
            found.entry(id).or_default().push((index, result.clone()));
        }
    }
    let sharded_run = reports.iter().any(|r| r.shard().is_some());

    let mut results = Vec::new();
    // On the judging machine at least one of its entries must have run: a
    // filter or plan that leaves none judges nothing (ADR 0052).
    let mut judged_here = false;
    for (id, budget) in expected(budgets, &plugins, &language_servers) {
        let all = found.remove(&id).unwrap_or_default();
        // The shard's other budgets are `sharded` in a report; the one
        // real entry is in the report of the shard that owns it.
        let live: Vec<&(usize, Json)> = all
            .iter()
            .filter(|(_, result)| result["status"].as_str() != Some("sharded"))
            .collect();
        if budget.judging_machine
            && live.iter().any(|(_, result)| {
                matches!(
                    result["status"].as_str(),
                    Some("ok" | "over" | "record" | "error")
                )
            })
        {
            judged_here = true;
        }
        match live.as_slice() {
            [] if all.is_empty() && budget.target == Target::Package && !has_package => {
                results.push(placeholder(&id, budget, "skipped", "未打包"));
            }
            [] => {
                let owner = shards::shard_of(budget)
                    .filter(|_| sharded_run || !all.is_empty())
                    .map(|shard| format!("：缺少分片 {} 的报告", shard.name));
                let owner = owner.unwrap_or_default();
                if all.is_empty() {
                    errors.push(format!(
                        "{id} 不在 {} 里{owner}",
                        report::file_name(budget.target)
                    ));
                    results.push(placeholder(&id, budget, "error", "报告里缺少这一项"));
                } else {
                    errors.push(format!("{id} 在全部报告里都是 sharded{owner}"));
                    results.push(placeholder(
                        &id,
                        budget,
                        "error",
                        "在全部报告里都是 sharded",
                    ));
                }
            }
            [(index, result)] if reports[*index].target == budget.target => {
                results.push(result.clone());
            }
            [(index, result)] => {
                errors.push(format!(
                    "{id} 出现在 {}，应在 {}",
                    reports[*index].label,
                    report::file_name(budget.target)
                ));
                results.push(result.clone());
            }
            [(_, result), ..] => {
                let files: Vec<&str> = live
                    .iter()
                    .map(|(index, _)| reports[*index].label.as_str())
                    .collect();
                errors.push(format!("{id} 出现了多次：{}", files.join("、")));
                results.push(result.clone());
            }
        }
    }
    for (id, occurrences) in found {
        for (index, result) in occurrences {
            if result["status"].as_str() == Some("sharded") {
                continue;
            }
            errors.push(format!(
                "{} 里的 {id} 不在预算表里（或插件、语言服务器列表里没有它）",
                reports[index].label
            ));
        }
    }
    if judge && !judged_here {
        errors.push(NOTHING_JUDGED.to_owned());
    }

    let exit = if !errors.is_empty() || results.iter().any(|r| r["status"] == "error") {
        2
    } else if gated && results.iter().any(|r| r["status"] == "over") {
        1
    } else {
        0
    };
    // Every benchmark's wall time, slowest first, with its target (and its
    // shard).
    let mut benches: Vec<Json> = Vec::new();
    for r in &reports {
        for bench in r.json["benches"].as_array().into_iter().flatten() {
            let mut bench = bench.clone();
            bench["target"] = json!(r.target.name());
            if let Some(shard) = r.shard() {
                bench["shard"] = json!(shard);
            }
            benches.push(bench);
        }
    }
    let seconds = |b: &Json| b["seconds"].as_f64().unwrap_or(0.0);
    benches.sort_by(|a, b| seconds(b).total_cmp(&seconds(a)));

    let title = report::title("report", &mode, gated, judge);
    let mut text = report::text_table(&title, &results, &errors);
    let mut markdown = report::markdown(&title, &results, &errors);
    if let Some(lines) = slowest(&json!({ "benches": benches })) {
        text.push_str("\nslowest benchmarks:\n");
        markdown.push_str("\nSlowest benchmarks:\n\n");
        for line in lines {
            text.push_str(&format!("  {line}\n"));
            markdown.push_str(&format!("- {line}\n"));
        }
    }
    let mut out = Map::new();
    out.insert("benches".into(), Json::Array(benches));
    out.insert("version".into(), json!(VERSION));
    out.insert("mode".into(), json!(mode));
    out.insert("gated".into(), json!(gated));
    out.insert("judge".into(), json!(judge));
    out.insert("host".into(), host);
    if !shard_hosts.is_empty() {
        out.insert("shards".into(), Json::Array(shard_hosts));
    }
    out.insert("commit".into(), json!(commit));
    out.insert("results".into(), Json::Array(results));
    out.insert("errors".into(), json!(errors));
    Merged {
        report: report::sorted(Json::Object(out)),
        text,
        markdown,
        exit,
    }
}
