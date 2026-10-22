//! `store_*` (docs/design/usage.md「数据库」): the file and its settings,
//! records and cursor in one transaction, migrations with a backup, newer
//! files, and corruption (REINDEX, then salvage).

use std::path::Path;

use lectern_services::session::{Agent, Positioned, Record};
use lectern_services::usage::{Tokens, Window};
use rusqlite::Connection;

use super::support::*;
use crate::store::schema::{CREATE_V1, CURRENT, Migration, Schema};
use crate::store::{BACKUP_PREFIX, Checked, Mode, StoreError};
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

fn raw(path: &Path) -> Connection {
    Connection::open(path).unwrap()
}

fn count(conn: &Connection, table: &str) -> i64 {
    let sql = format!("SELECT count(*) FROM {table}");
    conn.query_row(&sql, [], |r| r.get(0)).unwrap()
}

fn pragma(conn: &Connection, name: &str) -> i64 {
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

const V2_STEPS: &[Migration] = &[Migration {
    from: 1,
    sql: "CREATE TABLE extra_v2 (id INTEGER PRIMARY KEY);",
}];
static V2: Schema = Schema {
    version: 2,
    create: CREATE_V1,
    migrations: V2_STEPS,
};
const V4_STEPS: &[Migration] = &[
    Migration {
        from: 1,
        sql: "CREATE TABLE extra_v2 (id INTEGER PRIMARY KEY);",
    },
    Migration {
        from: 2,
        sql: "ALTER TABLE extra_v2 ADD COLUMN note TEXT;",
    },
    Migration {
        from: 3,
        sql: "CREATE TABLE extra_v4 (id INTEGER PRIMARY KEY);",
    },
];
static V3: Schema = Schema {
    version: 3,
    create: CREATE_V1,
    migrations: V4_STEPS,
};
static V4: Schema = Schema {
    version: 4,
    create: CREATE_V1,
    migrations: V4_STEPS,
};

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
    let reopened = h.open_with(&V2);
    assert_eq!(reopened.migrated_from, Some(1));
    assert_eq!(h.store().mode(), Mode::Writable);
    assert_eq!(h.total(), before);
    // The copy is the version-1 file, whole.
    let copy = raw(&h.dir.path().join(format!("{BACKUP_PREFIX}1")));
    assert_eq!(pragma(&copy, "user_version"), 1);
    assert_eq!(count(&copy, "usage_source"), 5);
    let main = raw(&h.db_path());
    assert_eq!(pragma(&main, "user_version"), 2);
    assert_eq!(count(&main, "usage_source"), 5);
    drop((copy, main));
    // Writing still works on the migrated file.
    let more = session("t", 2);
    h.read_all(&more, "t");
    assert_eq!(h.total().input, 7);

    // Two more migrations: only the two most recent copies stay.
    h.close(true);
    h.open_with(&V3);
    h.close(true);
    h.open_with(&V4);
    assert_eq!(
        backups(h.dir.path()),
        [format!("{BACKUP_PREFIX}2"), format!("{BACKUP_PREFIX}3")]
    );
    assert_eq!(h.total().input, 7);
}

#[test]
fn store_failed_backup_leaves_the_old_version_read_only() {
    let mut h = Harness::new();
    let fake = session("s", 5);
    h.read_all(&fake, "s");
    let before = h.total();
    h.close(true);
    // Something that cannot be replaced sits where the copy goes.
    let blocker = h.dir.path().join(format!("{BACKUP_PREFIX}1"));
    std::fs::create_dir(&blocker).unwrap();
    std::fs::write(blocker.join("keep"), b"x").unwrap();
    let reopened = h.open_with(&V2);
    assert_eq!(reopened.migrated_from, None);
    assert!(matches!(h.store().mode(), Mode::Unwritable(_)));
    let version = pragma(&raw(&h.db_path()), "user_version");
    assert_eq!(version, 1, "not migrated");
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
    h.read_every(&fake);
    assert_eq!(h.total(), before);
    assert_eq!(count(&raw(&h.db_path()), "usage_source"), 900);
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
