//! `limits_*`, `statusline_*` (docs/design/usage.md「额度快照」,
//! 「外部格式」): Codex samples from `RateLimits` records, Claude Code
//! samples from the statusline file, the snapshot built from them and the
//! plugin's reading of the file. The statusline files in
//! `tests/fixtures/statusline/` are synthetic (`SOURCE.md`); nothing here
//! reads a machine's Claude Code configuration.

use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use lectern_fs_watch::Flags;
use lectern_services::session::{Agent, RateLimitSample, RateWindow, Record, Timestamp};
use lectern_services::usage::{LimitKind, LimitSource, LimitWindow, Limits, UsageEvent};

use super::plugin::Rig;
use super::support::*;
use crate::limits::{self, Rejected};
use crate::store::limits::Sample;
use crate::testing::FakeSessions;

const COMPLETE: &str = include_str!("../../tests/fixtures/statusline/complete.json");
const FIVE_HOUR_ONLY: &str = include_str!("../../tests/fixtures/statusline/five-hour-only.json");
const SEVEN_DAY_ONLY: &str = include_str!("../../tests/fixtures/statusline/seven-day-only.json");
const NO_RATE_LIMITS: &str = include_str!("../../tests/fixtures/statusline/no-rate-limits.json");
const OUT_OF_RANGE: &str = include_str!("../../tests/fixtures/statusline/out-of-range.json");
const WRONG_TYPES: &str = include_str!("../../tests/fixtures/statusline/wrong-types.json");
const NOT_JSON: &str = include_str!("../../tests/fixtures/statusline/not-json.txt");
const ARRAY: &str = include_str!("../../tests/fixtures/statusline/array.json");

/// The `resets_at` of the fixtures' windows, in milliseconds.
const FIVE_HOUR_RESET: i64 = 1_792_680_000_000;
const SEVEN_DAY_RESET: i64 = 1_793_059_200_000;

/// The rig's now: 2026-10-22T12:00Z.
const NOW: i64 = OCT_20 + 2 * DAY + 12 * HOUR;

fn sampled(ms: i64) -> Timestamp {
    Timestamp(ms)
}

fn window(percent: f32, minutes: u32, reset_s: i64) -> RateWindow {
    RateWindow {
        used_percent: percent,
        window_minutes: Some(minutes),
        resets_at: Some(Timestamp(reset_s * 1000)),
    }
}

fn codex_sample(
    at: i64,
    limit_id: &str,
    primary: Option<RateWindow>,
    secondary: Option<RateWindow>,
) -> Record {
    Record::RateLimits(RateLimitSample {
        at: Timestamp(at),
        limit_id: Some(limit_id.into()),
        plan_type: None,
        primary,
        secondary,
    })
}

fn codex(id: &str, records: Vec<Record>) -> FakeSessions {
    let fake = FakeSessions::new("session-codex", Agent::Codex);
    fake.add(id, Path::new("/work/app"), OCT_20, None);
    let mut script = vec![context(0, OCT_20, "/work/app")];
    for (i, record) in records.into_iter().enumerate() {
        script.push(rec(100 * (i as u64 + 1), record));
    }
    fake.push(id, script);
    fake
}

fn stored(h: &Harness) -> Vec<Sample> {
    h.store().samples().unwrap()
}

fn statusline(json: &str) -> Result<Vec<Sample>, Rejected> {
    limits::parse_statusline(json.as_bytes(), sampled(OCT_20))
}

fn percents(samples: &[Sample]) -> Vec<(LimitKind, f32)> {
    samples.iter().map(|s| (s.kind, s.used_percent)).collect()
}

// --- statusline_* ----------------------------------------------------------

#[test]
fn statusline_reads_both_windows_and_ignores_the_rest() {
    let samples = statusline(COMPLETE).unwrap();
    assert_eq!(
        samples,
        [
            Sample {
                agent: Agent::ClaudeCode,
                source: LimitSource::Statusline,
                limit_id: None,
                kind: LimitKind::FiveHour,
                used_percent: 37.5,
                window_minutes: Some(300),
                resets_at: Some(Timestamp(FIVE_HOUR_RESET)),
                sampled_at: sampled(OCT_20),
            },
            Sample {
                agent: Agent::ClaudeCode,
                source: LimitSource::Statusline,
                limit_id: None,
                kind: LimitKind::SevenDay,
                used_percent: 62.25,
                window_minutes: Some(10_080),
                resets_at: Some(Timestamp(SEVEN_DAY_RESET)),
                sampled_at: sampled(OCT_20),
            },
        ],
        "the spend limit, the model and the rest are not read"
    );
}

#[test]
fn statusline_one_window_is_enough() {
    let five = statusline(FIVE_HOUR_ONLY).unwrap();
    assert_eq!(percents(&five), [(LimitKind::FiveHour, 4.0)]);
    let seven = statusline(SEVEN_DAY_ONLY).unwrap();
    assert_eq!(percents(&seven), [(LimitKind::SevenDay, 71.0)]);
}

#[test]
fn statusline_without_windows_is_rejected() {
    assert_eq!(statusline(NO_RATE_LIMITS), Err(Rejected::NoWindows));
    assert_eq!(statusline("{}"), Err(Rejected::NoWindows));
    assert_eq!(
        statusline(r#"{"rate_limits": {"spend_limit": {"used_percentage": 3}}}"#),
        Err(Rejected::NoWindows),
        "only the spend limit"
    );
    assert_eq!(
        statusline(r#"{"rate_limits": null}"#),
        Err(Rejected::NoWindows)
    );
}

#[test]
fn statusline_not_json_is_rejected() {
    assert_eq!(statusline(NOT_JSON), Err(Rejected::NotJson));
    assert_eq!(statusline(ARRAY), Err(Rejected::NotJson));
    assert_eq!(statusline(""), Err(Rejected::NotJson));
}

#[test]
fn statusline_leaves_out_a_window_with_an_unusable_percentage() {
    assert_eq!(statusline(OUT_OF_RANGE), Err(Rejected::NoWindows));
    // A percentage that is a string: that window goes; a reset time that
    // is a string: the window stays, without a reset time.
    let samples = statusline(WRONG_TYPES).unwrap();
    assert_eq!(percents(&samples), [(LimitKind::SevenDay, 50.0)]);
    assert_eq!(samples[0].resets_at, None);
    // The bounds themselves are fine.
    let edge = r#"{"rate_limits": {"five_hour": {"used_percentage": 0, "resets_at": 1},
                                   "seven_day": {"used_percentage": 100.0, "resets_at": 1}}}"#;
    assert_eq!(
        percents(&statusline(edge).unwrap()),
        [(LimitKind::FiveHour, 0.0), (LimitKind::SevenDay, 100.0)]
    );
}

/// A scratch directory with `bytes` in `claude-code.json`; `modified` sets
/// the file's modification time.
fn file_with(bytes: &[u8], modified: Option<i64>) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(limits::STATUSLINE_FILE);
    std::fs::write(&path, bytes).unwrap();
    if let Some(ms) = modified {
        let at = UNIX_EPOCH + Duration::from_millis(ms as u64);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(at)
            .unwrap();
    }
    (dir, path)
}

#[test]
fn statusline_sampling_time_is_the_modification_time() {
    let (_dir, path) = file_with(COMPLETE.as_bytes(), Some(OCT_20 + HOUR));
    let samples = limits::read_statusline(&path, Timestamp(0)).unwrap();
    assert!(
        samples
            .iter()
            .all(|s| s.sampled_at == sampled(OCT_20 + HOUR))
    );
}

#[test]
fn statusline_file_over_one_mib_is_ignored() {
    // Valid JSON, padded with whitespace to exactly 1 MiB: read. One byte
    // more: not read.
    let mut bytes = COMPLETE.as_bytes().to_vec();
    bytes.resize(limits::MAX_BYTES as usize, b' ');
    let (_dir, path) = file_with(&bytes, None);
    assert_eq!(
        limits::read_statusline(&path, Timestamp(5)).unwrap().len(),
        2
    );
    bytes.push(b' ');
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(
        limits::read_statusline(&path, Timestamp(5)),
        Err(Rejected::TooLarge)
    );
}

#[test]
fn statusline_missing_file_is_not_an_error_of_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path: PathBuf = dir.path().join(limits::STATUSLINE_FILE);
    assert_eq!(
        limits::read_statusline(&path, Timestamp(0)),
        Err(Rejected::Missing)
    );
    // A directory where the file should be cannot be read.
    std::fs::create_dir(&path).unwrap();
    assert_eq!(
        limits::read_statusline(&path, Timestamp(0)),
        Err(Rejected::Unreadable)
    );
}

// --- limits_* ----------------------------------------------------------------

#[test]
fn limits_codex_log_keeps_the_newest_sample_of_each_window() {
    let h = Harness::new();
    // Written out of order across two sessions: the later `at` wins, not
    // the later arrival.
    let late = codex(
        "late",
        vec![codex_sample(
            OCT_20 + 3 * HOUR,
            "codex",
            Some(window(30.0, 300, 1_792_700_000)),
            Some(window(55.0, 10_080, 1_793_059_200)),
        )],
    );
    let early = codex(
        "early",
        vec![codex_sample(
            OCT_20 + HOUR,
            "codex",
            Some(window(10.0, 300, 1_792_600_000)),
            None,
        )],
    );
    h.read_all(&late, "late");
    h.read_all(&early, "early");
    let samples = stored(&h);
    assert_eq!(
        percents(&samples),
        [(LimitKind::Primary, 30.0), (LimitKind::Secondary, 55.0)]
    );
    assert!(samples.iter().all(|s| s.agent == Agent::Codex
        && s.source == LimitSource::SessionLog
        && s.limit_id.as_deref() == Some("codex")));
    assert_eq!(samples[0].window_minutes, Some(300));
    assert_eq!(samples[0].resets_at, Some(Timestamp(1_792_700_000_000)));
    assert_eq!(samples[0].sampled_at, Timestamp(OCT_20 + 3 * HOUR));
}

#[test]
fn limits_codex_samples_of_other_limit_ids_are_kept_apart() {
    let h = Harness::new();
    let fake = codex(
        "s",
        vec![
            codex_sample(
                OCT_20 + HOUR,
                "codex",
                Some(window(20.0, 300, 1_792_700_000)),
                None,
            ),
            codex_sample(
                OCT_20 + 2 * HOUR,
                "other",
                Some(window(70.0, 300, 1_792_700_000)),
                None,
            ),
            Record::RateLimits(RateLimitSample {
                at: Timestamp(OCT_20 + 3 * HOUR),
                limit_id: None,
                plan_type: None,
                primary: Some(window(5.0, 300, 1_792_700_000)),
                secondary: None,
            }),
        ],
    );
    h.read_all(&fake, "s");
    let samples = stored(&h);
    let ids: Vec<(Option<&str>, f32)> = samples
        .iter()
        .map(|s| (s.limit_id.as_deref(), s.used_percent))
        .collect();
    assert_eq!(
        ids,
        [(None, 5.0), (Some("codex"), 20.0), (Some("other"), 70.0)]
    );
}

#[test]
fn limits_samples_survive_a_restart_and_a_replayed_log() {
    let mut h = Harness::new();
    let fake = codex(
        "s",
        vec![codex_sample(
            OCT_20 + HOUR,
            "codex",
            Some(window(20.0, 300, 1_792_700_000)),
            None,
        )],
    );
    h.read_all(&fake, "s");
    let before = stored(&h);
    h.reopen(true);
    assert_eq!(stored(&h), before);
    // The log is rewritten from the start with an older sample: the stored
    // newer one stays.
    fake.replace(
        "s",
        vec![
            context(0, OCT_20, "/work/app"),
            rec(
                100,
                codex_sample(OCT_20, "codex", Some(window(1.0, 300, 1_792_600_000)), None),
            ),
        ],
    );
    h.read_all(&fake, "s");
    assert_eq!(stored(&h), before);
}

#[test]
fn limits_reading_reports_whether_a_sample_changed() {
    let h = Harness::new();
    let fake = codex(
        "s",
        vec![codex_sample(
            OCT_20 + HOUR,
            "codex",
            Some(window(20.0, 300, 1_792_700_000)),
            None,
        )],
    );
    assert!(h.read_once(&fake, "s", 1 << 20).limits_changed);
    // The same sample again (a replay): nothing changed.
    fake.replace(
        "s",
        vec![
            context(0, OCT_20, "/work/app"),
            rec(
                100,
                codex_sample(
                    OCT_20 + HOUR,
                    "codex",
                    Some(window(20.0, 300, 1_792_700_000)),
                    None,
                ),
            ),
        ],
    );
    assert!(!h.read_once(&fake, "s", 1 << 20).limits_changed);
}

#[test]
fn limits_statusline_sample_is_replaced_by_a_newer_one_only() {
    let h = Harness::new();
    let at = |ms| limits::parse_statusline(FIVE_HOUR_ONLY.as_bytes(), sampled(ms)).unwrap();
    assert!(h.store().put_samples(&at(OCT_20 + 2 * HOUR)).unwrap());
    // An older file (the file was replaced by an older copy): ignored.
    assert!(!h.store().put_samples(&at(OCT_20 + HOUR)).unwrap());
    assert_eq!(stored(&h)[0].sampled_at, sampled(OCT_20 + 2 * HOUR));
    // The same bytes at the same time: no change.
    assert!(!h.store().put_samples(&at(OCT_20 + 2 * HOUR)).unwrap());
    // A newer sample with another percentage replaces it.
    let newer = limits::parse_statusline(
        br#"{"rate_limits": {"five_hour": {"used_percentage": 9, "resets_at": 1792680000}}}"#,
        sampled(OCT_20 + 3 * HOUR),
    )
    .unwrap();
    assert!(h.store().put_samples(&newer).unwrap());
    assert_eq!(percents(&stored(&h)), [(LimitKind::FiveHour, 9.0)]);
}

fn sample(
    agent: Agent,
    source: LimitSource,
    kind: LimitKind,
    percent: f32,
    resets: Option<i64>,
) -> Sample {
    Sample {
        agent,
        source,
        limit_id: None,
        kind,
        used_percent: percent,
        window_minutes: None,
        resets_at: resets.map(Timestamp),
        sampled_at: sampled(NOW - HOUR),
    }
}

#[test]
fn limits_tightest_is_the_highest_window_that_has_not_expired() {
    let samples = [
        sample(
            Agent::ClaudeCode,
            LimitSource::Statusline,
            LimitKind::FiveHour,
            40.0,
            Some(NOW + HOUR),
        ),
        sample(
            Agent::ClaudeCode,
            LimitSource::Statusline,
            LimitKind::SevenDay,
            90.0,
            Some(NOW - 1),
        ),
        sample(
            Agent::Codex,
            LimitSource::SessionLog,
            LimitKind::Primary,
            75.0,
            Some(NOW + DAY),
        ),
        sample(
            Agent::Codex,
            LimitSource::SessionLog,
            LimitKind::Secondary,
            75.0,
            None,
        ),
    ];
    let snapshot = limits::build(&samples, NOW, 7);
    assert_eq!(snapshot.generation, 7);
    let expired: Vec<bool> = snapshot.windows.iter().map(|w| w.expired).collect();
    assert_eq!(expired, [false, true, false, false]);
    // 90 % has expired; 75 % twice: the first of equals.
    assert_eq!(snapshot.tightest, Some(2));
    // A window expires exactly at its reset time.
    let at_reset = limits::build(&samples, NOW + HOUR, 8);
    assert!(at_reset.windows[0].expired);
    assert_eq!(at_reset.tightest, Some(2));
}

#[test]
fn limits_everything_expired_or_nothing_has_no_tightest() {
    assert_eq!(
        limits::build(&[], NOW, 1),
        Limits {
            generation: 1,
            ..Limits::default()
        }
    );
    let only = [sample(
        Agent::Codex,
        LimitSource::SessionLog,
        LimitKind::Primary,
        99.0,
        Some(NOW - DAY),
    )];
    let snapshot = limits::build(&only, NOW, 2);
    assert_eq!(snapshot.windows.len(), 1);
    assert!(snapshot.windows[0].expired);
    assert_eq!(snapshot.tightest, None);
}

#[test]
fn limits_next_reset_is_the_earliest_future_one() {
    let samples = [
        sample(
            Agent::Codex,
            LimitSource::SessionLog,
            LimitKind::Primary,
            1.0,
            Some(NOW - 5),
        ),
        sample(
            Agent::Codex,
            LimitSource::SessionLog,
            LimitKind::Secondary,
            1.0,
            Some(NOW + 30),
        ),
        sample(
            Agent::ClaudeCode,
            LimitSource::Statusline,
            LimitKind::FiveHour,
            1.0,
            Some(NOW + 10),
        ),
        sample(
            Agent::ClaudeCode,
            LimitSource::Statusline,
            LimitKind::SevenDay,
            1.0,
            None,
        ),
    ];
    assert_eq!(limits::next_reset(&samples, NOW), Some(NOW + 10));
    assert_eq!(limits::next_reset(&samples, NOW + 30), None);
}

// --- the plugin ---------------------------------------------------------------

/// Everything the rig's watcher reports takes the coalescing window; the
/// fake clock of the watcher moves past it.
const WATCH_WINDOW: Duration = Duration::from_secs(5);

fn data_dir(rig: &Rig) -> PathBuf {
    rig.dir.path().join("usage")
}

fn statusline_dir(rig: &Rig) -> PathBuf {
    data_dir(rig).join("statusline")
}

/// Writes the statusline file with this modification time and tells the
/// watcher, as the script's rename would. The directory is made first when
/// it is not there (the root appears).
pub(super) fn write_statusline(rig: &Rig, json: &str, modified: i64) {
    let dir = statusline_dir(rig);
    let appeared = !dir.exists();
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(limits::STATUSLINE_FILE);
    std::fs::write(&path, json).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_millis(modified as u64))
        .unwrap();
    if appeared {
        rig.backend.emit(
            &data_dir(rig),
            &[("statusline", Flags::DIR | Flags::CREATED)],
        );
    } else {
        rig.backend.emit(
            &dir,
            &[(limits::STATUSLINE_FILE, Flags::FILE | Flags::MODIFIED)],
        );
    }
    // The event enters the window first, then the window ends.
    rig.driver.run_until_idle();
    rig.driver.advance(WATCH_WINDOW);
    rig.settle();
}

fn rig() -> Rig {
    Rig::new(
        FakeSessions::new("session-claude-code", Agent::ClaudeCode),
        |_| {},
    )
}

/// The `Limits` events emitted since the last `take`.
struct Events<'a> {
    rig: &'a Rig,
    seen: usize,
}

fn listen(rig: &Rig) -> Events<'_> {
    Events { rig, seen: 0 }
}

impl Events<'_> {
    fn take(&mut self) -> Vec<u64> {
        let all: Vec<u64> = self
            .rig
            .kernel
            .emitted::<UsageEvent>()
            .into_iter()
            .filter_map(|e| match e {
                UsageEvent::Limits { generation } => Some(generation),
                UsageEvent::Changed { .. } => None,
            })
            .collect();
        let new = all[self.seen..].to_vec();
        self.seen = all.len();
        new
    }
}

fn windows(rig: &Rig) -> Vec<LimitWindow> {
    rig.usage().limits().windows.to_vec()
}

#[test]
fn statusline_plugin_has_no_limits_until_a_file_appears() {
    let rig = rig();
    rig.settle();
    assert!(rig.usage().limits().windows.is_empty());
    assert_eq!(rig.usage().limits().tightest, None);
    // The watch is on the ancestor of the missing directory.
    assert!(rig.backend.is_armed(&data_dir(&rig)));
}

#[test]
fn statusline_file_appearing_updates_the_limits_and_emits_once() {
    let rig = rig();
    let mut events = listen(&rig);
    rig.settle();
    write_statusline(&rig, COMPLETE, NOW - HOUR);
    let limits = rig.usage().limits();
    assert_eq!(limits.windows.len(), 2);
    assert_eq!(limits.windows[0].kind, LimitKind::FiveHour);
    assert_eq!(limits.windows[0].source, LimitSource::Statusline);
    assert_eq!(limits.windows[0].agent, Agent::ClaudeCode);
    assert_eq!(limits.windows[0].used_percent, 37.5);
    assert_eq!(limits.windows[0].sampled_at, sampled(NOW - HOUR));
    assert_eq!(limits.tightest, Some(1), "62.25 % of the seven days");
    assert_eq!(events.take(), [limits.generation]);
}

#[test]
fn statusline_file_replaced_updates_and_an_equal_file_does_not_emit() {
    let rig = rig();
    let mut events = listen(&rig);
    rig.settle();
    write_statusline(&rig, COMPLETE, NOW - 2 * HOUR);
    events.take();
    // The same content an hour later: the sampling time is part of the
    // window, so the snapshot changes.
    write_statusline(&rig, COMPLETE, NOW - HOUR);
    assert_eq!(events.take().len(), 1);
    // A write that changes neither content nor time: no event.
    write_statusline(&rig, COMPLETE, NOW - HOUR);
    assert!(events.take().is_empty());
    // A different file: one window.
    write_statusline(&rig, SEVEN_DAY_ONLY, NOW);
    let limits = rig.usage().limits();
    assert_eq!(events.take(), [limits.generation]);
    assert_eq!(
        limits.windows.len(),
        2,
        "the five-hour sample is kept until it expires"
    );
    assert_eq!(limits.windows[1].used_percent, 71.0);
    assert_eq!(limits.windows[0].used_percent, 37.5);
}

#[test]
fn statusline_unusable_files_keep_the_last_samples() {
    let rig = rig();
    let mut events = listen(&rig);
    rig.settle();
    write_statusline(&rig, COMPLETE, NOW - HOUR);
    let before = rig.usage().limits();
    events.take();
    for bad in [NOT_JSON, ARRAY, NO_RATE_LIMITS, OUT_OF_RANGE] {
        write_statusline(&rig, bad, NOW);
        assert_eq!(rig.usage().limits(), before);
    }
    // Over 1 MiB.
    let big = format!("{COMPLETE}{}", " ".repeat(limits::MAX_BYTES as usize));
    write_statusline(&rig, &big, NOW);
    assert_eq!(rig.usage().limits(), before);
    assert!(events.take().is_empty());
}

#[test]
fn statusline_file_older_than_the_run_is_read_at_start() {
    let rig = rig();
    rig.settle();
    write_statusline(&rig, COMPLETE, NOW - HOUR);
    let first = rig.usage().limits();
    // Lectern restarts: the stored samples are there before any event, and
    // the file is read again without one.
    let rig = rig.restart(|_| {});
    rig.settle();
    let again = rig.usage().limits();
    assert_eq!(again.windows, first.windows);
    assert_eq!(again.tightest, first.tightest);
}

#[test]
fn statusline_file_written_while_lectern_was_not_running_is_read() {
    let rig = rig();
    rig.settle();
    let dir = statusline_dir(&rig);
    let rig = rig.restart(|_| {
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(limits::STATUSLINE_FILE);
        std::fs::write(&path, FIVE_HOUR_ONLY).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_millis((NOW - HOUR) as u64))
            .unwrap();
    });
    rig.settle();
    assert_eq!(percents_of(&rig), [(LimitKind::FiveHour, 4.0)]);
}

fn percents_of(rig: &Rig) -> Vec<(LimitKind, f32)> {
    windows(rig)
        .iter()
        .map(|w| (w.kind, w.used_percent))
        .collect()
}

#[test]
fn limits_expire_with_the_clock_and_emit() {
    let rig = rig();
    let mut events = listen(&rig);
    rig.settle();
    write_statusline(&rig, COMPLETE, NOW - HOUR);
    events.take();
    assert_eq!(rig.usage().limits().tightest, Some(1));
    // The seven-day window resets at 2026-10-27T00:00Z; the five-hour one
    // at 2026-10-22T14:40Z. Past the first, it is expired and the tightest
    // is still the seven-day window; past the second, nothing is.
    rig.advance(Duration::from_millis((FIVE_HOUR_RESET - NOW) as u64 + 1));
    let limits = rig.usage().limits();
    assert!(limits.windows[0].expired);
    assert!(!limits.windows[1].expired);
    assert_eq!(limits.tightest, Some(1));
    assert_eq!(events.take(), [limits.generation]);
    rig.advance(Duration::from_millis(
        (SEVEN_DAY_RESET - FIVE_HOUR_RESET) as u64 + 1,
    ));
    let limits = rig.usage().limits();
    assert!(limits.windows.iter().all(|w| w.expired));
    assert_eq!(limits.tightest, None);
    assert_eq!(events.take(), [limits.generation]);
}

#[test]
fn limits_codex_and_statusline_share_the_tightest() {
    let fake = FakeSessions::new("session-codex", Agent::Codex);
    fake.add("c", Path::new("/work/app"), NOW - 60_000, None);
    fake.push(
        "c",
        vec![
            context(0, NOW - 60_000, "/work/app"),
            rec(
                100,
                codex_sample(
                    NOW - 60_000,
                    "codex",
                    Some(window(80.0, 300, (NOW / 1000) + 3600)),
                    Some(window(20.0, 10_080, (NOW / 1000) + 86_400)),
                ),
            ),
        ],
    );
    let rig = Rig::new(fake, |_| {});
    let mut events = listen(&rig);
    rig.settle();
    let limits = rig.usage().limits();
    assert_eq!(limits.windows.len(), 2);
    assert_eq!(limits.tightest, Some(0), "Codex primary, 80 %");
    assert!(
        !events.take().is_empty(),
        "the log's samples were announced"
    );
    write_statusline(&rig, COMPLETE, NOW - HOUR);
    let limits = rig.usage().limits();
    // Claude Code's windows sort before Codex's.
    let order: Vec<(Agent, LimitKind)> = limits
        .windows
        .iter()
        .map(|w| (w.agent.clone(), w.kind))
        .collect();
    assert_eq!(
        order,
        [
            (Agent::ClaudeCode, LimitKind::FiveHour),
            (Agent::ClaudeCode, LimitKind::SevenDay),
            (Agent::Codex, LimitKind::Primary),
            (Agent::Codex, LimitKind::Secondary),
        ]
    );
    assert_eq!(limits.tightest, Some(2), "80 % beats 62.25 %");
}

#[test]
fn statusline_stops_with_the_plugin() {
    let rig = rig();
    rig.settle();
    write_statusline(&rig, COMPLETE, NOW - HOUR);
    let dir = statusline_dir(&rig);
    assert!(rig.backend.is_armed(&dir));
    rig.kernel.deactivate();
    rig.driver.run_until_idle();
    assert!(!rig.backend.is_armed(&dir), "the watch was dropped");
}
