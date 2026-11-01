//! `log_*`, `store_*` and `archive_*` checks of what step 5 added (docs/design/
//! usage.md「日志与诊断」「数据库」「冷热分层」): nothing of a command or a
//! reflog line in the log, every statement of the new tables on an index,
//! the migration from version 5, the salvage and the archive.

use std::path::Path;

use lectern_fs_watch::{Origin, ReflogFile, WorktreeName};
use lectern_services::usage::{HistoryUsage, LocalDate, ProjectRef, UsageQuery, UsageSnapshot};

use super::attribution::{AT, Log, conn, session_usage, settled, source};
use super::branches::{Repo, watched};
use super::store::{BACK_TO_V5, plan, pragma, raw};
use super::support::*;
use crate::store::schema::{CURRENT, V5};
use crate::store::{BACKUP_PREFIX, Checked};

const SEC: i64 = 1_000;

#[test]
fn log_canary_never_reaches_a_record_of_the_branch_observations() {
    const CANARY: &str = "c4n4ry-5b7d";
    let capture = lectern_logging::testing::capture();
    let branch = format!("refs/heads/feat-{CANARY}");
    let (h, repo) = watched(&branch);
    // A record whose message and branch carry the canary.
    let commit = repo.record(
        ReflogFile::Head(WorktreeName::Main),
        Some(1),
        2,
        AT + 5 * SEC,
        100,
        None,
        &format!("commit: {CANARY}"),
        Origin::Appended,
    );
    h.reflog(
        &repo,
        vec![
            commit,
            repo.switch(
                &WorktreeName::Main,
                &format!("feat-{CANARY}"),
                &format!("other-{CANARY}"),
                AT + 9 * SEC,
            ),
        ],
        vec![repo.removed(&format!("refs/heads/other-{CANARY}"), AT + 20 * SEC)],
    );
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    let command = format!(
        "cd /work/app && git push https://user:{CANARY}@host/x.git HEAD:{CANARY} && git stash"
    );
    log.exec("t1", "c1", &command, "/work/app", AT + 20);
    log.read("t1", "c2", &["/work/app/src/a.rs"], AT + 60);
    log.edit("t1", "c3", &["/work/app/src/a.rs"], AT + 80, 3);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    let _ = session_usage(&h, &fake, "s");
    let logged = capture.lines();
    for expected in ["reflog batch stored", "turns settled"] {
        assert!(
            logged.iter().any(|l| l.contains(expected)),
            "no record says {expected:?}"
        );
    }
    capture.assert_absent(CANARY);
    // Nor is the text of the command in the database: only what it did, and
    // not an address that could carry a token.
    let conn = conn(&h);
    let (rows, with_token): (i64, bool) = conn
        .query_row(
            "SELECT count(*), coalesce(max(facts LIKE ?1), 0) FROM call WHERE git != 0",
            [format!("%{CANARY}@%")],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(rows, 1, "the command that mentions git has a row");
    assert!(!with_token);
    for table in ["call", "call_obs", "turn_obs", "reflog", "head_baseline"] {
        let mut stmt = conn.prepare(&format!("SELECT * FROM {table}")).unwrap();
        let n = stmt.column_count();
        let mut rows = stmt.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            for i in 0..n {
                if let Ok(rusqlite::types::ValueRef::Blob(b) | rusqlite::types::ValueRef::Text(b)) =
                    row.get_ref(i)
                {
                    let text = String::from_utf8_lossy(b);
                    assert!(
                        !text.contains("user:") && !text.contains("git stash"),
                        "{table} keeps command text"
                    );
                }
            }
        }
    }
}

#[test]
fn store_statements_of_the_branch_tables_use_an_index() {
    let h = Harness::new();
    let conn = raw(&h.db_path());
    // Every statement of an ingest, a settle or a query that finds rows of
    // the tables the observations add, with the index it must use.
    let statements: &[(&str, &str)] = &[
        (
            "SELECT id, state FROM call_fact WHERE session = ?1 AND cid = ?2 AND path = ?3",
            "sqlite_autoindex_call_fact_1",
        ),
        (
            "SELECT location, worktree FROM call_fact WHERE session = ?1 AND cid = ?2 AND path = ?3",
            "sqlite_autoindex_call_fact_1",
        ),
        (
            "UPDATE call_fact SET segment = ?2, state = ?3 WHERE id = ?1",
            "INTEGER PRIMARY KEY",
        ),
        (
            "SELECT EXISTS (SELECT 1 FROM turn_obs WHERE session = ?1 AND tid = ?2)",
            "sqlite_autoindex_turn_obs_1",
        ),
        (
            "SELECT o.flags, o.start_seg, o.end_seg, o.stays, o.enter FROM turn t
             JOIN turn_obs o ON o.session = t.session AND o.tid = t.tid WHERE t.id = ?1",
            "sqlite_autoindex_turn_obs_1",
        ),
        (
            "SELECT EXISTS (SELECT 1 FROM item i JOIN call_fact f ON f.id = i.cf
                            WHERE i.turn = ?1 AND f.worktree = ?2)",
            "item_turn",
        ),
        (
            "SELECT act, b, at, obs_ns, aft_ns, old, new FROM reflog
             WHERE repo = ?1 AND file = ?2 AND wt = ?3 AND obs_ns > ?4 ORDER BY run, seq",
            "reflog_file",
        ),
        (
            "SELECT obs_ns FROM reflog_reset WHERE repo = ?1 AND file = ?2 AND wt = ?3
               AND reason != 2 ORDER BY obs_ns",
            "reflog_reset_file",
        ),
        (
            "SELECT begin_ns, end_ns FROM watch_span WHERE repo = ?1 ORDER BY begin_ns",
            "watch_span_repo",
        ),
        (
            "SELECT at_ns, branch FROM head_baseline WHERE repo = ?1 AND wt = ?2 ORDER BY at_ns",
            "head_baseline_wt",
        ),
        (
            "UPDATE watch_span SET end_ns = ?2 WHERE repo = ?1 AND end_ns IS NULL",
            "watch_span_repo",
        ),
        (
            "UPDATE watch_span SET last_ns = max(last_ns, ?2) WHERE repo = ?1 AND end_ns IS NULL",
            "watch_span_repo",
        ),
        (
            "UPDATE watch_span SET end_ns = last_ns WHERE end_ns IS NULL",
            "watch_span_open",
        ),
        (
            "SELECT EXISTS (SELECT 1 FROM watch_span w JOIN location l ON l.id = w.repo
               WHERE l.kind = 0 AND l.path = ?1 AND w.begin_ns <= ?3
                 AND (w.end_ns IS NULL OR w.end_ns >= ?2))",
            "watch_span_repo",
        ),
        (
            "SELECT DISTINCT s.id, s.kind, s.begin_at, s.end_at FROM segment_name n
             JOIN segment s ON s.id = n.segment
             WHERE n.name = ?1 AND s.repo = ?2 AND s.kind IN (0, 2)",
            "segment_name_name",
        ),
        (
            "SELECT s.id, s.begin_at, s.begin_reason FROM segment_name n
             JOIN segment s ON s.id = n.segment
             WHERE n.name = ?1 AND s.repo = ?2 AND s.kind = 0 AND s.end_at IS NULL
               AND s.end_reason IS NULL",
            "segment_name_name",
        ),
        (
            "SELECT name FROM segment_name WHERE segment = ?1 AND (at IS NULL OR at <= ?2)
             ORDER BY coalesce(at, -9223372036854775808) DESC, id DESC LIMIT 1",
            "segment_name_segment",
        ),
        (
            "SELECT id FROM segment WHERE repo = ?1 AND kind = 1 AND wt = ?2
               AND end_reason IS NULL ORDER BY id DESC LIMIT 1",
            "segment_repo",
        ),
        (
            "SELECT s.id FROM segment s JOIN segment_name n ON n.segment = s.id
             WHERE s.repo = ?1 AND s.kind = 2 AND n.name = ?2 LIMIT 1",
            "segment_name_name",
        ),
        (
            "SELECT review, coalesce(sum(input), 0) FROM fact WHERE segment = ?1 GROUP BY review",
            "fact_segment",
        ),
        (
            "SELECT coalesce(segment, 0), kind, coalesce(sum(input), 0) FROM fact
             WHERE session IN (1, 2) GROUP BY segment, kind",
            "fact_session",
        ),
        (
            "SELECT review, input FROM cold_total WHERE kind = ?1 AND target = ?2",
            "sqlite_autoindex_cold_total_1",
        ),
        (
            "SELECT location, segment, kind, weight FROM share WHERE turn = ?1",
            "sqlite_autoindex_share_1",
        ),
        (
            "INSERT OR IGNORE INTO call_obs (session, cid, wt, at, flags, end_seg, stays)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            "",
        ),
    ];
    let big = [
        "reflog",
        "reflog_reset",
        "call_fact",
        "call_obs",
        "turn_obs",
        "segment",
        "segment_name",
        "fact",
        "watch_span",
        "head_baseline",
        "item",
        "share",
    ];
    for (sql, index) in statements {
        let lines = plan(&conn, sql);
        let text = lines.join(" | ");
        for table in big {
            assert!(
                !lines
                    .iter()
                    .any(|l| l.starts_with(&format!("SCAN {table}")) && !l.contains("USING")),
                "scans {table}: {sql}\n{text}"
            );
        }
        assert!(text.contains(index), "does not use {index}: {sql}\n{text}");
    }
    // The weights of a turn, with the files' segments (a join on the key).
    let sql = crate::store::weights_sql("1,2,3");
    let lines = plan(&conn, &sql);
    let text = lines.join(" | ");
    assert!(
        !lines
            .iter()
            .any(|l| l.starts_with("SCAN item") || l.starts_with("SCAN call_fact")),
        "{text}"
    );
}

#[test]
fn store_migration_from_version_5_settles_every_turn_again() {
    let (mut h, repo) = watched("refs/heads/feature");
    let _ = &repo;
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.edit("t1", "c1", &["/work/app/src/a.rs"], AT + 20, 3);
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    let before = h.total();
    let key = fake.key("s");
    h.close(true);
    // A version-5 file: the tables of that version, the same rows.
    let c = raw(&h.db_path());
    c.execute_batch(BACK_TO_V5).unwrap();
    c.execute_batch("PRAGMA user_version = 5;").unwrap();
    drop(c);
    let fresh = tempfile::tempdir().unwrap();
    drop(crate::store::Store::open_with(fresh.path(), &V5, h.now).unwrap());
    let names = |path: &Path| -> String {
        raw(path)
            .query_row(
                "SELECT group_concat(name) FROM (SELECT name FROM sqlite_schema ORDER BY name)",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(names(&h.db_path()), names(&fresh.path().join("usage.db")));
    let reopened = h.open_with(&CURRENT);
    assert_eq!(reopened.migrated_from, Some(5));
    assert_eq!(h.total(), before, "the totals are the same");
    // Every session is read again, every turn settled again.
    assert!(h.store().cursor(&key).unwrap().is_start());
    let main = raw(&h.db_path());
    assert_eq!(pragma(&main, "user_version"), 6);
    assert_eq!(
        main.query_row("SELECT count(*) FROM turn WHERE done = 1", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        main.query_row("SELECT count(*) FROM item", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(main);
    h.read_all(&fake, "s");
    assert_eq!(h.total(), before, "nothing counted twice");
    settled(&h, &fake, "s");
    let main = raw(&h.db_path());
    assert_eq!(
        main.query_row("SELECT count(*) FROM item", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_buckets(&h);
    let copy = raw(&h.dir.path().join(format!("{BACKUP_PREFIX}5")));
    assert_eq!(pragma(&copy, "user_version"), 5);
}

#[test]
fn store_salvage_copies_the_branch_observations() {
    let (h, repo) = watched("refs/heads/feature");
    h.reflog(
        &repo,
        vec![repo.switch(&WorktreeName::Main, "feature", "other", AT + 5 * SEC)],
        vec![],
    );
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.edit("t1", "c1", &["/work/app/src/a.rs"], AT + 20, 3);
    log.tool(
        "t1",
        "c2",
        lectern_services::session::ToolKind::Execute {
            command: "git switch other".into(),
            workdir: std::sync::Arc::from(Path::new("/work/app")),
            exec_session: None,
            workdir_exact: true,
        },
        AT + 3 * SEC,
        AT + 8 * SEC,
    );
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    let tables = [
        "run",
        "watch_span",
        "head_baseline",
        "reflog",
        "segment",
        "segment_name",
        "turn_obs",
        "call_obs",
        "call_fact",
        "item",
        "call",
    ];
    let counts = |h: &Harness| -> Vec<i64> {
        let c = raw(&h.db_path());
        tables.iter().map(|t| super::store::count(&c, t)).collect()
    };
    let before = counts(&h);
    assert!(before.iter().all(|n| *n > 0), "{before:?}");
    let rows_before = session_usage(&h, &fake, "s").rows;
    let r = h.store().salvage_now(h.now).unwrap();
    assert_eq!(r.lost, 0);
    assert_eq!(counts(&h), before, "every observation was copied");
    // The shares are derived: settled again from the items, the same.
    assert_eq!(session_usage(&h, &fake, "s").rows, rows_before);
    assert_buckets(&h);
    let _ = Checked::Ok;
}

fn history_of(h: &Harness, from: LocalDate, to: LocalDate) -> HistoryUsage {
    let key = h.open[0].clone();
    match h.snapshot(&UsageQuery::History {
        project: ProjectRef::Open(key),
        from,
        to,
        days: false,
    }) {
        Some(UsageSnapshot::History(history)) => history,
        other => panic!("{other:?}"),
    }
}

fn utc(year: i32, month: u8, day: u8, hour: i64) -> i64 {
    crate::window::start(
        LocalDate { year, month, day },
        &crate::testing::TableZone::fixed(0),
    ) + hour * HOUR
}

#[test]
fn archive_moves_the_branch_observations_and_keeps_the_segment_totals() {
    let mut h = Harness::new();
    h.open_repo("/work/app");
    let repo = Repo::new("/work/app/.git");
    h.git.set_default(&repo.common, "refs/heads/main");
    let old = utc(2025, 9, 10, 15);
    h.arm(
        &repo,
        old - 60 * SEC,
        &[("/work/app/.git", Some("refs/heads/feature"))],
    );
    h.reflog(
        &repo,
        vec![repo.switch(&WorktreeName::Main, "feature", "other", old + 5 * SEC)],
        vec![],
    );
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", old);
    log.turn("a", old).usage("a", "ma", old + 10, 40);
    log.tool(
        "a",
        "cb",
        lectern_services::session::ToolKind::Execute {
            command: "git switch other".into(),
            workdir: std::sync::Arc::from(Path::new("/work/app")),
            exec_session: None,
            workdir_exact: true,
        },
        old + 3 * SEC,
        old + 8 * SEC,
    );
    log.edit("a", "ca", &["/work/app/src/a.rs"], old + 20 * SEC, 2);
    log.write();
    h.read_every(&fake);
    h.settle_all(&fake, "s", h.now);
    h.store().close_stale_spans().unwrap();
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
    let before = history_of(&h, all, to);
    let totals = |h: &Harness| {
        h.store()
            .worktree_totals(&repo.common, "refs/heads/other", h.now)
            .unwrap()
            .expect("the branch has a segment")
    };
    let branch_before = totals(&h);
    assert!(branch_before.all.iter().sum::<u64>() > 0);
    let tables = [
        "turn_obs",
        "call_obs",
        "reflog",
        "head_baseline",
        "watch_span",
    ];
    let c = raw(&h.db_path());
    for t in tables {
        assert!(super::store::count(&c, t) > 0, "{t} hot before");
    }
    drop(c);
    h.store().archive_all(h.now, &h.zone).unwrap();
    let c = raw(&h.db_path());
    for t in tables {
        assert_eq!(super::store::count(&c, t), 0, "{t} archived");
        let parts: i64 = c
            .query_row("SELECT count(*) FROM cold_part WHERE tbl = ?1", [t], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(parts, 1, "{t} has a cold segment");
    }
    // The segment stays hot (it is an entity); its tokens are in the
    // totals of the archive.
    assert_eq!(super::store::count(&c, "segment"), 2);
    drop(c);
    assert_eq!(history_of(&h, all, to).months, before.months);
    // The branch's tokens are the archive's totals now, the same sum; the
    // session's own part of it was cold with its facts (a known limit: the
    // archive keeps totals per session and per segment, not both).
    assert_eq!(totals(&h), branch_before);
    assert!(session_usage(&h, &fake, "s").rows.is_empty());
    assert_buckets(&h);
}

#[test]
fn reset_keeps_branch_observations() {
    let (h, _repo) = watched("refs/heads/feature");
    let fake = source();
    let mut log = Log::new(&fake, "s", "/work/app", AT);
    log.turn("t1", AT).usage("t1", "m1", AT + 10, 100);
    log.edit("t1", "c1", &["/work/app/src/a.rs"], AT + 20, 3);
    log.turn("t2", AT + 60 * SEC)
        .usage("t2", "m2", AT + 60 * SEC + 10, 50);
    let records = log.records.clone();
    log.write();
    h.read_every(&fake);
    settled(&h, &fake, "s");
    let before = session_usage(&h, &fake, "s").rows;
    assert_eq!(before.len(), 1);
    // The spans and the reflog are removed, so that observing the calls
    // again would find nobody watched them.
    let c = raw(&h.db_path());
    c.execute_batch("DELETE FROM watch_span; DELETE FROM reflog; DELETE FROM head_baseline;")
        .unwrap();
    drop(c);
    // The adapter is upgraded: the log is read again into a new generation.
    fake.replace("s", records);
    h.read_all(&fake, "s");
    settled(&h, &fake, "s");
    let gens: i64 = raw(&h.db_path())
        .query_row("SELECT count(DISTINCT gen) FROM turn", [], |r| r.get(0))
        .unwrap();
    assert_eq!(gens, 1, "the old generation is gone");
    // What was observed stays: the branch of the edit, and the turn that
    // followed it.
    assert_eq!(session_usage(&h, &fake, "s").rows, before);
    assert_buckets(&h);
}
