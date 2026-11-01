//! `store_*` (docs/design/usage.md「数据库」): the file and its settings,
//! records and cursor in one transaction, migrations with a backup, newer
//! files, and corruption (REINDEX, then salvage).

use std::path::{Path, PathBuf};

use lectern_services::session::{
    Agent, Positioned, Record, Timestamp, ToolCallId, ToolFinished, ToolKind, ToolStarted,
    ToolStatus, TurnId,
};
use lectern_services::usage::{Tokens, Window};
use rusqlite::Connection;

use super::support::*;
use crate::store::schema::{CREATE_V6, CURRENT, Migration, Schema, V1, V2, V3, V4};
use crate::store::{BACKUP_PREFIX, Checked, Mode, SALVAGE_FILE, Step, StoreError, fail_at};
use crate::testing::FakeSessions;

/// A source with one session of `n` usage records.
fn session(id: &str, n: u64) -> FakeSessions {
    let fake = FakeSessions::new("session-claude-code", Agent::ClaudeCode);
    add(&fake, id, n);
    fake
}

/// A session of `n` usage records of 1 input token each (plus `i` output).
fn add(fake: &FakeSessions, id: &str, n: u64) {
    fake.add(id, Path::new("/work/app"), OCT_20, None);
    let at = OCT_20 + 10 * HOUR;
    let mut records: Vec<Positioned<Record>> = vec![context(0, at, "/work/app")];
    for i in 0..n {
        let (pos, at) = (100 * (i + 1), at + i as i64);
        let message = format!("{id}-{i}");
        records.push(claude(pos, at, None, &message, [1, i, 0, 0]));
    }
    fake.push(id, records);
}

pub(super) fn raw(path: &Path) -> Connection {
    Connection::open(path).unwrap()
}

pub(super) fn count(conn: &Connection, table: &str) -> i64 {
    let sql = format!("SELECT count(*) FROM {table}");
    conn.query_row(&sql, [], |r| r.get(0)).unwrap()
}

pub(super) fn pragma(conn: &Connection, name: &str) -> i64 {
    let sql = format!("PRAGMA {name}");
    conn.query_row(&sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn store_page_cache_is_bounded() {
    let h = Harness::new();
    insta::assert_snapshot!(h.store().pragmas());
}

#[test]
fn store_file_is_in_the_data_directory() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("app-data/usage");
    assert!(!data.exists());
    let opened = crate::store::Store::open(&data, OCT_20).unwrap();
    assert!(data.join("usage.db").is_file());
    assert!(!opened.needs_check, "a new file needs no check");
    // Nothing else is written next to it.
    let mut names: Vec<String> = std::fs::read_dir(&data)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert!(names.iter().all(|n| n.starts_with("usage.db")), "{names:?}");
}

#[test]
fn store_records_and_cursor_commit_together() {
    let mut h = Harness::new();
    let fake = session("s", 30);
    // The first batch commits; the second fails between its records and
    // its cursor.
    let first = h.read_once(&fake, "s", 1000);
    assert!(!first.at_end);
    let after_first = h.total();
    h.store().faults.lock().unwrap().before_cursor = true;
    assert!(h.try_read_once(&fake, "s", 1000).is_err());
    assert_eq!(h.total(), after_first, "the failed batch left nothing");
    // A restart reads the failed batch again: nothing twice, nothing lost.
    h.reopen(false);
    h.read_all(&fake, "s");
    assert_eq!(h.total().input, 30);
    assert_eq!(h.total().output, (0..30).sum::<u64>());
    let conn = raw(&h.db_path());
    assert_eq!(count(&conn, "usage_source"), 30);
    assert_eq!(count(&conn, "fact"), 30);
}

/// Versions after the current one (6), for the migration tests.
const V7_STEPS: &[Migration] = &[Migration {
    from: 6,
    sql: "CREATE TABLE extra_v6 (id INTEGER PRIMARY KEY);",
}];
static V7: Schema = Schema {
    version: 7,
    create: CREATE_V6,
    migrations: V7_STEPS,
};
const V9_STEPS: &[Migration] = &[
    Migration {
        from: 6,
        sql: "CREATE TABLE extra_v6 (id INTEGER PRIMARY KEY);",
    },
    Migration {
        from: 7,
        sql: "ALTER TABLE extra_v6 ADD COLUMN note TEXT;",
    },
    Migration {
        from: 8,
        sql: "CREATE TABLE extra_v8 (id INTEGER PRIMARY KEY);",
    },
];
static V8: Schema = Schema {
    version: 8,
    create: CREATE_V6,
    migrations: V9_STEPS,
};
static V9: Schema = Schema {
    version: 9,
    create: CREATE_V6,
    migrations: V9_STEPS,
};

/// Turns the current tables into version 5's: the branch observations gone,
/// `share` without segments, and the columns version 6 added dropped.
macro_rules! back_to_v5 {
    () => {
        "
DROP TABLE run;
DROP TABLE watch_span;
DROP TABLE head_baseline;
DROP TABLE reflog;
DROP TABLE reflog_reset;
DROP TABLE segment;
DROP TABLE segment_name;
DROP TABLE turn_obs;
DROP TABLE call_obs;
DROP INDEX fact_segment;
DROP TABLE share;
CREATE TABLE share (
  id INTEGER PRIMARY KEY, turn INTEGER NOT NULL, location INTEGER NOT NULL,
  kind INTEGER NOT NULL DEFAULT 0, weight INTEGER NOT NULL, total INTEGER NOT NULL,
  at INTEGER NOT NULL, UNIQUE (turn, location, kind));
CREATE INDEX share_at ON share (at);
ALTER TABLE turn DROP COLUMN worktree;
ALTER TABLE turn DROP COLUMN branch;
ALTER TABLE session_gen DROP COLUMN worktree;
ALTER TABLE session_gen DROP COLUMN branch;
ALTER TABLE call DROP COLUMN wt;
ALTER TABLE call DROP COLUMN git;
ALTER TABLE call DROP COLUMN facts;
ALTER TABLE call_fact DROP COLUMN segment;
ALTER TABLE call_fact DROP COLUMN state;
ALTER TABLE item DROP COLUMN cf;
"
    };
}
pub(super) const BACK_TO_V5: &str = back_to_v5!();

/// Turns the version 5 tables into version 4's: no calls, items and shares,
/// and the turn and session columns of version 5 gone.
macro_rules! back_to_v4 {
    () => {
        "
DROP TABLE call;
DROP TABLE call_fact;
DROP TABLE item;
DROP TABLE share;
DROP INDEX turn_pending;
DROP INDEX turn_changed;
DROP INDEX turn_seq;
ALTER TABLE turn DROP COLUMN edits;
DROP INDEX session_parent;
DROP INDEX usage_source_turn;
ALTER TABLE turn DROP COLUMN last_at;
ALTER TABLE turn DROP COLUMN closed;
ALTER TABLE turn DROP COLUMN done;
ALTER TABLE turn DROP COLUMN changed;
ALTER TABLE session DROP COLUMN parent_call;
"
    };
}
pub(super) const BACK_TO_V4: &str = concat!(back_to_v5!(), back_to_v4!());

/// Turns the current tables into version 3's: the ones of version 4, then
/// the `project` table with a unique location and without the columns of
/// version 4, no identity tables, and `fact_bucket` without the project in
/// its key.
pub(super) const BACK_TO_V3: &str = concat!(
    back_to_v5!(),
    back_to_v4!(),
    "
DROP TABLE project_fact;
DROP TABLE alias;
CREATE TABLE project_v3 (
  id INTEGER PRIMARY KEY, pid BLOB NOT NULL UNIQUE, kind INTEGER NOT NULL,
  location INTEGER NOT NULL UNIQUE, opened INTEGER NOT NULL DEFAULT 0, name TEXT);
INSERT INTO project_v3 SELECT id, pid, kind, location, opened, name FROM project;
DROP TABLE project;
ALTER TABLE project_v3 RENAME TO project;
DROP TABLE fact_bucket;
CREATE TABLE fact_bucket (
  bucket INTEGER NOT NULL, location INTEGER NOT NULL, agent INTEGER NOT NULL,
  model INTEGER NOT NULL, review INTEGER NOT NULL, input INTEGER NOT NULL,
  output INTEGER NOT NULL, cache_read INTEGER NOT NULL, cache_write INTEGER NOT NULL,
  rows INTEGER NOT NULL, PRIMARY KEY (bucket, location, agent, model, review)
) WITHOUT ROWID;
INSERT INTO fact_bucket
  SELECT bucket, location, agent, model, review, sum(input), sum(output), sum(cache_read),
         sum(cache_write), count(*) FROM fact GROUP BY bucket, location, agent, model, review;
"
);

fn backups(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(BACKUP_PREFIX))
        .collect();
    out.sort();
    out
}

#[test]
fn store_migration_keeps_observations_after_a_backup() {
    let mut h = Harness::new();
    let fake = session("s", 5);
    h.read_all(&fake, "s");
    let before = h.total();
    h.close(true);
    let reopened = h.open_with(&V7);
    assert_eq!(reopened.migrated_from, Some(6));
    assert_eq!(h.store().mode(), Mode::Writable);
    assert_eq!(h.total(), before);
    // The copy is the version-6 file, whole.
    let copy = raw(&h.dir.path().join(format!("{BACKUP_PREFIX}6")));
    assert_eq!(pragma(&copy, "user_version"), 6);
    assert_eq!(count(&copy, "usage_source"), 5);
    let main = raw(&h.db_path());
    assert_eq!(pragma(&main, "user_version"), 7);
    assert_eq!(count(&main, "usage_source"), 5);
    drop((copy, main));
    // Writing still works on the migrated file.
    let more = session("t", 2);
    h.read_all(&more, "t");
    assert_eq!(h.total().input, 7);

    // Two more migrations: only the two most recent copies stay.
    h.close(true);
    h.open_with(&V8);
    h.close(true);
    h.open_with(&V9);
    assert_eq!(
        backups(h.dir.path()),
        [format!("{BACKUP_PREFIX}7"), format!("{BACKUP_PREFIX}8")]
    );
    assert_eq!(h.total().input, 7);
}

#[test]
fn store_migration_from_version_1_adds_the_cold_tables() {
    let mut h = Harness::new();
    let fake = session("s", 5);
    h.read_all(&fake, "s");
    let before = h.total();
    h.close(true);
    // A file of the first part of step 1: the same rows, without what
    // version 2 adds.
    let conn = raw(&h.db_path());
    conn.execute_batch(BACK_TO_V3).unwrap();
    conn.execute_batch(
        "DROP TABLE cold_part; DROP TABLE cold_month; DROP TABLE cold_total;
         DROP TABLE cold_anchor; DROP TABLE cold_seen; DROP TABLE fact_bucket;
         DROP INDEX usage_source_at; DROP INDEX turn_started; DROP TABLE limit_sample;
         PRAGMA user_version = 1;",
    )
    .unwrap();
    let v1: String = conn
        .query_row(
            "SELECT group_concat(name) FROM (SELECT name FROM sqlite_schema ORDER BY name)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    drop(conn);
    // The schema of version 1 as written by `CREATE_V1`.
    let fresh = tempfile::tempdir().unwrap();
    let written = crate::store::Store::open_with(fresh.path(), &V1, h.now).unwrap();
    drop(written);
    let expected: String = raw(&fresh.path().join("usage.db"))
        .query_row(
            "SELECT group_concat(name) FROM (SELECT name FROM sqlite_schema ORDER BY name)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(v1, expected);
    let reopened = h.open_with(&CURRENT);
    assert_eq!(reopened.migrated_from, Some(1));
    assert_eq!(h.total(), before);
    let main = raw(&h.db_path());
    assert_eq!(pragma(&main, "user_version"), 6);
    for table in [
        "cold_part",
        "cold_month",
        "cold_total",
        "cold_anchor",
        "cold_seen",
        "limit_sample",
        "project_fact",
        "alias",
        "call",
        "call_fact",
        "item",
        "share",
    ] {
        assert_eq!(count(&main, table), 0, "{table}");
    }
    assert_eq!(count(&main, "usage_source"), 5);
    assert_eq!(count(&main, "fact_bucket"), 1, "the facts summed");
    assert_buckets(&h);
    let copy = raw(&h.dir.path().join(format!("{BACKUP_PREFIX}1")));
    assert_eq!(pragma(&copy, "user_version"), 1);
}

#[test]
fn store_migration_from_version_2_adds_the_limit_samples() {
    let mut h = Harness::new();
    let fake = session("s", 5);
    h.read_all(&fake, "s");
    let before = h.total();
    h.close(true);
    // A version-2 file: the same rows, without the limit samples.
    let conn = raw(&h.db_path());
    conn.execute_batch(BACK_TO_V3).unwrap();
    conn.execute_batch("DROP TABLE limit_sample; PRAGMA user_version = 2;")
        .unwrap();
    drop(conn);
    let fresh = tempfile::tempdir().unwrap();
    drop(crate::store::Store::open_with(fresh.path(), &V2, h.now).unwrap());
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
    assert_eq!(reopened.migrated_from, Some(2));
    assert_eq!(h.total(), before);
    let main = raw(&h.db_path());
    assert_eq!(pragma(&main, "user_version"), 6);
    assert_eq!(count(&main, "limit_sample"), 0);
    assert_eq!(count(&main, "usage_source"), 5);
    let copy = raw(&h.dir.path().join(format!("{BACKUP_PREFIX}2")));
    assert_eq!(pragma(&copy, "user_version"), 2);
}

#[test]
fn store_migration_from_version_3_keeps_every_project_and_sum() {
    let mut h = Harness::new();
    let repo = h.open_repo("/work/app");
    let folder = h.projects.folder(Path::new("/work/notes"));
    h.open_project(&folder);
    let fake = session("s", 5);
    h.read_all(&fake, "s");
    let before = h.total();
    let projects = h.store().projects().unwrap();
    assert_eq!(projects.len(), 2);
    h.close(true);
    // A version-3 file: the same rows in the tables of that version.
    let conn = raw(&h.db_path());
    conn.execute_batch(BACK_TO_V3).unwrap();
    conn.execute_batch("PRAGMA user_version = 3;").unwrap();
    drop(conn);
    let fresh = tempfile::tempdir().unwrap();
    drop(crate::store::Store::open_with(fresh.path(), &V3, h.now).unwrap());
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
    assert_eq!(reopened.migrated_from, Some(3));
    assert_eq!(h.total(), before);
    // The projects are the same rows with the same IDs; they own their
    // locations from the start.
    let migrated = h.store().projects().unwrap();
    assert_eq!(migrated.len(), projects.len());
    for (a, b) in projects.iter().zip(&migrated) {
        assert_eq!(
            (a.id, a.pid, a.opened, &a.name),
            (b.id, b.pid, b.opened, &b.name)
        );
        assert_eq!((b.since, b.temp), (0, None));
    }
    assert_eq!(h.project(&repo, Window::D7).total.input, 5);
    let main = raw(&h.db_path());
    assert_eq!(pragma(&main, "user_version"), 6);
    assert_eq!(count(&main, "project_fact"), 0);
    assert_eq!(count(&main, "alias"), 0);
    assert_buckets(&h);
    let copy = raw(&h.dir.path().join(format!("{BACKUP_PREFIX}3")));
    assert_eq!(pragma(&copy, "user_version"), 3);
    assert_eq!(count(&copy, "project"), 2);
    // Writing works on the migrated tables.
    let more = session("t", 2);
    h.read_all(&more, "t");
    assert_eq!(h.total().input, 7);
}

#[test]
fn store_migration_from_version_4_reads_every_session_again() {
    let mut h = Harness::new();
    let fake = session("s", 5);
    h.read_all(&fake, "s");
    let before = h.total();
    let key = fake.key("s");
    h.close(true);
    // A version-4 file: the same rows without calls, items and shares.
    let conn = raw(&h.db_path());
    conn.execute_batch(BACK_TO_V4).unwrap();
    conn.execute_batch("PRAGMA user_version = 4;").unwrap();
    drop(conn);
    let fresh = tempfile::tempdir().unwrap();
    drop(crate::store::Store::open_with(fresh.path(), &V4, h.now).unwrap());
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
    assert_eq!(reopened.migrated_from, Some(4));
    assert_eq!(h.total(), before);
    // Version 4 stored no calls: the session is read again from its start.
    assert!(h.store().cursor(&key).unwrap().is_start());
    h.read_all(&fake, "s");
    assert_eq!(h.total(), before, "nothing counted twice");
    let main = raw(&h.db_path());
    assert_eq!(pragma(&main, "user_version"), 6);
    assert_eq!(count(&main, "usage_source"), 5);
    assert_buckets(&h);
    let copy = raw(&h.dir.path().join(format!("{BACKUP_PREFIX}4")));
    assert_eq!(pragma(&copy, "user_version"), 4);
}

#[test]
fn store_failed_backup_leaves_the_old_version_read_only() {
    let mut h = Harness::new();
    let fake = session("s", 5);
    h.read_all(&fake, "s");
    let before = h.total();
    h.close(true);
    // Something that cannot be replaced sits where the copy goes.
    let blocker = h.dir.path().join(format!("{BACKUP_PREFIX}6"));
    std::fs::create_dir(&blocker).unwrap();
    std::fs::write(blocker.join("keep"), b"x").unwrap();
    let reopened = h.open_with(&V7);
    assert_eq!(reopened.migrated_from, None);
    assert!(matches!(h.store().mode(), Mode::Unwritable(_)));
    let version = pragma(&raw(&h.db_path()), "user_version");
    assert_eq!(version, 6, "not migrated");
    // Read only: no write, the old data still readable.
    let more = session("t", 2);
    let refused = h.try_read_once(&more, "t", 1 << 20);
    assert!(matches!(refused, Err(StoreError::ReadOnly)));
    assert_eq!(h.total(), before);
}

#[test]
fn store_newer_version_is_read_only() {
    let mut h = Harness::new();
    let fake = session("s", 3);
    h.read_all(&fake, "s");
    let before = h.total();
    h.close(true);
    let conn = raw(&h.db_path());
    conn.execute_batch("PRAGMA user_version = 99;").unwrap();
    drop(conn);
    h.open_with(&CURRENT);
    assert_eq!(h.store().mode(), Mode::Newer);
    let more = session("t", 2);
    let refused = h.try_read_once(&more, "t", 1 << 20);
    assert!(matches!(refused, Err(StoreError::ReadOnly)));
    assert_eq!(h.total(), before);
    // Not even a repair rewrites it.
    let repaired = h.store().repair(h.now);
    assert!(matches!(repaired, Err(StoreError::ReadOnly)));
    let version = pragma(&raw(&h.db_path()), "user_version");
    assert_eq!(version, 99, "left as it was");
}

/// The page size and a table's or index's root page.
fn root_page(path: &Path, name: &str) -> (usize, usize) {
    let conn = raw(path);
    let size = pragma(&conn, "page_size");
    let root: i64 = conn
        .query_row(
            "SELECT rootpage FROM sqlite_schema WHERE name = ?1",
            [name],
            |r| r.get(0),
        )
        .unwrap();
    (size as usize, root as usize)
}

/// Rewrites one byte of a page (the database is closed).
fn poke(path: &Path, page_size: usize, page: usize, offset: usize, value: u8) {
    let mut bytes = std::fs::read(path).unwrap();
    let header = if page == 1 { 100 } else { 0 };
    bytes[(page - 1) * page_size + header + offset] = value;
    std::fs::write(path, bytes).unwrap();
}

fn names_in(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn store_reindex_repairs_a_damaged_index() {
    let mut h = Harness::new();
    let fake = session("s", 20);
    h.read_all(&fake, "s");
    let before = h.total();
    h.close(false);
    let (size, root) = root_page(&h.db_path(), "fact_at");
    // A wrong fragment count on the index's page: quick_check reports it,
    // the table data is intact.
    poke(&h.db_path(), size, root, 7, 13);
    let reopened = h.reopen(false);
    assert!(reopened.needs_check, "not closed normally");
    assert_eq!(h.store().check(h.now).unwrap(), Checked::Reindexed);
    assert_eq!(h.store().check(h.now).unwrap(), Checked::Ok);
    assert_eq!(h.total(), before);
    let names = names_in(h.dir.path());
    assert!(!names.iter().any(|n| n.contains("corrupt")), "{names:?}");
}

/// The first child of an interior table page.
fn first_child(path: &Path, page_size: usize, page: usize) -> usize {
    let bytes = std::fs::read(path).unwrap();
    let base = (page - 1) * page_size;
    assert_eq!(bytes[base], 0x05, "an interior table page");
    let cell = u16::from_be_bytes([bytes[base + 12], bytes[base + 13]]) as usize;
    let child: [u8; 4] = bytes[base + cell..base + cell + 4].try_into().unwrap();
    u32::from_be_bytes(child) as usize
}

#[test]
fn store_salvage_keeps_every_readable_row() {
    let mut h = Harness::new();
    let repo = h.open_repo("/work/app");
    let fake = FakeSessions::new("session-claude-code", Agent::ClaudeCode);
    for id in ["a", "b", "c"] {
        add(&fake, id, 300);
    }
    h.read_every(&fake);
    let before = h.total();
    // A limit sample is an observation: the salvage copies it.
    let limit = crate::limits::parse_statusline(
        br#"{"rate_limits": {"five_hour": {"used_percentage": 42, "resets_at": 1792680000}}}"#,
        Timestamp(h.now - HOUR),
    )
    .unwrap();
    assert!(h.store().put_samples(&limit, h.now).unwrap());
    h.close(false);
    let (size, root) = root_page(&h.db_path(), "usage_source");
    let leaf = first_child(&h.db_path(), size, root);
    // Garbage over a whole leaf of the sources table.
    let mut bytes = std::fs::read(h.db_path()).unwrap();
    for b in &mut bytes[(leaf - 1) * size..leaf * size] {
        *b = 0xFF;
    }
    std::fs::write(h.db_path(), bytes).unwrap();

    h.reopen(false);
    let Checked::Recovered(r) = h.store().check(h.now).unwrap() else {
        panic!("not salvaged");
    };
    assert!(r.lost > 0);
    assert!(r.copied > 0);
    assert_eq!(r.sessions_reread, 3);
    assert!(r.kept.is_file(), "the corrupt file is kept");
    let quick: String = raw(&h.db_path())
        .query_row("PRAGMA quick_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(quick, "ok");
    // The project survived the copy; the sessions read again from the
    // start give back every usage, once.
    let p = h.project(&repo, Window::D7);
    assert!(p.project.key.is_some());
    assert_eq!(
        h.store().samples().unwrap(),
        limit,
        "the limit sample survived"
    );
    h.read_every(&fake);
    assert_eq!(h.total(), before);
    assert_eq!(count(&raw(&h.db_path()), "usage_source"), 900);
}

/// Garbage over the first leaf of the sources table (the database is
/// closed).
fn damage_sources(path: &Path) {
    let (size, root) = root_page(path, "usage_source");
    let leaf = first_child(path, size, root);
    garbage(path, size, leaf);
}

fn garbage(path: &Path, size: usize, page: usize) {
    let mut bytes = std::fs::read(path).unwrap();
    for b in &mut bytes[(page - 1) * size..page * size] {
        *b = 0xFF;
    }
    std::fs::write(path, bytes).unwrap();
}

fn quick_ok(path: &Path) -> bool {
    let quick: String = raw(path)
        .query_row("PRAGMA quick_check", [], |r| r.get(0))
        .unwrap_or_default();
    quick == "ok"
}

#[test]
fn store_interrupted_salvage_runs_again() {
    let steps = [
        Step::Created,
        Step::Copied,
        Step::Rebuilt,
        Step::Synced,
        Step::Kept,
        Step::Unjournaled,
    ];
    for step in steps {
        let mut h = Harness::new();
        let fake = FakeSessions::new("session-claude-code", Agent::ClaudeCode);
        for id in ["a", "b"] {
            add(&fake, id, 300);
        }
        h.read_every(&fake);
        let before = h.total();
        h.close(false);
        damage_sources(&h.db_path());
        h.reopen(false);
        // The process ends in the middle of the salvage.
        fail_at(step);
        assert!(h.store().check(h.now).is_err(), "{step:?}");
        h.close(false);
        // `usage.db` is still the damaged original, the copy is left over.
        assert!(!quick_ok(&h.db_path()), "{step:?}: replaced too early");
        assert!(h.dir.path().join(SALVAGE_FILE).exists(), "{step:?}");
        // The next open salvages again, from the start.
        let reopened = h.reopen(false);
        let r = reopened.recovered.expect("salvaged at open");
        assert!(r.lost > 0, "{step:?}");
        assert!(!h.dir.path().join(SALVAGE_FILE).exists(), "{step:?}");
        assert!(quick_ok(&h.db_path()), "{step:?}");
        h.read_every(&fake);
        assert_eq!(h.total(), before, "{step:?}");
        assert_eq!(count(&raw(&h.db_path()), "usage_source"), 600);
    }
}

/// A session per ID, each in its own directory, with one usage record whose
/// message ID contains the session ID.
fn many(fake: &FakeSessions, ids: std::ops::Range<usize>) {
    for i in ids {
        let id = long_id(i);
        let cwd = format!("/work/{id}");
        fake.add(&id, Path::new(&cwd), OCT_20, None);
        let at = OCT_20 + 10 * HOUR;
        let message = format!("{id}-m");
        let turn_id = format!("{id}-t");
        let call = ToolCallId(format!("{id}-c").into());
        let records = vec![
            context(0, at, &cwd),
            turn(50, &turn_id, at),
            claude(100, at, None, &message, [1, 0, 0, 0]),
            // An edit of a file in the session's directory.
            rec(
                150,
                Record::ToolStarted(ToolStarted {
                    call: call.clone(),
                    turn: Some(TurnId(turn_id.as_str().into())),
                    at: Timestamp(at),
                    name: "Edit".into(),
                    kind: ToolKind::Edit {
                        paths: vec![PathBuf::from(format!("{cwd}/f.rs"))].into(),
                    },
                }),
            ),
            rec(
                160,
                Record::ToolFinished(ToolFinished {
                    call,
                    at: Timestamp(at),
                    status: ToolStatus::Completed,
                    exec_session: None,
                }),
            ),
        ];
        fake.push(&id, records);
    }
}

fn long_id(i: usize) -> String {
    format!("session-with-a-long-name-{i:04}")
}

/// Counted usage not under the session, directory, model and agent it was
/// written for: every key and every directory contains its session's ID.
fn misattributed(path: &Path) -> i64 {
    raw(path)
        .query_row(
            "SELECT count(*) FROM usage_value v
               JOIN usage_source u ON u.id = v.source
               LEFT JOIN session s ON s.id = u.session
               LEFT JOIN intern i ON i.id = s.sid
               LEFT JOIN intern a ON a.id = s.agent
               LEFT JOIN location l ON l.id = u.location
               LEFT JOIN intern m ON m.id = u.model
             WHERE i.text IS NULL OR l.path IS NULL OR m.text IS NULL OR a.text IS NULL
                OR instr(u.key, i.text) = 0 OR instr(l.path, i.text) = 0
                OR m.text != CAST('model-a' AS BLOB)
                OR a.text != CAST('agent:claude-code' AS BLOB)",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

/// References to rows that do not exist (`NULL NOT IN` an empty table is
/// true, so nullable columns are checked for NULL first).
fn dangling(path: &Path) -> i64 {
    let sql = "SELECT
        (SELECT count(*) FROM session WHERE source NOT IN (SELECT id FROM intern)
           OR sid NOT IN (SELECT id FROM intern) OR agent NOT IN (SELECT id FROM intern)
           OR (parent_source IS NOT NULL AND parent_source NOT IN (SELECT id FROM intern))
           OR (parent_sid IS NOT NULL AND parent_sid NOT IN (SELECT id FROM intern)))
      + (SELECT count(*) FROM usage_source WHERE session NOT IN (SELECT id FROM session)
           OR location NOT IN (SELECT id FROM location) OR model NOT IN (SELECT id FROM intern)
           OR (turn IS NOT NULL AND turn NOT IN (SELECT id FROM turn)))
      + (SELECT count(*) FROM turn WHERE session NOT IN (SELECT id FROM session)
           OR tid NOT IN (SELECT id FROM intern)
           OR (location IS NOT NULL AND location NOT IN (SELECT id FROM location)))
      + (SELECT count(*) FROM session_gen WHERE session NOT IN (SELECT id FROM session)
           OR (cwd IS NOT NULL AND cwd NOT IN (SELECT id FROM location))
           OR (model IS NOT NULL AND model NOT IN (SELECT id FROM intern))
           OR (turn IS NOT NULL AND turn NOT IN (SELECT id FROM turn)))
      + (SELECT count(*) FROM project WHERE location NOT IN (SELECT id FROM location))
      + (SELECT count(*) FROM call WHERE session NOT IN (SELECT id FROM session)
           OR (turn IS NOT NULL AND turn NOT IN (SELECT id FROM turn)))
      + (SELECT count(*) FROM call_fact WHERE session NOT IN (SELECT id FROM session)
           OR path NOT IN (SELECT id FROM intern)
           OR location NOT IN (SELECT id FROM location)
           OR (worktree IS NOT NULL AND worktree NOT IN (SELECT id FROM intern)))
      + (SELECT count(*) FROM item WHERE session NOT IN (SELECT id FROM session)
           OR turn NOT IN (SELECT id FROM turn) OR location NOT IN (SELECT id FROM location)
           OR path NOT IN (SELECT id FROM intern)
           OR (cf IS NOT NULL AND cf NOT IN (SELECT id FROM call_fact)))
      + (SELECT count(*) FROM turn WHERE
           (worktree IS NOT NULL AND worktree NOT IN (SELECT id FROM intern))
           OR (branch IS NOT NULL AND branch NOT IN (SELECT id FROM intern)))
      + (SELECT count(*) FROM session_gen WHERE
           (worktree IS NOT NULL AND worktree NOT IN (SELECT id FROM intern))
           OR (branch IS NOT NULL AND branch NOT IN (SELECT id FROM intern)))
      + (SELECT count(*) FROM call_fact WHERE
           segment IS NOT NULL AND segment NOT IN (SELECT id FROM segment))
      + (SELECT count(*) FROM watch_span WHERE repo NOT IN (SELECT id FROM location))
      + (SELECT count(*) FROM head_baseline WHERE repo NOT IN (SELECT id FROM location)
           OR wt NOT IN (SELECT id FROM intern))
      + (SELECT count(*) FROM reflog WHERE repo NOT IN (SELECT id FROM location)
           OR run NOT IN (SELECT id FROM run)
           OR (file != 2 AND wt NOT IN (SELECT id FROM intern)))
      + (SELECT count(*) FROM reflog_reset WHERE repo NOT IN (SELECT id FROM location)
           OR run NOT IN (SELECT id FROM run))
      + (SELECT count(*) FROM segment WHERE repo NOT IN (SELECT id FROM location))
      + (SELECT count(*) FROM segment_name WHERE segment NOT IN (SELECT id FROM segment))
      + (SELECT count(*) FROM turn_obs WHERE session NOT IN (SELECT id FROM session)
           OR tid NOT IN (SELECT id FROM intern)
           OR (end_seg IS NOT NULL AND end_seg NOT IN (SELECT id FROM segment)))
      + (SELECT count(*) FROM call_obs WHERE session NOT IN (SELECT id FROM session)
           OR wt NOT IN (SELECT id FROM intern))";
    raw(path).query_row(sql, [], |r| r.get(0)).unwrap()
}

/// The last child of an interior table page.
fn last_child(path: &Path, page_size: usize, page: usize) -> usize {
    let bytes = std::fs::read(path).unwrap();
    let base = (page - 1) * page_size;
    assert_eq!(bytes[base], 0x05, "an interior table page");
    let child: [u8; 4] = bytes[base + 8..base + 12].try_into().unwrap();
    u32::from_be_bytes(child) as usize
}

#[test]
fn store_salvage_keeps_rows_that_point_at_lost_rows() {
    // The first leaf of `intern` holds the source ID, the agent label and the
    // model every session shares; the last leaves hold the newest rows,
    // whose IDs new rows would take.
    let cases = [
        ("intern", true),
        ("intern", false),
        ("session", false),
        ("location", false),
    ];
    for (table, first) in cases {
        let case = format!("{table} {}", if first { "first" } else { "last" });
        let mut h = Harness::new();
        let fake = FakeSessions::new("session-claude-code", Agent::ClaudeCode);
        many(&fake, 0..300);
        h.read_every(&fake);
        let before = h.total();
        h.close(false);
        let (size, root) = root_page(&h.db_path(), table);
        let leaf = if first {
            first_child(&h.db_path(), size, root)
        } else {
            last_child(&h.db_path(), size, root)
        };
        garbage(&h.db_path(), size, leaf);
        h.reopen(false);
        let Checked::Recovered(r) = h.store().check(h.now).unwrap() else {
            panic!("{case}: not salvaged");
        };
        assert!(r.lost > 0 && r.repaired > 0, "{case}: {r:?}");
        assert_eq!(dangling(&h.db_path()), 0, "{case}: after the salvage");
        // Every usage still counts, logs or not.
        assert_eq!(h.total(), before, "{case}: after the salvage");
        // New sessions first, so they would take lost IDs; then the old ones
        // read their logs again and win back their usage, counted once.
        many(&fake, 300..400);
        for i in 300..400 {
            h.read_all(&fake, &long_id(i));
        }
        h.read_every(&fake);
        assert_eq!(dangling(&h.db_path()), 0, "{case}: after reading");
        assert_eq!(misattributed(&h.db_path()), 0, "{case}: after reading");
        assert_eq!(h.total().input, before.input + 100, "{case}");
        assert_eq!(count(&raw(&h.db_path()), "usage_value"), 400, "{case}");
        // Nothing waits for a repair any more, placeholder sessions
        // included: no log will ever give their records again.
        let marked: i64 = raw(&h.db_path())
            .query_row(
                "SELECT count(*) FROM usage_source WHERE repaired = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(marked, 0, "{case}: sources still marked");
        assert!(!h.store().repairing(), "{case}");
        assert_buckets(&h);
    }
}

#[test]
fn store_salvage_does_not_mark_placeholder_sessions() {
    // The first leaves of `intern` and `location` hold the first sessions'
    // IDs and directories: those sessions become placeholders whose
    // sources lost their location (and are repaired).
    let mut h = Harness::new();
    let fake = FakeSessions::new("session-claude-code", Agent::ClaudeCode);
    many(&fake, 0..300);
    h.read_every(&fake);
    h.close(false);
    for table in ["intern", "location"] {
        let (size, root) = root_page(&h.db_path(), table);
        garbage(&h.db_path(), size, first_child(&h.db_path(), size, root));
    }
    h.reopen(false);
    assert!(matches!(
        h.store().check(h.now).unwrap(),
        Checked::Recovered(_)
    ));
    let unknown: i64 = raw(&h.db_path())
        .query_row(
            "SELECT count(*) FROM usage_source u JOIN session s ON s.id = u.session
             JOIN intern b ON b.id = s.sid JOIN location l ON l.id = u.location
             WHERE substr(b.text, 1, 1) = x'FF' AND l.path = x''",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(unknown > 0, "placeholder sessions with repaired sources");
    let placeholder_marked: i64 = raw(&h.db_path())
        .query_row(
            "SELECT count(*) FROM usage_source u JOIN session s ON s.id = u.session
             JOIN intern b ON b.id = s.sid
             WHERE u.repaired = 1 AND substr(b.text, 1, 1) = x'FF'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(placeholder_marked, 0);
    let placeholders: i64 = raw(&h.db_path())
        .query_row(
            "SELECT count(*) FROM session s JOIN intern b ON b.id = s.sid
             WHERE substr(b.text, 1, 1) = x'FF'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(placeholders > 0, "the case has placeholder sessions");
    // No source waits for a log that will never come: ingest stops
    // looking for repairs once the other sessions are read again
    // (`store_salvage_keeps_rows_that_point_at_lost_rows` clears marks by
    // reading).
    h.read_every(&fake);
    assert!(!h.store().repairing());
}

#[test]
fn store_batch_read_across_a_salvage_is_read_again() {
    let mut h = Harness::new();
    let fake = FakeSessions::new("session-claude-code", Agent::ClaudeCode);
    for id in ["a", "b"] {
        add(&fake, id, 300);
    }
    h.read_once(&fake, "a", 2000);
    h.read_all(&fake, "b");
    // A read of "a" is under way (cursor and batch taken, not yet stored)
    // when the file turns out corrupt and is salvaged: every cursor goes
    // back to the start.
    let pending = h.read_batch(&fake, "a", 2000);
    assert!(pending.cursor.offset() > 0);
    h.close(false);
    damage_sources(&h.db_path());
    h.reopen(false);
    assert!(matches!(
        h.store().check(h.now).unwrap(),
        Checked::Recovered(_)
    ));
    assert_eq!(h.store().cursor(&fake.key("a")).unwrap().offset(), 0);
    // The read in flight stores now: its cursor is stale, nothing is
    // written, and the start of the log is not skipped.
    let out = h.commit(&pending).unwrap();
    assert!(out.stale);
    assert_eq!(h.store().cursor(&fake.key("a")).unwrap().offset(), 0);
    h.read_every(&fake);
    assert_eq!(h.total().input, 600);
    assert_eq!(count(&raw(&h.db_path()), "usage_value"), 600);
}

#[test]
fn store_unreadable_file_is_salvaged_at_open() {
    let mut h = Harness::new();
    let fake = session("s", 4);
    h.read_all(&fake, "s");
    let before = h.total();
    h.close(true);
    let mut bytes = std::fs::read(h.db_path()).unwrap();
    for b in &mut bytes[..100] {
        *b = 0x5A;
    }
    std::fs::write(h.db_path(), bytes).unwrap();
    let reopened = h.reopen(false);
    let r = reopened.recovered.expect("salvaged at open");
    assert!(r.kept.is_file());
    assert!(!reopened.needs_check);
    // Nothing could be copied; the session is read again.
    assert_eq!(h.total(), Tokens::default());
    h.read_all(&fake, "s");
    assert_eq!(h.total(), before);
}

#[test]
fn store_full_disk_fails_the_write_and_keeps_the_cursor() {
    let h = Harness::new();
    let fake = session("s", 400);
    let pages = pragma(&raw(&h.db_path()), "page_count");
    h.store().set_max_page_count(pages as u64);
    let err = h.try_read_once(&fake, "s", 1 << 20).unwrap_err();
    assert!(!err.is_corrupt());
    assert_eq!(h.store().cursor(&fake.key("s")).unwrap().offset(), 0);
    h.store().set_unwritable(err.reason());
    assert!(matches!(h.store().mode(), Mode::Unwritable(_)));
    // Space again: the next write succeeds and the store is writable.
    h.store().set_max_page_count(1 << 30);
    h.read_all(&fake, "s");
    assert_eq!(h.store().mode(), Mode::Writable);
    assert_eq!(h.total().input, 400);
}

#[test]
fn store_migration_from_version_4_goes_on_with_a_replay_in_progress() {
    use super::attribution::{Log, source};
    let mut h = Harness::new();
    let lib = h.open_repo("/work/lib");
    let fake = source();
    let at = OCT_20 + 9 * HOUR;
    // The edit is in the first lines; more usage follows.
    let mut log = Log::new(&fake, "s", "/work/app", at);
    log.turn("t1", at)
        .edit("t1", "c1", &["/work/lib/a.rs"], at + 20, 2);
    for i in 0..4 {
        log.usage("t1", &format!("m{i}"), at + 100 + i, 5);
    }
    log.write();
    h.read_all(&fake, "s");
    // The adapter is upgraded: the log is replayed, and a read ends before
    // the replay does.
    let records: Vec<_> = {
        let mut again = Log::new(&fake, "again", "/work/app", at);
        again
            .turn("t1", at)
            .edit("t1", "c1", &["/work/lib/a.rs"], at + 20, 2);
        for i in 0..4 {
            again.usage("t1", &format!("m{i}"), at + 100 + i, 5);
        }
        std::mem::take(&mut again.records)
    };
    fake.replace("s", records);
    let part = h.read_once(&fake, "s", 450);
    assert!(part.replay_started && !part.at_end);
    h.close(true);
    // A version-4 file: no calls, and the replay still in progress.
    let conn = raw(&h.db_path());
    conn.execute_batch(BACK_TO_V4).unwrap();
    conn.execute_batch("PRAGMA user_version = 4;").unwrap();
    let replaying: i64 = conn
        .query_row(
            "SELECT count(*) FROM session WHERE replay IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(replaying, 1);
    drop(conn);
    let reopened = h.open_with(&CURRENT);
    assert_eq!(reopened.migrated_from, Some(4));
    // The session is read from its start into the generation being filled,
    // calls included, and the replay ends.
    h.read_all(&fake, "s");
    let main = raw(&h.db_path());
    let replaying: i64 = main
        .query_row(
            "SELECT count(*) FROM session WHERE replay IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(replaying, 0);
    assert_eq!(count(&main, "call_fact"), 1);
    drop(main);
    assert_eq!(h.total().input, 20, "nothing counted twice");
    h.settle_all(&fake, "s", OCT_20 + 2 * DAY);
    assert_eq!(
        h.project(&lib, Window::D7).total.input,
        20,
        "the edit counts"
    );
    assert_buckets(&h);
}

/// Manual measurement (`cargo test --release -p lectern-usage --lib
/// heavy_disk_by_table -- --ignored --nocapture`): where the bytes of three
/// heavy days are, per table and index, by `dbstat` when SQLite has it and
/// by row counts otherwise.
#[test]
#[ignore = "a measurement, run by hand"]
fn heavy_disk_by_table() {
    let dir = tempfile::tempdir().unwrap();
    crate::testing::heavy_days(dir.path(), 3).unwrap();
    let file = dir.path().join("usage.db");
    let conn = Connection::open(&file).unwrap();
    let total = std::fs::metadata(&file).unwrap().len();
    eprintln!(
        "usage.db after 3 heavy days: {total} bytes, {} per day",
        total / 3
    );
    let sql =
        "SELECT name, sum(pgsize), count(*) FROM dbstat GROUP BY name ORDER BY 2 DESC LIMIT 30";
    match conn.prepare(sql) {
        Ok(mut stmt) => {
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })
                .unwrap();
            for row in rows {
                let (name, bytes, pages) = row.unwrap();
                eprintln!("{name:32} {:>10} bytes {pages:>6} pages", bytes);
            }
        }
        Err(e) => eprintln!("no dbstat: {e}"),
    }
    for table in [
        "intern",
        "usage_source",
        "usage_value",
        "fact",
        "fact_bucket",
        "turn",
        "call",
        "call_fact",
        "item",
        "share",
        "location",
        "turn_obs",
        "call_obs",
        "reflog",
        "segment",
        "head_baseline",
    ] {
        eprintln!("rows {table:14} {}", count(&conn, table));
    }
}

/// The lines of the query plan of an SQL statement.
pub(super) fn plan(conn: &Connection, sql: &str) -> Vec<String> {
    let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    let n = stmt.parameter_count();
    let params = rusqlite::params_from_iter(std::iter::repeat_n(1i64, n));
    stmt.query_map(params, |r| r.get::<_, String>(3))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn store_statements_on_item_and_share_use_an_index() {
    use crate::store::{DELETE_ITEMS, RESTART_ITEMS, drop_by_turn_sql, weights_sql};
    let h = Harness::new();
    let conn = raw(&h.db_path());
    let frozen =
        "NOT ((started_at >= 1 AND started_at < 2) OR (started_at >= 5 AND started_at < 6))";
    // Every statement that finds `item` or `share` rows. A scan of either
    // table in an ingest transaction holds the writer for as long as the
    // table is big (millions of rows); `item` and `share` have no session or
    // generation to find rows by, so rows are found through their turns.
    let statements: Vec<(String, &str)> = vec![
        (drop_by_turn_sql("item", "NOT 0"), "item_turn"),
        (drop_by_turn_sql("item", frozen), "item_turn"),
        (
            drop_by_turn_sql("share", "NOT 0"),
            "sqlite_autoindex_share_1",
        ),
        (
            drop_by_turn_sql("share", frozen),
            "sqlite_autoindex_share_1",
        ),
        (RESTART_ITEMS.to_owned(), "item_turn"),
        (DELETE_ITEMS.to_owned(), "item_turn"),
        (weights_sql("1,2,3"), "item_turn"),
        (
            "DELETE FROM share WHERE turn = ?1".to_owned(),
            "sqlite_autoindex_share_1",
        ),
        (
            "SELECT location, weight FROM share WHERE turn = ?1".to_owned(),
            "sqlite_autoindex_share_1",
        ),
    ];
    // The check sees a scan: a delete by the session and the generation,
    // which `item` has no index for, is one.
    let old = plan(&conn, "DELETE FROM item WHERE session = ?1 AND gen = ?2");
    assert!(old.iter().any(|l| l.starts_with("SCAN item")), "{old:?}");
    for (sql, index) in statements {
        let lines = plan(&conn, &sql);
        let text = lines.join(" | ");
        assert!(
            !lines
                .iter()
                .any(|l| l.starts_with("SCAN item") || l.starts_with("SCAN share")),
            "scans a big table: {sql}\n{text}"
        );
        assert!(text.contains(index), "does not use {index}: {sql}\n{text}");
    }
}

/// Manual measurement (`cargo test --release -p lectern-usage --lib
/// drop_generation_with_two_million_items -- --ignored --nocapture`): the
/// time `drop_generation` holds the writer for one session of a table of
/// about 2 million `item` rows, against the statement it replaced (a delete
/// by session and generation, which scans the table).
#[test]
#[ignore = "a measurement, run by hand"]
fn drop_generation_with_two_million_items() {
    use std::time::Instant;
    let dir = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(dir.path(), OCT_20).unwrap().store;
    let (sessions, turns, files) = (1000i64, 50i64, 40i64);
    store
        .transaction(|tx| {
            let mut turn = tx.prepare(
                "INSERT INTO turn (session, gen, tid, seq, started_at, location, done)
                 VALUES (?1, 1, ?2, ?3, ?4, 1, 1)",
            )?;
            let mut item = tx.prepare(
                "INSERT INTO item (session, gen, turn, path, location, lines, at)
                 VALUES (?1, 1, ?2, ?3, 1, 5, ?4)",
            )?;
            for s in 1..=sessions {
                for t in 0..turns {
                    let at = OCT_20 + s * 1000 + t;
                    turn.execute(rusqlite::params![s, t + 1, t + 1, at])?;
                    let id = tx.last_insert_rowid();
                    for f in 0..files {
                        item.execute(rusqlite::params![s, id, f + 1, at])?;
                    }
                }
            }
            Ok(())
        })
        .unwrap();
    let rows: i64 = store
        .read(|c| c.query_row("SELECT count(*) FROM item", [], |r| r.get(0)))
        .unwrap();
    eprintln!("item rows: {rows}");
    let time =
        |what: &str, work: &dyn Fn(&rusqlite::Transaction<'_>) -> rusqlite::Result<usize>| {
            let started = Instant::now();
            let deleted = store
                .write(|conn| {
                    let tx = conn.transaction()?;
                    let n = work(&tx)?;
                    // Rolled back: the table stays as it is.
                    drop(tx);
                    Ok(n)
                })
                .unwrap();
            eprintln!("{what}: {:?} ({deleted} rows)", started.elapsed());
        };
    time("drop_generation (through the turns)", &|tx| {
        let frozen = crate::store::cold::frozen(tx)?;
        let mut keys = std::collections::HashSet::new();
        let before = tx.total_changes();
        crate::store::drop_generation(tx, 500, 1, &mut keys, &frozen)?;
        // Rows deleted: the items, the turns, nothing else is in the table.
        Ok(usize::try_from(tx.total_changes() - before).unwrap_or(0))
    });
    time("old statement (by session and generation)", &|tx| {
        tx.execute(
            "DELETE FROM item WHERE session = ?1 AND gen = ?2 AND NOT 0",
            rusqlite::params![500, 1],
        )
    });
}
