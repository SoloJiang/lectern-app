//! `archive_*` (docs/design/usage.md「冷热分层」): the cutoff in the local
//! zone, whole UTC months, the split of sessions across the cutoff, the
//! crash points of an archive step, the decoding check, replays and late
//! records of archived months, the half archived month, the full rebuild,
//! the space reclaim and `UsageQuery::History`.
//!
//! The harness's now is 2026-10-22 12:00 UTC: the cutoff is 2025-10-23
//! 00:00, so August and September 2025 are archived and October 2025 stays
//! hot (it does not end a day before the cutoff).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use lectern_services::project::ProjectKey;
use lectern_services::session::{Agent, Positioned, Record};
use lectern_services::usage::{
    HistoryUsage, LocalDate, ProjectRef, UsageQuery, UsageSnapshot, Window,
};
use rusqlite::Connection;
use rusqlite::types::Value;

use super::support::*;
use crate::store::cold::{self, Fault, Reclaim, Stepped};
use crate::testing::{FakeSessions, TableZone};
use crate::window;

fn utc(year: i32, month: u8, day: u8, hour: i64) -> i64 {
    window::day_number(LocalDate { year, month, day }) * DAY + hour * HOUR
}

fn month(year: i32, m: u8) -> cold::Month {
    i64::from(year) * 12 + i64::from(m) - 1
}

/// A session's records: a context, then one turn and one usage record per
/// instant. Message IDs are `<prefix>-<i>`.
fn records(cwd: &str, prefix: &str, times: &[i64]) -> Vec<Positioned<Record>> {
    let mut out = vec![context(0, times[0], cwd)];
    for (i, &at) in times.iter().enumerate() {
        let pos = 100 * (2 * i as u64 + 1);
        let tid = format!("{prefix}-t{i}");
        out.push(turn(pos, &tid, at));
        let tokens = [10 + i as u64, i as u64, 3, 1];
        out.push(claude(
            pos + 100,
            at + 1000,
            Some(&tid),
            &format!("{prefix}-{i}"),
            tokens,
        ));
    }
    out
}

fn every(from: i64, to: i64, step_days: i64) -> Vec<i64> {
    (0..)
        .map(|i| from + i * step_days * DAY)
        .take_while(|at| *at <= to)
        .collect()
}

/// The fixture: "long" every 5 days from August 2025 to yesterday (across
/// the cutoff), "old" in September 2025 and "dup", another session with the
/// same keys (counted once), "recent" in the last days.
fn fixture(h: &mut Harness) -> (FakeSessions, ProjectKey) {
    let repo = h.open_repo("/work/app");
    let fake = FakeSessions::new("session-claude-code", Agent::ClaudeCode);
    let sessions: [(&str, &str, &str, Vec<i64>); 4] = [
        (
            "long",
            "/work/app",
            "long",
            every(utc(2025, 8, 20, 9), utc(2026, 10, 21, 9), 5),
        ),
        (
            "old",
            "/work/app/sub",
            "old",
            every(utc(2025, 9, 3, 15), utc(2025, 9, 27, 15), 3),
        ),
        (
            "dup",
            "/work/app/sub",
            "old",
            every(utc(2025, 9, 3, 15), utc(2025, 9, 27, 15), 3),
        ),
        (
            "recent",
            "/work/app",
            "recent",
            every(utc(2026, 10, 20, 8), utc(2026, 10, 22, 8), 1),
        ),
    ];
    for (id, cwd, prefix, times) in sessions {
        fake.add(id, Path::new(cwd), times[0], None);
        fake.push(id, records(cwd, prefix, &times));
    }
    h.read_every(&fake);
    (fake, repo)
}

fn raw(h: &Harness) -> Connection {
    Connection::open(h.db_path()).unwrap()
}

fn history(h: &Harness, repo: &ProjectKey, days: bool) -> HistoryUsage {
    let query = UsageQuery::History {
        project: ProjectRef::Open(repo.clone()),
        from: LocalDate {
            year: 2025,
            month: 8,
            day: 1,
        },
        to: LocalDate {
            year: 2026,
            month: 10,
            day: 22,
        },
        days,
    };
    match h.snapshot(&query) {
        Some(UsageSnapshot::History(u)) => u,
        other => panic!("not a history: {other:?}"),
    }
}

/// The history of `from` to `to`, months only.
fn history_of(h: &Harness, repo: &ProjectKey, from: LocalDate, to: LocalDate) -> HistoryUsage {
    let query = UsageQuery::History {
        project: ProjectRef::Open(repo.clone()),
        from,
        to,
        days: false,
    };
    match h.snapshot(&query) {
        Some(UsageSnapshot::History(u)) => u,
        other => panic!("not a history: {other:?}"),
    }
}

fn render_history(u: &HistoryUsage) -> String {
    let mut out = String::new();
    for ((y, m), t) in u.months.iter() {
        let _ = writeln!(out, "month {y:04}-{m:02}: {}", fmt_tokens(t));
    }
    for (d, t) in u.days.iter().flat_map(|d| d.iter()) {
        if t.total() > 0 {
            let _ = writeln!(out, "day {}: {}", date(*d), fmt_tokens(t));
        }
    }
    out
}

/// Per session, hot facts plus `cold_total`, in millionths.
fn session_totals(h: &Harness) -> BTreeMap<i64, [i64; 4]> {
    let conn = raw(h);
    let mut out: BTreeMap<i64, [i64; 4]> = BTreeMap::new();
    let sql = "SELECT session, sum(input), sum(output), sum(cache_read), sum(cache_write)
                 FROM fact GROUP BY session
               UNION ALL
               SELECT target, input, output, cache_read, cache_write FROM cold_total
                 WHERE kind = 0";
    let mut stmt = conn.prepare(sql).unwrap();
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                [r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?],
            ))
        })
        .unwrap();
    for row in rows {
        let (session, m): (i64, [i64; 4]) = row.unwrap();
        let slot = out.entry(session).or_default();
        for (a, b) in slot.iter_mut().zip(m) {
            *a += b;
        }
    }
    out
}

/// What must not change: the window snapshots, the history and the
/// per-session totals.
#[derive(Debug, PartialEq)]
struct Seen {
    windows: String,
    history: String,
    sessions: BTreeMap<i64, [i64; 4]>,
}

fn seen(h: &Harness, repo: &ProjectKey) -> Seen {
    let mut windows = String::new();
    for w in [Window::D7, Window::D14, Window::D30] {
        windows.push_str(&render_project(&h.project(repo, w)));
    }
    windows.push_str(&render_all(&h.all(Window::D30)));
    Seen {
        windows,
        history: render_history(&history(h, repo, true)),
        sessions: session_totals(h),
    }
}

/// The rows of the archived tables in August and September 2025, sorted.
fn old_rows(h: &Harness) -> Vec<String> {
    let end = utc(2025, 10, 1, 0);
    let conn = raw(h);
    let mut out = Vec::new();
    for (table, cond) in [
        ("usage_source", format!("at < {end}")),
        ("turn", format!("started_at < {end}")),
        (
            "usage_value",
            format!("source IN (SELECT source FROM fact WHERE at < {end})"),
        ),
        ("fact", format!("at < {end}")),
    ] {
        let mut stmt = conn
            .prepare(&format!("SELECT * FROM {table} WHERE {cond}"))
            .unwrap();
        let n = stmt.column_count();
        let rows = stmt
            .query_map([], |r| {
                (0..n)
                    .map(|i| r.get::<_, Value>(i))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap();
        for row in rows {
            out.push(format!("{table} {:?}", row.unwrap()));
        }
    }
    out.sort();
    out
}

/// The cold rows of the same tables, sorted.
fn cold_rows(h: &Harness) -> Vec<String> {
    let mut out = Vec::new();
    for table in ["usage_source", "turn", "usage_value", "fact"] {
        for row in h.store().cold_rows(table) {
            out.push(format!("{table} {row:?}"));
        }
    }
    out.sort();
    out
}

fn count(h: &Harness, sql: &str) -> i64 {
    raw(h).query_row(sql, [], |r| r.get(0)).unwrap()
}

fn archive(h: &Harness) -> cold::Archived {
    h.store().archive_all(h.now, &h.zone).unwrap()
}

#[test]
fn archive_cutoff_follows_the_local_zone() {
    // One record on 15 October 2025 and one on 1 November 2025, 01:00 UTC.
    let cases: [(&str, TableZone, bool); 4] = [
        ("utc", TableZone::fixed(0), true),
        // The cutoff's local midnight is 10:00 UTC the day before: October
        // does not end a day before it.
        ("east 14", TableZone::fixed(14 * 3600), false),
        ("west 12", TableZone::fixed(-12 * 3600), true),
        // New York: the cutoff day, 2 November 2025, is the day clocks go
        // back; its midnight is still 04:00 UTC.
        (
            "new york",
            TableZone::new(&[
                (i64::MIN, -4 * 3600),
                (utc(2025, 11, 2, 6), -5 * 3600),
                (utc(2026, 3, 8, 7), -4 * 3600),
                (utc(2026, 11, 1, 6), -5 * 3600),
            ]),
            true,
        ),
    ];
    for (case, zone, october) in cases {
        let h = Harness::new();
        let fake = FakeSessions::new("session-claude-code", Agent::ClaudeCode);
        let times = [utc(2025, 10, 15, 12), utc(2025, 11, 1, 1)];
        fake.add("s", Path::new("/work/app"), times[0], None);
        fake.push("s", records("/work/app", "s", &times));
        h.read_every(&fake);
        // Local noon of 1 November 2026.
        let offset = i64::from(crate::LocalZone::offset_at(&zone, utc(2026, 11, 1, 12))) * 1000;
        let now = utc(2026, 11, 1, 12) - offset;
        let cutoff = cold::cutoff(now, &zone);
        assert_eq!(
            cutoff,
            window::last_days(365, now, &zone)[0].1.start,
            "{case}"
        );
        if case == "new york" {
            assert_eq!(cutoff, utc(2025, 11, 2, 4), "{case}");
        }
        let plan = h.store().archive_plan(now, &zone).unwrap();
        assert_eq!(plan.contains(&month(2025, 10)), october, "{case}: {plan:?}");
        assert!(
            !plan.contains(&month(2025, 11)),
            "{case}: only whole months"
        );
    }
}

#[test]
fn archive_moves_old_months_and_keeps_every_total() {
    let mut h = Harness::new();
    let (_fake, repo) = fixture(&mut h);
    let before = seen(&h, &repo);
    let rows = old_rows(&h);
    let plan = h.store().archive_plan(h.now, &h.zone).unwrap();
    assert_eq!(plan, [month(2025, 8), month(2025, 9)]);
    let done = archive(&h);
    assert_eq!(done.months, 2);
    assert!(done.packed_bytes < done.raw_bytes, "{done:?}");
    // Hot rows of August and September are gone, cold ones are the same
    // rows; October 2025 stays hot.
    assert!(old_rows(&h).is_empty());
    assert_eq!(cold_rows(&h), rows);
    let october = utc(2025, 10, 1, 0);
    let hot_october = count(
        &h,
        &format!("SELECT count(*) FROM usage_source WHERE at >= {october}"),
    );
    assert!(hot_october > 0);
    assert_eq!(h.store().month_state(month(2025, 8)), Some(2));
    assert_eq!(h.store().month_state(month(2025, 10)), None);
    // The windows read no cold segment; neither do the months of the
    // history. Its days decode the segments of the range.
    let reads = h.store().cold_reads();
    let mut windows = String::new();
    for w in [Window::D7, Window::D14, Window::D30] {
        windows.push_str(&render_project(&h.project(&repo, w)));
    }
    windows.push_str(&render_all(&h.all(Window::D30)));
    let months_only = history(&h, &repo, false);
    assert_eq!(
        h.store().cold_reads(),
        reads,
        "windows and months read no segment"
    );
    assert!(months_only.days.is_none());
    let after = seen(&h, &repo);
    assert!(h.store().cold_reads() > reads);
    // The sessions across the cutoff split: hot plus cold_total is the
    // total before.
    assert_eq!(after, before);
    assert_eq!(windows, before.windows);
    // August and September from `cold_month`, the rest from hot facts.
    insta::assert_snapshot!(render_history(&months_only));
    // A range inside a month gives the whole month's row, cold (September
    // 2025) and hot (October 2026) alike.
    let date = |year, month, day| LocalDate { year, month, day };
    let whole = |ym: (i32, u8)| {
        months_only
            .months
            .iter()
            .find(|(m, _)| *m == ym)
            .map(|(_, t)| *t)
            .unwrap()
    };
    let part = history_of(&h, &repo, date(2025, 9, 15), date(2025, 9, 16));
    assert_eq!(part.months.len(), 1);
    assert_eq!(part.months[0].1, whole((2025, 9)));
    let part = history_of(&h, &repo, date(2026, 10, 5), date(2026, 10, 6));
    assert_eq!(part.months[0].1, whole((2026, 10)));
    assert!(whole((2026, 10)).total() > 0);
    assert_buckets(&h);
    // Nothing more to do; another run archives nothing.
    assert!(h.store().archive_plan(h.now, &h.zone).unwrap().is_empty());
    assert_eq!(archive(&h).segments, 0);
}

/// The steps of archiving the fixture in segments of `SMALL` bytes.
fn steps_in_small_segments() -> usize {
    let mut h = Harness::new();
    fixture(&mut h);
    h.store().set_segment_bytes(SMALL);
    let mut steps = 0;
    for m in h.store().archive_plan(h.now, &h.zone).unwrap() {
        loop {
            steps += 1;
            match h.store().archive_step(m, h.now, &h.zone).unwrap() {
                Stepped::Segment { finished: true, .. } | Stepped::Finished => break,
                Stepped::Mismatch => panic!("mismatch"),
                _ => {}
            }
        }
    }
    steps
}

/// Small segments: several per table and month.
const SMALL: usize = 300;

/// Archives the fixture in small segments, ending the process after
/// `stop` steps, and with `fault` in the next segment step; then a restart
/// finishes the archive.
fn crash_at(stop: usize, fault: Option<Fault>) {
    let case = format!("after {stop} steps, {fault:?}");
    let mut h = Harness::new();
    let (_fake, repo) = fixture(&mut h);
    let before = seen(&h, &repo);
    let rows = old_rows(&h);
    h.store().set_segment_bytes(SMALL);
    let plan = h.store().archive_plan(h.now, &h.zone).unwrap();
    let mut steps = 0;
    let mut month = 0;
    while steps < stop {
        steps += 1;
        match h.store().archive_step(plan[month], h.now, &h.zone).unwrap() {
            Stepped::Segment { finished: true, .. } | Stepped::Finished => month += 1,
            Stepped::Mismatch => panic!("{case}: mismatch"),
            _ => {}
        }
    }
    if let Some(fault) = fault {
        h.store().faults.lock().unwrap().archive = Some(fault);
        // The fault fires in the next step that writes a segment.
        let failed = (0..3).any(|_| h.store().archive_step(plan[month], h.now, &h.zone).is_err());
        assert!(failed, "{case}: no segment step");
    }
    // The process ends; the next run goes on.
    h.reopen(false);
    // Hot and cold together are the rows before, all along.
    let mut now_rows = old_rows(&h);
    now_rows.extend(cold_rows(&h));
    now_rows.sort();
    assert_eq!(now_rows, rows, "{case}: after the crash");
    assert_eq!(seen(&h, &repo), before, "{case}: after the crash");
    h.store().set_segment_bytes(300);
    archive(&h);
    assert!(old_rows(&h).is_empty(), "{case}");
    assert_eq!(cold_rows(&h), rows, "{case}");
    assert_eq!(seen(&h, &repo), before, "{case}");
    assert_eq!(archive(&h).segments, 0, "{case}: archived twice");
}

#[test]
fn archive_survives_an_end_at_every_point_of_a_step() {
    let total = steps_in_small_segments();
    assert!(total > 12, "{total} steps");
    for fault in [Fault::Inserted, Fault::Deleted, Fault::Summarized] {
        // The first segment (usage_source), one in the middle, the last one
        // (facts and their summaries, the month marked archived).
        for stop in [1, total / 2, total - 2] {
            crash_at(stop, Some(fault));
        }
    }
}

#[test]
fn archive_survives_an_end_between_segments() {
    let total = steps_in_small_segments();
    for stop in 1..total {
        crash_at(stop, None);
    }
}

#[test]
fn archive_counts_a_key_once_across_segments() {
    // "copy" has the keys of "early" later in August: "early" counts (the
    // earlier one), goes cold first, and its `usage_value` rows with it.
    // Later segments must not count "copy" in its place.
    let mut h = Harness::new();
    let repo = h.open_repo("/work/app");
    let fake = FakeSessions::new("session-claude-code", Agent::ClaudeCode);
    let early = every(utc(2025, 8, 3, 9), utc(2025, 8, 9, 9), 1);
    let copy: Vec<i64> = early.iter().map(|at| at + 20 * DAY).collect();
    for (id, times) in [("early", &early), ("copy", &copy)] {
        fake.add(id, Path::new("/work/app"), times[0], None);
        fake.push(id, records("/work/app", "early", times));
    }
    h.read_every(&fake);
    let counted = |h: &Harness| count(h, "SELECT count(*) FROM usage_value");
    assert_eq!(counted(&h), early.len() as i64);
    let before = seen(&h, &repo);
    h.store().set_segment_bytes(SMALL);
    let done = archive(&h);
    assert!(done.segments > 4, "{done:?}");
    assert_eq!(seen(&h, &repo), before);
    assert_eq!(h.store().cold_rows("fact").len(), early.len());
}

#[test]
fn archive_check_failure_keeps_the_hot_rows() {
    let mut h = Harness::new();
    let (_fake, repo) = fixture(&mut h);
    let before = seen(&h, &repo);
    let rows = old_rows(&h);
    let august = month(2025, 8);
    assert_eq!(
        h.store().archive_step(august, h.now, &h.zone).unwrap(),
        Stepped::Started
    );
    h.store().faults.lock().unwrap().mismatch = true;
    assert_eq!(
        h.store().archive_step(august, h.now, &h.zone).unwrap(),
        Stepped::Mismatch
    );
    // Nothing deleted, nothing cold; the month stays archiving (frozen)
    // and is not tried again with this segment format.
    assert_eq!(old_rows(&h), rows);
    assert_eq!(count(&h, "SELECT count(*) FROM cold_part"), 0);
    assert_eq!(h.store().month_state(august), Some(1));
    let plan = h.store().archive_plan(h.now, &h.zone).unwrap();
    assert_eq!(plan, [month(2025, 9)]);
    assert_eq!(seen(&h, &repo), before);
}

#[test]
fn archive_replay_does_not_write_archived_months_again() {
    let mut h = Harness::new();
    let (fake, repo) = fixture(&mut h);
    let before = seen(&h, &repo);
    archive(&h);
    // An adapter upgrade replays the sessions across the cutoff and of
    // September.
    for id in ["long", "dup"] {
        let times = match id {
            "long" => every(utc(2025, 8, 20, 9), utc(2026, 10, 21, 9), 5),
            _ => every(utc(2025, 9, 3, 15), utc(2025, 9, 27, 15), 3),
        };
        let cwd = if id == "long" {
            "/work/app"
        } else {
            "/work/app/sub"
        };
        let prefix = if id == "long" { "long" } else { "old" };
        fake.replace(id, records(cwd, prefix, &times));
        let first = h.read_once(&fake, id, 1 << 30);
        assert!(first.replay_started && first.replay_done, "{id}");
        assert!(first.kept_cold > 0, "{id}");
    }
    assert!(
        old_rows(&h).is_empty(),
        "nothing of August or September hot"
    );
    assert_eq!(seen(&h, &repo), before);
    assert_buckets(&h);
}

#[test]
fn archive_late_records_get_their_own_segment() {
    let mut h = Harness::new();
    let (fake, repo) = fixture(&mut h);
    archive(&h);
    let before = history(&h, &repo, false);
    // A first read of an old log: September 2025, never seen.
    let times = every(utc(2025, 9, 10, 10), utc(2025, 9, 12, 10), 1);
    fake.add("late", Path::new("/work/app"), times[0], None);
    fake.push("late", records("/work/app", "late", &times));
    h.read_all(&fake, "late");
    let september = month(2025, 9);
    let range = cold::month_range(september);
    let hot = format!(
        "SELECT count(*) FROM usage_source WHERE at >= {} AND at < {}",
        range.start, range.end
    );
    let facts = format!(
        "SELECT count(*) FROM fact WHERE at >= {} AND at < {}",
        range.start, range.end
    );
    // Written hot, not counted yet: the month is frozen.
    assert_eq!(count(&h, &hot), 3);
    assert_eq!(count(&h, &facts), 0);
    assert_eq!(history(&h, &repo, false), before);
    assert_eq!(h.store().archive_plan(h.now, &h.zone).unwrap(), [september]);
    // The next archive counts them and appends segments.
    let done = archive(&h);
    assert!(done.segments >= 3, "{done:?}");
    assert_eq!(count(&h, &hot), 0);
    let segments = count(
        &h,
        &format!(
            "SELECT count(*) FROM cold_part WHERE month = {september} AND tbl = 'usage_source'"
        ),
    );
    assert_eq!(segments, 2);
    let after = history(&h, &repo, true);
    let sept = |u: &HistoryUsage| {
        u.months
            .iter()
            .find(|(ym, _)| *ym == (2025, 9))
            .map(|(_, t)| t.input)
            .unwrap()
    };
    assert_eq!(sept(&after), sept(&before) + 10 + 11 + 12);
    let late_days: u64 = after
        .days
        .iter()
        .flat_map(|d| d.iter())
        .filter(|(d, _)| (10..=12).contains(&d.day) && d.month == 9 && d.year == 2025)
        .map(|(_, t)| t.input)
        .sum();
    assert!(late_days >= 33);
}

#[test]
fn archive_salvage_keeps_new_source_ids_above_the_cold_ones() {
    let mut h = Harness::new();
    let repo = h.open_repo("/work/app");
    let fake = FakeSessions::new("session-claude-code", Agent::ClaudeCode);
    // The recent session is read first: the archived sources get the
    // highest IDs, and none of them is left hot.
    let sessions = [
        (
            "recent",
            every(utc(2026, 10, 20, 8), utc(2026, 10, 22, 8), 1),
        ),
        ("old", every(utc(2025, 9, 3, 15), utc(2025, 9, 27, 15), 3)),
    ];
    for (id, times) in &sessions {
        fake.add(id, Path::new("/work/app"), times[0], None);
        fake.push(id, records("/work/app", id, times));
        h.read_all(&fake, id);
    }
    archive(&h);
    let hot_top = count(&h, "SELECT max(id) FROM usage_source");
    let cold_top = h
        .store()
        .cold_rows("usage_source")
        .iter()
        .filter_map(|r| match r[0] {
            Value::Integer(id) => Some(id),
            _ => None,
        })
        .max()
        .unwrap();
    assert!(cold_top > hot_top);
    // The copy holds only the hot rows; their IDs alone would let new
    // sources take the cold ones' IDs, at or below September's mark.
    let r = h.store().salvage_now(h.now).unwrap();
    assert_eq!(r.lost, 0);
    let seq = count(
        &h,
        "SELECT seq FROM sqlite_sequence WHERE name = 'usage_source'",
    );
    assert!(seq >= cold_top, "{seq} < {cold_top}");
    // A first read of an old September log after the salvage is counted by
    // the next archive.
    let sept = |u: &HistoryUsage| {
        u.months
            .iter()
            .find(|(ym, _)| *ym == (2025, 9))
            .map(|(_, t)| t.input)
            .unwrap()
    };
    let before = sept(&history(&h, &repo, false));
    let at = utc(2025, 9, 10, 10);
    fake.add("late", Path::new("/work/app"), at, None);
    fake.push("late", records("/work/app", "late", &[at]));
    h.read_all(&fake, "late");
    archive(&h);
    assert_eq!(sept(&history(&h, &repo, false)), before + 10);
}

#[test]
fn archive_half_month_rebuild() {
    let mut h = Harness::new();
    let (fake, repo) = fixture(&mut h);
    let before = seen(&h, &repo);
    let rows = old_rows(&h);
    let fact_rows = count(
        &h,
        &format!(
            "SELECT count(*) FROM fact WHERE at < {}",
            utc(2025, 10, 1, 0)
        ),
    );
    h.store().set_segment_bytes(300);
    let august = month(2025, 8);
    let range = cold::month_range(august);
    let hot_observations = format!(
        "SELECT (SELECT count(*) FROM usage_source WHERE at >= {0} AND at < {1})
              + (SELECT count(*) FROM turn WHERE started_at >= {0} AND started_at < {1})",
        range.start, range.end
    );
    loop {
        h.store().archive_step(august, h.now, &h.zone).unwrap();
        if count(&h, &hot_observations) == 0 {
            break;
        }
    }
    // Only August's facts are still hot (its counted-source rows went with
    // the sources); the process ends.
    let hot_facts = format!(
        "SELECT count(*) FROM fact WHERE at >= {} AND at < {}",
        range.start, range.end
    );
    assert!(count(&h, &hot_facts) > 0);
    h.reopen(false);
    // The hot window is rebuilt, keys are chosen again (replays), before
    // the archive goes on: nothing of August changes.
    h.store().rebuild_hot().unwrap();
    assert_eq!(seen(&h, &repo), before);
    let times = every(utc(2025, 8, 20, 9), utc(2026, 10, 21, 9), 5);
    fake.replace("long", records("/work/app", "long", &times));
    h.read_all(&fake, "long");
    assert_eq!(seen(&h, &repo), before);
    archive(&h);
    assert_eq!(seen(&h, &repo), before);
    // September was rebuilt and replayed meanwhile (new row IDs); August
    // is the same rows. As many rows per table as before, no fact twice.
    assert_eq!(per_table(&cold_rows(&h)), per_table(&rows));
    let cold_facts = h.store().cold_rows("fact");
    assert_eq!(cold_facts.len() as i64, fact_rows, "no fact archived twice");
    let mut ids: Vec<String> = cold_facts.iter().map(|r| format!("{:?}", r[0])).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len() as i64, fact_rows);
    let august_rows = |rows: &[String]| -> Vec<String> {
        rows.iter()
            .filter(|r| r.starts_with("usage_source") && august_source(r, range.start, range.end))
            .cloned()
            .collect()
    };
    assert_eq!(august_rows(&cold_rows(&h)), august_rows(&rows));
}

/// Rows per table of `table [values]` lines.
fn per_table(rows: &[String]) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for r in rows {
        let table = r.split(' ').next().unwrap_or_default().to_owned();
        *out.entry(table).or_default() += 1;
    }
    out
}

/// Whether a `usage_source [..]` line's `at` (the seventh column) is in
/// [start, end).
fn august_source(line: &str, start: i64, end: i64) -> bool {
    let values: Vec<&str> = line.split("Integer(").skip(1).collect();
    // id, session, gen, pos, seq, part, at: the key is a blob, not counted.
    values
        .get(6)
        .and_then(|v| v.split(')').next())
        .and_then(|v| v.parse::<i64>().ok())
        .is_some_and(|at| (start..end).contains(&at))
}

#[test]
fn archive_full_rebuild_gives_the_same_results() {
    let mut h = Harness::new();
    let (_fake, repo) = fixture(&mut h);
    let before = seen(&h, &repo);
    archive(&h);
    let cold = cold_rows(&h);
    // Rebuilding the hot window leaves the cold months alone.
    h.store().rebuild_hot().unwrap();
    assert_eq!(seen(&h, &repo), before);
    assert_eq!(cold_rows(&h), cold);
    // The full rebuild restores every cold month, rebuilds, and the next
    // archive gives the same cold rows again.
    assert_eq!(h.store().unarchive_all().unwrap(), 2);
    assert_eq!(count(&h, "SELECT count(*) FROM cold_part"), 0);
    assert_eq!(count(&h, "SELECT count(*) FROM cold_month"), 0);
    assert_eq!(count(&h, "SELECT count(*) FROM cold_total"), 0);
    assert_eq!(seen(&h, &repo), before);
    assert_buckets(&h);
    archive(&h);
    assert_eq!(seen(&h, &repo), before);
    assert_buckets(&h);
    // As many rows per table (the facts were rebuilt under new row IDs).
    assert_eq!(per_table(&cold_rows(&h)), per_table(&cold));
}

#[test]
fn archive_heavy_days_round_trip() {
    // The bench fixture at a small size, through `testing::Scenario`: two
    // heavy days, then the clock moves 400 days on and they are archived.
    let dir = tempfile::tempdir().unwrap();
    crate::testing::heavy_days(dir.path(), 2).unwrap();
    let mut s = crate::testing::heavy_open(dir.path()).unwrap();
    let all = match s
        .snapshot(&UsageQuery::AllProjects { window: Window::D7 })
        .unwrap()
    {
        Some(UsageSnapshot::All(a)) => a,
        other => panic!("{other:?}"),
    };
    let records = (2 * crate::testing::HEAVY_SESSIONS * crate::testing::HEAVY_TURNS * 3) as u64;
    // Claude Code: 1000 + k input; Codex: 25 000 - 20 000 cached + k.
    assert_eq!(all.projects.len(), crate::testing::HEAVY_REPOS);
    assert_eq!(all.other, lectern_services::usage::Tokens::default());
    let per_turn_claude = 1000 + 1001 + 1002;
    let per_turn_codex = 5000 + 5001 + 5002;
    let turns = (2 * crate::testing::HEAVY_TURNS) as u64;
    assert_eq!(
        all.total.input,
        turns * (40 * per_turn_claude + 10 * per_turn_codex)
    );
    let roots = crate::testing::heavy_roots();
    let repo = ProjectKey::Git {
        common_dir: roots[0].join(".git").into(),
    };
    let from = LocalDate {
        year: 2026,
        month: 10,
        day: 1,
    };
    let to = LocalDate {
        year: 2026,
        month: 10,
        day: 22,
    };
    let before = s.history(&repo, from, to, true).unwrap();
    s.set_now(crate::testing::HEAVY_NOW + 400 * DAY);
    assert_eq!(s.archive_plan().unwrap(), [(2026, 10)]);
    let done = s.archive_all().unwrap();
    assert_eq!(done.months, 1);
    assert!(done.rows as u64 > records, "{done:?}");
    assert_eq!(s.history(&repo, from, to, true).unwrap(), before);
}

#[test]
fn archive_reclaims_free_pages_only_over_the_thresholds() {
    let mut h = Harness::new();
    let repo = h.open_repo("/work/app");
    let fake = FakeSessions::new("session-claude-code", Agent::ClaudeCode);
    let times = every(utc(2025, 8, 1, 0), utc(2025, 9, 29, 0), 1);
    let times: Vec<i64> = times
        .iter()
        .flat_map(|t| (0..40).map(move |i| t + i * 60_000))
        .collect();
    fake.add("bulk", Path::new("/work/app"), times[0], None);
    fake.push("bulk", records("/work/app", "bulk", &times));
    h.read_every(&fake);
    let _ = repo;
    archive(&h);
    let free = count(&h, "PRAGMA freelist_count");
    assert!(free > 20, "free pages: {free}");
    // Under the default 64 MiB nothing is done.
    assert_eq!(h.store().reclaim(&cold::RECLAIM).unwrap(), None);
    assert_eq!(count(&h, "PRAGMA freelist_count"), free);
    // With no byte threshold: at most `pages` per step, until the free
    // pages are no more than a quarter of the file.
    let policy = Reclaim {
        fraction: (1, 4),
        min_bytes: 0,
        pages: 10,
    };
    let mut steps = 0;
    while let Some(n) = h.store().reclaim(&policy).unwrap() {
        assert!(n > 0 && n <= 10, "{n}");
        steps += 1;
        assert!(steps < 10_000);
    }
    assert!(steps > 0);
    let free = count(&h, "PRAGMA freelist_count");
    let pages = count(&h, "PRAGMA page_count");
    assert!(free * 4 <= pages, "{free} of {pages}");
}

/// A session that changes `lib` in September 2025 (archived at the harness's
/// now) and has a turn in the last days.
fn changing_session(h: &mut Harness) -> (std::sync::Arc<FakeSessions>, ProjectKey) {
    use super::attribution::{Log, source};
    h.open_repo("/work/app");
    let lib = h.open_repo("/work/lib");
    let fake = source();
    let old = utc(2025, 9, 10, 15);
    let mut log = Log::new(&fake, "s", "/work/app", old);
    log.turn("a", old).usage("a", "ma", old + 10, 40);
    log.edit("a", "ca", &["/work/lib/a.rs"], old + 20, 2);
    log.spawn("a", "sp", old + 25);
    let recent = utc(2026, 10, 21, 9);
    log.turn("b", recent).usage("b", "mb", recent + 10, 5);
    log.write();
    h.read_every(&fake);
    h.settle_all(&fake, "s", h.now);
    (fake, lib)
}

#[test]
fn archive_moves_calls_items_and_shares_with_their_turns() {
    let mut h = Harness::new();
    let (_fake, lib) = changing_session(&mut h);
    let all = LocalDate {
        year: 2025,
        month: 8,
        day: 1,
    };
    let to = LocalDate {
        year: 2026,
        month: 10,
        day: 22,
    };
    let before = history_of(&h, &lib, all, to);
    let in_month = |conn: &Connection, table: &str, column: &str| -> i64 {
        let from = utc(2025, 9, 1, 0);
        let sql = format!("SELECT count(*) FROM {table} WHERE {column} >= ?1 AND {column} < ?2");
        conn.query_row(&sql, [from, utc(2025, 10, 1, 0)], |r| r.get(0))
            .unwrap()
    };
    let conn = raw(&h);
    for (table, column) in [
        ("call", "started_at"),
        ("call_fact", "at"),
        ("item", "at"),
        ("share", "at"),
    ] {
        assert_eq!(in_month(&conn, table, column), 1, "{table} hot before");
    }
    drop(conn);
    h.store().archive_all(h.now, &h.zone).unwrap();
    let conn = raw(&h);
    for (table, column) in [
        ("call", "started_at"),
        ("call_fact", "at"),
        ("item", "at"),
        ("share", "at"),
    ] {
        assert_eq!(in_month(&conn, table, column), 0, "{table} archived");
        let parts: i64 = conn
            .query_row(
                "SELECT count(*) FROM cold_part WHERE tbl = ?1",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(parts, 1, "{table} has a cold segment");
    }
    let after = history_of(&h, &lib, all, to);
    assert_eq!(after.months, before.months, "every total is the same");
    // The October turn stays hot, with its items and shares.
    let hot: i64 = conn
        .query_row("SELECT count(*) FROM share", [], |r| r.get(0))
        .unwrap();
    assert_eq!(hot, 1);
}

#[test]
fn archive_leaves_an_anchor_for_the_hot_turns_that_follow_a_cold_change() {
    let mut h = Harness::new();
    let (fake, lib) = changing_session(&mut h);
    h.store().archive_all(h.now, &h.zone).unwrap();
    let conn = raw(&h);
    let anchors: i64 = conn
        .query_row("SELECT count(*) FROM cold_anchor WHERE kind = 0", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(anchors, 1);
    drop(conn);
    // A new turn with no change of its own: no hot turn before it has one,
    // the cold one counts.
    let day = utc(2026, 10, 22, 8);
    let mut log = super::attribution::Log::resume(&fake, "s", "/work/app", day);
    log.records.clear();
    log.pos = 10_000;
    log.turn("c", day).usage("c", "mc", day + 10, 7);
    log.write();
    h.read_every(&fake);
    h.settle_all(&fake, "s", h.now);
    assert_eq!(h.project(&lib, Window::D7).total.input, 12, "5 and 7");
    assert_eq!(h.all(Window::D7).other.input, 0);
    assert_buckets(&h);
}
