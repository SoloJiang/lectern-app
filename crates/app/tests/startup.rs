//! Startup tests (docs/design/app.md「测试」): launch the real `lectern`
//! with a temporary `HOME` and the debug-only `--quit-after-start`, then
//! read the log file it wrote. `CLAUDE_CONFIG_DIR` and `CODEX_HOME` are
//! removed, so the session sources look only under the temporary `HOME`,
//! never at this machine's sessions. Needs a macOS graphical session, which the
//! macOS CI job has. Checks are on the file's contents and the order of
//! records, never on wall-clock durations; the timeout only ends a hang.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

const HANG: Duration = Duration::from_secs(30);

struct Run {
    home: tempfile::TempDir,
}

impl Run {
    fn new() -> Run {
        Run::with_prefix(".tmp")
    }

    /// A `HOME` whose name starts with `prefix`.
    fn with_prefix(prefix: &str) -> Run {
        Run {
            home: tempfile::Builder::new().prefix(prefix).tempdir().unwrap(),
        }
    }

    fn logs(&self) -> PathBuf {
        self.home.path().join("Library/Logs/Lectern")
    }

    /// Launches `lectern` and waits for it to exit.
    fn launch(&self, args: &[&str], lectern_log: Option<&str>) -> ExitStatus {
        let mut command = Command::new(env!("CARGO_BIN_EXE_lectern"));
        command
            .args(args)
            .env("HOME", self.home.path())
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME");
        match lectern_log {
            Some(filter) => command.env("LECTERN_LOG", filter),
            None => command.env_remove("LECTERN_LOG"),
        };
        let mut child = command.spawn().unwrap();
        let start = Instant::now();
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            if start.elapsed() > HANG {
                let _ = child.kill();
                panic!("lectern did not exit within {HANG:?}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The first file of each run, oldest first.
    fn runs(&self) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = fs::read_dir(self.logs())
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                let name = p.file_name().unwrap().to_string_lossy().into_owned();
                // `lectern-<time>-<pid>.log`; rotated files have `.<seq>.log`.
                name.starts_with("lectern-")
                    && name.ends_with(".log")
                    && name.matches('.').count() == 1
            })
            .collect();
        // Names hold the start time to the second and the pid; order by
        // creation instead.
        files.sort_by_key(|p| fs::metadata(p).unwrap().created().unwrap());
        files
    }
}

fn records(file: &Path) -> Vec<Value> {
    fs::read_to_string(file)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}")))
        .collect()
}

fn msg(record: &Value) -> &str {
    record["msg"].as_str().unwrap_or_default()
}

fn position(records: &[Value], wanted: &str) -> usize {
    records
        .iter()
        .position(|r| msg(r) == wanted)
        .unwrap_or_else(|| panic!("no `{wanted}` in {records:#?}"))
}

/// `t` (RFC 3339, UTC, milliseconds) as a `SystemTime`.
fn time(record: &Value) -> SystemTime {
    let t = record["t"].as_str().unwrap();
    let num = |range: std::ops::Range<usize>| t[range].parse::<i64>().unwrap();
    let (y, m, d) = (num(0..4), num(5..7), num(8..10));
    let (hh, mm, ss, ms) = (num(11..13), num(14..16), num(17..19), num(20..23));
    // Days from 1970-01-01 (Howard Hinnant's days_from_civil).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let ms = ((days * 24 + hh) * 60 + mm) * 60_000 + ss * 1000 + ms;
    UNIX_EPOCH + Duration::from_millis(u64::try_from(ms).unwrap())
}

#[test]
fn startup_writes_a_header_and_an_end_record() {
    let run = Run::new();
    assert!(run.launch(&["--quit-after-start"], None).success());
    let runs = run.runs();
    assert_eq!(runs.len(), 1, "{runs:?}");
    let records = records(&runs[0]);
    assert_eq!(msg(&records[0]), "file", "{records:#?}");
    let initialised = position(&records, "ui initialised");
    let drawn = position(&records, "first frame drawn");
    let ready = position(&records, "ui ready");
    let assembled = position(&records, "plugins assembled");
    let released = position(&records, "startup barrier released");
    // `ui::init` before the first frame, `ui::ready` behind the barrier
    // (docs/design/ui.md「对其他 crate 的要求」, ADR 0017).
    assert!(
        initialised < drawn && drawn < ready && ready < assembled && assembled < released,
        "{records:#?}"
    );
    // The system colours are read only in `ready`.
    assert_eq!(records[initialised]["accent"], "builtin", "{records:#?}");
    assert_eq!(records[ready]["accent"], "system", "{records:#?}");
    // project, two session sources, review, usage, workspace, welcome,
    // welcome-discovery, welcome-review, project-home, project-home-usage,
    // review-view, worktrees, worktrees-usage.
    assert_eq!(records[assembled]["plugins"], 14);
    // Nothing of the review screen runs before the first frame
    // (docs/design/review_view.md「验收」第 3 步), nor of the worktrees
    // overview (docs/design/worktrees.md「性能」).
    assert!(
        records[..drawn]
            .iter()
            .all(|r| r["tg"] != "review_view" && r["tg"] != "worktrees"),
        "{records:#?}"
    );
    assert_eq!(records[released]["profile"], "desktop");
    let quit = position(&records, "quit requested");
    assert_eq!(records[quit]["path"], "action");
    assert_eq!(msg(records.last().unwrap()), "run end", "{records:#?}");
}

#[test]
fn log_file_is_created_after_the_first_frame() {
    let run = Run::new();
    assert!(run.launch(&["--quit-after-start"], None).success());
    let file = &run.runs()[0];
    let records = records(file);
    let drawn = time(&records[position(&records, "first frame drawn")]);
    let created = fs::metadata(file).unwrap().created().unwrap();
    // `t` is truncated to the millisecond.
    assert!(
        created + Duration::from_millis(1) >= drawn,
        "created {created:?}, first frame {drawn:?}"
    );
}

#[test]
fn cli_and_env_filters_reach_the_header() {
    for (args, env, expected) in [
        (&["--quit-after-start"][..], Some("debug"), "debug"),
        (
            &["--quit-after-start", "--log", "cache=trace"][..],
            None,
            "cache=trace",
        ),
        (
            &["--quit-after-start", "--log=cache=trace"][..],
            Some("debug"),
            "cache=trace",
        ),
    ] {
        let run = Run::new();
        assert!(run.launch(args, env).success());
        let records = records(&run.runs()[0]);
        assert_eq!(records[0]["filter"], expected, "{args:?} {env:?}");
    }
}

#[test]
fn system_quit_writes_the_end_record() {
    let run = Run::new();
    assert!(run.launch(&["--quit-after-start=system"], None).success());
    let records = records(&run.runs()[0]);
    let quit = position(&records, "quit requested");
    assert_eq!(records[quit]["path"], "system");
    assert_eq!(msg(records.last().unwrap()), "run end", "{records:#?}");
}

#[test]
fn second_run_sees_a_clean_previous_run() {
    let run = Run::new();
    assert!(run.launch(&["--quit-after-start"], None).success());
    assert!(run.launch(&["--quit-after-start"], None).success());
    let runs = run.runs();
    assert_eq!(runs.len(), 2, "{runs:?}");
    let records = records(&runs[1]);
    assert!(
        records
            .iter()
            .all(|r| msg(r) != "previous run ended unexpectedly"),
        "{records:#?}"
    );
}

#[test]
fn quit_shuts_the_services_down_in_time() {
    let run = Run::new();
    assert!(run.launch(&["--quit-after-start"], None).success());
    let records = records(&run.runs()[0]);
    for timed_out in [
        "kernel shutdown timed out",
        "watcher shutdown timed out",
        "executor shutdown timed out",
        "cache close timed out",
    ] {
        assert!(
            records.iter().all(|r| msg(r) != timed_out),
            "{timed_out}: {records:#?}"
        );
    }
    assert_eq!(msg(records.last().unwrap()), "run end", "{records:#?}");
}

#[test]
fn home_path_stays_out_of_the_log() {
    let canary = "canary-5d1b8e27";
    let run = Run::with_prefix(canary);
    assert!(run.launch(&["--quit-after-start"], Some("trace")).success());
    let files = run.runs();
    assert!(!files.is_empty());
    for path in files {
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains(canary), "{}: {text}", path.display());
        // The window state file is never named in the log either.
        assert!(!text.contains("window.json"), "{}: {text}", path.display());
    }
}

/// A `window.json` left by an earlier run: the project path in it and the
/// path of the file stay out of the log, whatever the state restore does
/// with them (docs/design/workspace.md「验收」第 3 步).
#[test]
fn window_state_stays_out_of_the_log() {
    let canary = "canary-c41e7a09";
    for version in [1, 2] {
        let run = Run::new();
        let dir = run
            .home
            .path()
            .join("Library/Application Support/Lectern/workspace");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("window.json"),
            format!("{{\"version\":{version},\"project\":\"/{canary}\",\"inspector\":true}}"),
        )
        .unwrap();
        assert!(run.launch(&["--quit-after-start"], Some("trace")).success());
        for path in run.runs() {
            let text = fs::read_to_string(&path).unwrap();
            assert!(!text.contains(canary), "{}: {text}", path.display());
            assert!(!text.contains("window.json"), "{}: {text}", path.display());
        }
    }
}
