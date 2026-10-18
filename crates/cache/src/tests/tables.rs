//! Custom tables (`tables_*`): registration and versions, unit writes and
//! reads, access times, quota and budget eviction, the eviction generation,
//! and rollbacks and crashes leaving no rows without a unit.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use rusqlite::Connection;

use super::support::{Blob, Fixture, config, key, meta_bytes, minute, noise, raw, sum_sizes};
use crate::db::{ENTRY_OVERHEAD, TOUCH_CHUNK, TOUCH_SQL};
use crate::slot::TOUCHED_MAX;
use crate::{
    Budgets, Cache, Compression, Dropped, Key, MAX_BATCH, MAX_KEY, Namespace, Param, Partition,
    PartitionState, TableSet, TableWrite, Tables, Unit, Written,
};

const HOUR: Duration = Duration::from_secs(60 * 60);
const MINUTE: Duration = Duration::from_secs(60);

/// A registration shaped like code_intel's: one row per unit in `<p>_ext`,
/// definitions and references in two more tables, under namespace `$ns`.
macro_rules! tag_tables {
    ($module:ident, $ns:literal, $version:literal, $p:literal, $quota:expr) => {
        pub(super) mod $module {
            use crate::{Compression, Namespace, Tables};

            pub(crate) const INSERT_EXT: &str =
                concat!("INSERT INTO ", $p, "_ext (unit, oid) VALUES (?1, ?2)");
            pub(crate) const INSERT_DEF: &str = concat!(
                "INSERT INTO ",
                $p,
                "_def (unit, name, line) VALUES (?1, ?2, ?3)"
            );
            pub(crate) const INSERT_REF: &str =
                concat!("INSERT INTO ", $p, "_ref (name, unit) VALUES (?1, ?2)");
            /// Definitions by name, with the unit's OID.
            pub(crate) const DEFS: &str = concat!(
                "SELECT d.name, d.line, e.oid, d.unit FROM ",
                $p,
                "_def d JOIN ",
                $p,
                "_ext e ON e.unit = d.unit WHERE d.name = ?1 ORDER BY e.oid, d.line"
            );
            /// The units of up to four OIDs; unused parameters are NULL.
            pub(crate) const UNITS: &str = concat!(
                "SELECT unit, oid FROM ",
                $p,
                "_ext WHERE oid IN (?1, ?2, ?3, ?4) ORDER BY oid"
            );
            pub(crate) const ALL_UNITS: &str =
                concat!("SELECT unit FROM ", $p, "_ext ORDER BY unit");

            pub(crate) static TABLES: Tables = Tables {
                namespace: Namespace {
                    name: $ns,
                    version: $version,
                    compression: Compression::None,
                },
                schema: &[
                    concat!(
                        "CREATE TABLE ",
                        $p,
                        "_ext (unit INTEGER PRIMARY KEY REFERENCES entry (id) ON DELETE CASCADE,",
                        " oid BLOB NOT NULL UNIQUE)"
                    ),
                    concat!(
                        "CREATE TABLE ",
                        $p,
                        "_def (unit INTEGER NOT NULL REFERENCES entry (id) ON DELETE CASCADE,",
                        " name TEXT NOT NULL, line INTEGER NOT NULL)"
                    ),
                    concat!("CREATE INDEX ", $p, "_def_name ON ", $p, "_def (name)"),
                    concat!("CREATE INDEX ", $p, "_def_unit ON ", $p, "_def (unit)"),
                    concat!(
                        "CREATE TABLE ",
                        $p,
                        "_ref (name TEXT NOT NULL,",
                        " unit INTEGER NOT NULL REFERENCES entry (id) ON DELETE CASCADE,",
                        " PRIMARY KEY (name, unit)) WITHOUT ROWID"
                    ),
                    concat!("CREATE INDEX ", $p, "_ref_unit ON ", $p, "_ref (unit)"),
                ],
                statements: &[INSERT_EXT, INSERT_DEF, INSERT_REF, DEFS, UNITS, ALL_UNITS],
                quota: $quota,
            };
        }
    };
}

tag_tables!(tags, "test.tags", 1, "t", 256 << 20);
tag_tables!(tags_v2, "test.tags", 2, "u", 256 << 20);
tag_tables!(small, "test.small", 1, "s", 256 << 10);
// The tables of `tags` with a 1 MiB quota, as a later build that lowered it.
tag_tables!(tags_1m, "test.tags", 1, "t", 1 << 20);

/// The table names of each registration, for inspecting the file.
const T: [&str; 3] = ["t_ext", "t_def", "t_ref"];
const U: [&str; 3] = ["u_ext", "u_def", "u_ref"];
const S: [&str; 3] = ["s_ext", "s_def", "s_ref"];

fn oid(i: u64) -> Vec<u8> {
    let mut oid = vec![0u8; 20];
    oid[..8].copy_from_slice(&i.to_be_bytes());
    oid
}

fn unit_key(i: u64) -> Key {
    Key::new().bytes(&oid(i)).str("rust")
}

/// Records unit `i` with `defs` definitions named `def<j>` and one reference
/// to `shared`, declaring `bytes` bytes.
fn add_unit(w: &mut TableWrite, m: Statements, i: u64, defs: u32, bytes: u64) {
    let unit = w.create(&unit_key(i), bytes);
    let oid = oid(i);
    w.execute(m.ext, &[Param::New(unit), Param::Bytes(&oid)]);
    for j in 0..defs {
        let name = format!("def{j}");
        w.execute(
            m.def,
            &[
                Param::New(unit),
                Param::Text(&name),
                Param::Int(i64::from(j) + 1),
            ],
        );
    }
    w.execute(m.r#ref, &[Param::Text("shared"), Param::New(unit)]);
}

#[derive(Clone, Copy)]
struct Statements {
    ext: &'static str,
    def: &'static str,
    r#ref: &'static str,
}

const TAGS: Statements = Statements {
    ext: tags::INSERT_EXT,
    def: tags::INSERT_DEF,
    r#ref: tags::INSERT_REF,
};
const SMALL: Statements = Statements {
    ext: small::INSERT_EXT,
    def: small::INSERT_DEF,
    r#ref: small::INSERT_REF,
};

/// Units `range` in one write, each with two definitions.
fn write_units(set: &TableSet, m: Statements, range: std::ops::Range<u64>, bytes: u64) -> Written {
    let mut w = set.write();
    for i in range {
        add_unit(&mut w, m, i, 2, bytes);
    }
    w.commit()
}

fn rows(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .expect("count rows")
}

/// Rows whose unit is not an entry: `PRAGMA foreign_key_check` reports them
/// even on a connection without foreign keys.
fn orphans(conn: &Connection) -> i64 {
    let mut stmt = conn.prepare("PRAGMA foreign_key_check").unwrap();
    let mut rows = stmt.query([]).unwrap();
    let mut n = 0;
    while rows.next().unwrap().is_some() {
        n += 1;
    }
    n
}

fn units_of(conn: &Connection, name: &str) -> i64 {
    conn.query_row(
        "SELECT count(*) FROM entry e JOIN namespace n ON e.ns = n.id WHERE n.name = ?1",
        [name],
        |r| r.get(0),
    )
    .unwrap()
}

fn custom_bytes(conn: &Connection, name: &str) -> Option<i64> {
    conn.query_row("SELECT bytes FROM custom WHERE name = ?1", [name], |r| {
        r.get(0)
    })
    .ok()
}

fn sum_sizes_of(conn: &Connection, name: &str, version: u32) -> i64 {
    conn.query_row(
        "SELECT coalesce(sum(e.size), 0) FROM entry e JOIN namespace n ON e.ns = n.id
         WHERE n.name = ?1 AND n.version = ?2",
        rusqlite::params![name, version],
        |r| r.get(0),
    )
    .unwrap()
}

fn has_table(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE name = ?1",
        [name],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
        == 1
}

/// The definitions named `name`: (line, OID's index, unit).
fn defs(set: &TableSet, name: &str) -> Option<Vec<(i64, u64, Unit)>> {
    set.read(|r| {
        let mut out = Vec::new();
        let ok = r.query(tags::DEFS, &[Param::Text(name)], |row| {
            assert_eq!(row.text(0), Some(name));
            let oid = row.bytes(2).unwrap();
            out.push((
                row.int(1).unwrap(),
                u64::from_be_bytes(oid[..8].try_into().unwrap()),
                row.unit(3).unwrap(),
            ));
            true
        });
        assert!(ok);
        out
    })
}

/// Bytes counted for a unit of `unit_key` declaring `bytes`.
fn unit_size(bytes: u64) -> u64 {
    unit_key(0).len() as u64 + bytes + ENTRY_OVERHEAD
}

fn project_budget(bytes: u64) -> Budgets {
    Budgets {
        project: bytes,
        ..Budgets::default()
    }
}

// Registration and versions.

#[test]
fn tables_handles_can_move_between_threads() {
    fn send_sync<T: Send + Sync>() {}
    fn send<T: Send>() {}
    send_sync::<TableSet>();
    send_sync::<Tables>();
    send::<TableWrite>();
}

#[test]
fn tables_handle_does_no_io_and_first_use_creates_them() {
    let f = Fixture::new();
    let set = f.repo().tables(&tags::TABLES);
    assert_eq!(set.evictions(), 0);
    assert!(!f.root().exists());

    // A read creates the tables; there is nothing in them yet.
    assert_eq!(defs(&set, "def0"), Some(Vec::new()));
    let conn = raw(f.repo().file());
    for table in T {
        assert!(has_table(&conn, table), "{table}");
    }
    let ns: i64 = conn
        .query_row("SELECT ns FROM custom WHERE name = 'test.tags'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(has_table(&conn, &format!("entry_ns{ns}")));
    let listed: String = conn
        .query_row("SELECT tables FROM custom WHERE ns = ?1", [ns], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(listed.lines().collect::<Vec<_>>(), T);
    assert_eq!(custom_bytes(&conn, "test.tags"), Some(0));
    // Creating them deleted nothing.
    assert_eq!(set.evictions(), 0);
}

#[test]
fn tables_version_bump_drops_the_old_tables_and_retires_their_units() {
    let f = Fixture::new();
    let p = f.repo();
    let v1 = p.tables(&tags::TABLES);
    assert_eq!(write_units(&v1, TAGS, 0..50, 1000), Written::Committed);
    assert_eq!(p.put(&key(1), &Blob(vec![1; 100])), Written::Committed);
    let conn = raw(p.file());
    assert_eq!(units_of(&conn, "test.tags"), 50);
    let before = v1.evictions();

    // The next program version: other tables under the same namespace.
    let v2 = p.tables(&tags_v2::TABLES);
    assert_eq!(v2.read(|_| ()), Some(()));
    for table in T {
        assert!(!has_table(&conn, table), "{table} dropped");
    }
    for table in U {
        assert!(has_table(&conn, table), "{table} created");
    }
    let retired: Vec<(i64, i64)> = conn
        .prepare("SELECT version, retired FROM namespace WHERE name = 'test.tags' ORDER BY version")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(retired, vec![(1, 1), (2, 0)]);
    assert!(v1.evictions() > before, "the old tables went");
    assert_eq!(custom_bytes(&conn, "test.tags"), Some(0));

    // The old units have no rows; write transactions delete them with the
    // other retired entries, and the bytes follow.
    assert_eq!(
        sum_sizes_of(&conn, "test.tags", 1),
        50 * unit_size(1000) as i64
    );
    assert_eq!(p.put(&key(2), &Blob(vec![2])), Written::Committed);
    assert_eq!(sum_sizes_of(&conn, "test.tags", 1), 0);
    assert_eq!(meta_bytes(&conn), sum_sizes(&conn));
    assert!(p.get::<Blob>(&key(1)).is_some());

    // An older program dropping the newer tables works the same way.
    assert_eq!(
        write_units(&v2_set(&p), TAGS_V2, 0..3, 10),
        Written::Committed
    );
    let mut f = f;
    f.reopen();
    let old = f.repo().tables(&tags::TABLES);
    assert_eq!(defs(&old, "def0"), Some(Vec::new()));
    assert!(has_table(&conn, "t_ext") && !has_table(&conn, "u_ext"));
    assert_eq!(orphans(&conn), 0);
}

const TAGS_V2: Statements = Statements {
    ext: tags_v2::INSERT_EXT,
    def: tags_v2::INSERT_DEF,
    r#ref: tags_v2::INSERT_REF,
};

fn v2_set(p: &Partition) -> TableSet {
    p.tables(&tags_v2::TABLES)
}

/// Registrations that break a rule, each under its own namespace.
mod broken {
    use crate::{Compression, Namespace, Tables};

    const fn ns(name: &'static str) -> Namespace {
        Namespace {
            name,
            version: 1,
            compression: Compression::None,
        }
    }

    pub(super) static NO_FOREIGN_KEY: Tables = Tables {
        namespace: ns("test.no_fk"),
        schema: &["CREATE TABLE b1 (unit INTEGER PRIMARY KEY, name TEXT)"],
        statements: &[],
        quota: 1 << 20,
    };
    pub(super) static NO_CASCADE: Tables = Tables {
        namespace: ns("test.no_cascade"),
        schema: &["CREATE TABLE b2 (unit INTEGER PRIMARY KEY REFERENCES entry (id))"],
        statements: &[],
        quota: 1 << 20,
    };
    pub(super) static NO_INDEX: Tables = Tables {
        namespace: ns("test.no_index"),
        schema: &[
            "CREATE TABLE b3 (name TEXT, unit INTEGER REFERENCES entry (id) ON DELETE CASCADE)",
        ],
        statements: &[],
        quota: 1 << 20,
    };
    pub(super) static TRIGGER: Tables = Tables {
        namespace: ns("test.trigger"),
        schema: &[
            "CREATE TABLE b4 (unit INTEGER PRIMARY KEY REFERENCES entry (id) ON DELETE CASCADE)",
            "CREATE TRIGGER b4_t AFTER INSERT ON b4 BEGIN SELECT 1; END",
        ],
        statements: &[],
        quota: 1 << 20,
    };
    pub(super) static BAD_STATEMENT: Tables = Tables {
        namespace: ns("test.bad_statement"),
        schema: &[
            "CREATE TABLE b5 (unit INTEGER PRIMARY KEY REFERENCES entry (id) ON DELETE CASCADE)",
        ],
        statements: &["SELECT nothing FROM b5"],
        quota: 1 << 20,
    };
    pub(super) static BAD_SCHEMA: Tables = Tables {
        namespace: ns("test.bad_schema"),
        schema: &["CREATE TABLE b6 (unit INTEGER PRIMARY KEY"],
        statements: &[],
        quota: 1 << 20,
    };
}

#[test]
fn tables_registration_breaking_a_rule_creates_nothing() {
    let f = Fixture::new();
    let p = f.repo();
    assert_eq!(p.put(&key(1), &Blob(vec![1])), Written::Committed);
    let conn = raw(p.file());
    let objects = |conn: &Connection| -> i64 {
        conn.query_row("SELECT count(*) FROM sqlite_schema", [], |r| r.get(0))
            .unwrap()
    };
    let before = objects(&conn);
    for (tables, table) in [
        (&broken::NO_FOREIGN_KEY, "b1"),
        (&broken::NO_CASCADE, "b2"),
        (&broken::NO_INDEX, "b3"),
        (&broken::TRIGGER, "b4"),
        (&broken::BAD_STATEMENT, "b5"),
        (&broken::BAD_SCHEMA, "b6"),
    ] {
        let set = p.tables(tables);
        assert_eq!(set.read(|_| ()), None, "{table}");
        let mut w = set.write();
        w.create(&unit_key(1), 10);
        assert_eq!(w.commit(), Written::Dropped(Dropped::Io), "{table}");
        assert!(!has_table(&conn, table), "{table}");
        assert_eq!(objects(&conn), before, "{table}");
    }
    // The partition is not disabled and nothing else changed.
    for _ in 0..3 {
        assert_eq!(p.put(&key(2), &Blob(vec![2])), Written::Committed);
    }
    assert_eq!(p.get::<Blob>(&key(1)), Some(Blob(vec![1])));
    assert_eq!(state(&f), PartitionState::Open);
    assert_eq!(meta_bytes(&conn), sum_sizes(&conn));
}

/// The state of the fixture's repository partition.
fn state(f: &Fixture) -> PartitionState {
    f.cache
        .stats()
        .partitions
        .iter()
        .find(|s| s.path.as_deref() == Some(Path::new("/work/lectern/.git")))
        .map(|s| s.state)
        .expect("the repository partition")
}

// Writes and reads.

#[test]
fn tables_write_then_read_back() {
    let f = Fixture::new();
    let p = f.repo();
    let set = p.tables(&tags::TABLES);
    let mut w = set.write();
    add_unit(&mut w, TAGS, 1, 2, 300);
    add_unit(&mut w, TAGS, 2, 1, 500);
    assert!(w.bytes() > 0);
    assert_eq!(w.commit(), Written::Committed);

    let found = defs(&set, "def0").unwrap();
    assert_eq!(found.len(), 2);
    assert_eq!((found[0].0, found[0].1), (1, 1));
    assert_eq!((found[1].0, found[1].1), (1, 2));
    assert_ne!(found[0].2, found[1].2);
    let second = defs(&set, "def1").unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].2, found[0].2, "Param::New is the unit's ID");

    // A fixed list of placeholders, unused ones NULL; accessors check types.
    let units = set
        .read(|r| {
            let mut out = Vec::new();
            let oid1 = oid(1);
            let oid9 = oid(9);
            assert!(r.query(
                tags::UNITS,
                &[
                    Param::Bytes(&oid1),
                    Param::Bytes(&oid9),
                    Param::Null,
                    Param::Null
                ],
                |row| {
                    assert_eq!(row.text(1), None);
                    assert_eq!(row.int(1), None);
                    assert_eq!(row.bytes(0), None);
                    assert_eq!(row.int(5), None);
                    out.push((row.unit(0).unwrap(), row.bytes(1).unwrap().to_vec()));
                    true
                }
            ));
            out
        })
        .unwrap();
    assert_eq!(units, vec![(found[0].2, oid(1))]);

    // The bytes: key + declared + 64 per unit, in meta.bytes and custom.bytes.
    let conn = raw(p.file());
    let expected = (unit_size(300) + unit_size(500)) as i64;
    assert_eq!(custom_bytes(&conn, "test.tags"), Some(expected));
    assert_eq!(meta_bytes(&conn), expected as u64);
    assert_eq!(meta_bytes(&conn), sum_sizes(&conn));
    assert_eq!(rows(&conn, "t_def"), 3);
    assert_eq!(rows(&conn, "t_ref"), 2);
    assert_eq!(set.evictions(), 0, "only new units");
    assert_eq!(f.cache.stats().written, 1);
}

#[test]
fn tables_create_replaces_the_unit_of_a_key() {
    let f = Fixture::new();
    let p = f.repo();
    let set = p.tables(&tags::TABLES);
    assert_eq!(write_units(&set, TAGS, 1..2, 100), Written::Committed);
    let first = defs(&set, "def0").unwrap()[0].2;
    let mut w = set.write();
    add_unit(&mut w, TAGS, 1, 5, 200);
    assert_eq!(w.commit(), Written::Committed);

    let conn = raw(p.file());
    assert_eq!(units_of(&conn, "test.tags"), 1);
    assert_eq!(rows(&conn, "t_def"), 5);
    let now = defs(&set, "def0").unwrap();
    assert_eq!(now.len(), 1);
    // `entry.id` is a rowid: the new unit may get the deleted unit's ID, so
    // a changed generation is what tells the user that IDs it read before
    // may name other units now.
    let _ = first;
    assert_eq!(
        custom_bytes(&conn, "test.tags"),
        Some(unit_size(200) as i64)
    );
    assert_eq!(set.evictions(), 1, "the old unit was deleted");
}

#[test]
fn tables_remove_deletes_the_rows() {
    let f = Fixture::new();
    let p = f.repo();
    let set = p.tables(&tags::TABLES);
    assert_eq!(write_units(&set, TAGS, 0..3, 100), Written::Committed);
    let units: Vec<Unit> = defs(&set, "def0").unwrap().iter().map(|d| d.2).collect();

    let mut w = set.write();
    w.remove(&unit_key(0)).remove_unit(units[1]);
    // Removing what does not exist is not an error.
    w.remove(&unit_key(99)).remove_unit(Unit::from_id(-5));
    assert_eq!(w.commit(), Written::Committed);
    assert_eq!(set.evictions(), 1);

    let conn = raw(p.file());
    assert_eq!(units_of(&conn, "test.tags"), 1);
    for table in T {
        assert_eq!(rows(&conn, table), if table == "t_def" { 2 } else { 1 });
    }
    assert_eq!(defs(&set, "def1").unwrap()[0].2, units[2]);
    assert_eq!(
        custom_bytes(&conn, "test.tags"),
        Some(unit_size(100) as i64)
    );
    assert_eq!(meta_bytes(&conn), sum_sizes(&conn));
    assert_eq!(orphans(&conn), 0);
}

#[test]
fn tables_statement_errors_roll_back_without_disabling() {
    let f = Fixture::new();
    let p = f.repo();
    let set = p.tables(&tags::TABLES);
    assert_eq!(write_units(&set, TAGS, 0..1, 100), Written::Committed);
    let conn = raw(p.file());
    let units_before = units_of(&conn, "test.tags");

    for round in 0..5 {
        let mut w = set.write();
        add_unit(&mut w, TAGS, 10 + round, 2, 100);
        match round % 3 {
            // A row for a unit that does not exist.
            0 => w.execute(
                tags::INSERT_DEF,
                &[Param::Int(9999), Param::Text("x"), Param::Int(1)],
            ),
            // The UNIQUE constraint on the OID.
            1 => w.execute(tags::INSERT_EXT, &[Param::Int(9999), Param::Bytes(&oid(0))]),
            // A wrong number of parameters.
            _ => w.execute(tags::INSERT_REF, &[Param::Text("x")]),
        };
        assert_eq!(w.commit(), Written::Dropped(Dropped::Io), "round {round}");
    }
    // Nothing of those writes is there, the unit created first included.
    assert_eq!(units_of(&conn, "test.tags"), units_before);
    assert_eq!(rows(&conn, "t_def"), 2);
    assert_eq!(orphans(&conn), 0);
    assert_eq!(f.cache.stats().dropped(Dropped::Io), 5);
    // More than three in a row, and the partition still works.
    assert_eq!(state(&f), PartitionState::Open);
    assert_eq!(write_units(&set, TAGS, 20..21, 100), Written::Committed);
    assert_eq!(units_of(&conn, "test.tags"), units_before + 1);
}

#[test]
fn tables_queries_only_read() {
    let f = Fixture::new();
    let p = f.repo();
    let set = p.tables(&tags::TABLES);
    assert_eq!(write_units(&set, TAGS, 0..1, 100), Written::Committed);
    // A registered statement that writes is refused on the read connection.
    let ran = set.read(|r| {
        let first = r.query(tags::INSERT_REF, &[Param::Text("x"), Param::Int(1)], |_| {
            true
        });
        // Later queries of the same read do not run.
        let second = r.query(tags::ALL_UNITS, &[], |_| true);
        (first, second)
    });
    assert_eq!(ran, None);
    // A unit placeholder means nothing in a query.
    let mut w = set.write();
    let new = w.create(&unit_key(5), 1);
    drop(w);
    assert_eq!(
        set.read(|r| r.query(tags::DEFS, &[Param::New(new)], |_| true)),
        None
    );
    // Stopping early is not a failure.
    let mut seen = 0;
    assert_eq!(
        set.read(|r| r.query(tags::DEFS, &[Param::Text("def0")], |_| {
            seen += 1;
            false
        })),
        Some(true)
    );
    assert_eq!(seen, 1);
    assert_eq!(state(&f), PartitionState::Open);
    assert_eq!(defs(&set, "def0").unwrap().len(), 1);
}

#[test]
fn tables_too_large_writes_are_dropped() {
    let f = Fixture::new();
    let set = f.repo().tables(&tags::TABLES);
    let mut w = set.write();
    w.create(&Key::new().bytes(&vec![0; MAX_KEY + 1]), 1);
    assert_eq!(w.commit(), Written::Dropped(Dropped::TooLarge));

    let mut w = set.write();
    let unit = w.create(&unit_key(1), 1);
    let big = vec![7u8; 1 << 20];
    for _ in 0..=(MAX_BATCH >> 20) {
        w.execute(tags::INSERT_EXT, &[Param::New(unit), Param::Bytes(&big)]);
    }
    assert!(w.bytes() > MAX_BATCH);
    assert_eq!(w.commit(), Written::Dropped(Dropped::TooLarge));
    assert_eq!(set.write().commit(), Written::Committed, "an empty write");
    assert!(!f.root().exists(), "nothing was opened");
}

#[test]
fn tables_on_the_ui_thread_do_no_io() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cache");
    let mut cfg = config(&root, Budgets::default());
    cfg.ui_thread = Some(std::thread::current().id());
    let cache = Cache::new(cfg);
    cache.set_ui_thread_panics(false);
    let set = cache
        .repo(Path::new("/work/lectern/.git"))
        .tables(&tags::TABLES);
    assert_eq!(set.read(|_| ()), None);
    set.touch(&[Unit::from_id(1)]);
    let mut w = set.write();
    w.create(&unit_key(1), 1);
    assert_eq!(w.commit(), Written::Dropped(Dropped::WrongThread));
    assert!(!root.exists());
}

// Access times.

fn atime_of(conn: &Connection, unit: Unit) -> i64 {
    conn.query_row("SELECT atime FROM entry WHERE id = ?1", [unit.id()], |r| {
        r.get(0)
    })
    .unwrap()
}

#[test]
fn tables_touch_is_written_back_like_a_hit() {
    let f = Fixture::new();
    let p = f.repo();
    let set = p.tables(&tags::TABLES);
    assert_eq!(write_units(&set, TAGS, 0..2, 100), Written::Committed);
    let written = minute(&f.clock);
    let units: Vec<Unit> = defs(&set, "def0").unwrap().iter().map(|d| d.2).collect();
    let conn = raw(p.file());

    // Within the hour: recorded, but the write-back leaves it alone.
    f.clock.advance(30 * MINUTE);
    set.touch(&units[..1]);
    assert_eq!(p.slot.pending_touched(), 1);
    assert_eq!(p.put(&key(1), &Blob(vec![1])), Written::Committed);
    assert_eq!(p.slot.pending_touched(), 0);
    assert_eq!(atime_of(&conn, units[0]), written);

    // After an hour: written back by the next write, and only the touched.
    f.clock.advance(2 * HOUR);
    set.touch(&[units[0], units[0]]);
    assert_eq!(p.slot.pending_touched(), 1);
    assert_eq!(atime_of(&conn, units[0]), written, "memory only");
    assert_eq!(p.put(&key(2), &Blob(vec![2])), Written::Committed);
    assert_eq!(atime_of(&conn, units[0]), minute(&f.clock));
    assert_eq!(atime_of(&conn, units[1]), written);
    assert_eq!(set.evictions(), 0);
}

#[test]
fn tables_touch_writes_back_when_the_set_is_full() {
    let f = Fixture::new();
    let p = f.repo();
    let set = p.tables(&tags::TABLES);
    let n = TOUCHED_MAX as u64 + 904;
    let mut w = set.write();
    for i in 0..n {
        let unit = w.create(&unit_key(i), 10);
        w.execute(tags::INSERT_EXT, &[Param::New(unit), Param::Bytes(&oid(i))]);
    }
    assert_eq!(w.commit(), Written::Committed);
    let units: Vec<Unit> = set
        .read(|r| {
            let mut out = Vec::new();
            r.query(tags::ALL_UNITS, &[], |row| {
                out.push(row.unit(0).unwrap());
                true
            });
            out
        })
        .unwrap();
    assert_eq!(units.len() as u64, n);

    f.clock.advance(2 * HOUR);
    let now = minute(&f.clock);
    set.touch(&units);
    let conn = raw(p.file());
    let fresh = |conn: &Connection| -> i64 {
        conn.query_row("SELECT count(*) FROM entry WHERE atime = ?1", [now], |r| {
            r.get(0)
        })
        .unwrap()
    };
    // The first 4096 were written back in the call; the rest wait.
    assert_eq!(fresh(&conn), TOUCHED_MAX as i64);
    assert_eq!(p.slot.pending_touched(), 904);
    assert_eq!(p.put(&key(1), &Blob(vec![1])), Written::Committed);
    // All units, and the entry just written.
    assert_eq!(fresh(&conn), n as i64 + 1);
}

/// The write-back runs one statement per `TOUCH_CHUNK` IDs: across several
/// chunks and a partial last one, units over an hour old get the new access
/// time, units written within the hour keep theirs, and deleted units are
/// skipped.
#[test]
fn tables_touch_write_back_spans_chunks_and_skips_recent_units() {
    let f = Fixture::new();
    let p = f.repo();
    let set = p.tables(&tags::TABLES);
    let all_units = |set: &TableSet| -> Vec<Unit> {
        set.read(|r| {
            let mut out = Vec::new();
            r.query(tags::ALL_UNITS, &[], |row| {
                out.push(row.unit(0).unwrap());
                true
            });
            out
        })
        .unwrap()
    };
    let old_n = 2 * TOUCH_CHUNK as u64 + 7;
    let mut w = set.write();
    for i in 0..old_n {
        let unit = w.create(&unit_key(i), 10);
        w.execute(tags::INSERT_EXT, &[Param::New(unit), Param::Bytes(&oid(i))]);
    }
    assert_eq!(w.commit(), Written::Committed);
    let old = all_units(&set);
    assert_eq!(old.len() as u64, old_n);
    let written_old = minute(&f.clock);

    // Two hours later: ten new units, and five old ones deleted.
    f.clock.advance(2 * HOUR);
    let mut w = set.write();
    for i in old_n..old_n + 10 {
        let unit = w.create(&unit_key(i), 10);
        w.execute(tags::INSERT_EXT, &[Param::New(unit), Param::Bytes(&oid(i))]);
    }
    for &unit in &old[..5] {
        w.remove_unit(unit);
    }
    assert_eq!(w.commit(), Written::Committed);
    let written_new = minute(&f.clock);
    let new: Vec<Unit> = all_units(&set)
        .into_iter()
        .filter(|u| !old.contains(u))
        .collect();
    assert_eq!(new.len(), 10);

    // 30 minutes later every unit, deleted ones included, is touched.
    f.clock.advance(30 * MINUTE);
    let mut touched = old.clone();
    touched.extend(&new);
    set.touch(&touched);
    assert_eq!(p.slot.pending_touched(), touched.len());
    assert_eq!(p.put(&key(1), &Blob(vec![1])), Written::Committed);
    assert_eq!(p.slot.pending_touched(), 0);

    let conn = raw(p.file());
    let now = minute(&f.clock);
    assert!(now > written_new && written_new > written_old);
    for &unit in &old[5..] {
        assert_eq!(atime_of(&conn, unit), now, "{unit:?}");
    }
    for &unit in &new {
        assert_eq!(atime_of(&conn, unit), written_new, "{unit:?}");
    }
    let gone: i64 = conn
        .query_row(
            "SELECT count(*) FROM entry WHERE id IN (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params_from_iter(old[..5].iter().map(|u| u.id())),
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(gone, 0);

    // The statement looks units up by ID, not by a scan of `entry_atime`.
    let nulls = vec![rusqlite::types::Null; 2 + TOUCH_CHUNK];
    let mut plan = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {TOUCH_SQL}"))
        .unwrap();
    let detail: Vec<String> = plan
        .query_map(rusqlite::params_from_iter(nulls), |r| r.get(3))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(detail.len(), 1, "{detail:?}");
    assert!(detail[0].contains("INTEGER PRIMARY KEY"), "{detail:?}");
}

// Eviction.

#[test]
fn tables_budget_eviction_deletes_units_with_their_rows() {
    let budget = 1 << 20;
    let f = Fixture::with_budgets(project_budget(budget));
    let p = f.repo();
    let set = p.tables(&tags::TABLES);
    // 40 units of 16 KiB, a minute apart, then entries newer than all.
    for i in 0..40 {
        assert_eq!(
            write_units(&set, TAGS, i..i + 1, 16 << 10),
            Written::Committed
        );
        f.clock.advance(MINUTE);
    }
    let before = set.evictions();
    for i in 0..10 {
        assert_eq!(
            p.put(&key(i), &Blob(noise(i, 64 << 10))),
            Written::Committed
        );
    }
    let conn = raw(p.file());
    let left = units_of(&conn, "test.tags");
    assert!(left < 40, "{left}");
    assert!(meta_bytes(&conn) <= budget);
    // The oldest units went, rows and all.
    assert_eq!(rows(&conn, "t_ext"), left);
    assert_eq!(rows(&conn, "t_def"), 2 * left);
    assert_eq!(orphans(&conn), 0);
    let oldest_left = defs(&set, "def0").unwrap()[0].1;
    assert_eq!(oldest_left, 40 - left as u64);
    assert!(set.evictions() > before);
    assert_eq!(
        custom_bytes(&conn, "test.tags"),
        Some(sum_sizes_of(&conn, "test.tags", 1))
    );
    assert_eq!(meta_bytes(&conn), sum_sizes(&conn));
    for i in 0..10 {
        assert!(p.get::<Blob>(&key(i)).is_some());
    }
}

#[test]
fn tables_quota_evicts_the_namespace_without_touching_other_entries() {
    // 256 KiB quota in a 1 MiB partition: 512 KiB of entries fit beside it.
    let budget = 1 << 20;
    let f = Fixture::with_budgets(project_budget(budget));
    let p = f.repo();
    for i in 0..8 {
        assert_eq!(
            p.put(&key(i), &Blob(noise(i, 64 << 10))),
            Written::Committed
        );
    }
    f.clock.advance(MINUTE);
    let set = p.tables(&small::TABLES);
    let quota = small::TABLES.quota as i64;
    let conn = raw(p.file());
    let before = set.evictions();
    // 2 MiB of units in writes of 64 KiB, newer than every entry.
    for i in 0..32 {
        assert_eq!(
            write_units(&set, SMALL, i * 4..i * 4 + 4, 16 << 10),
            Written::Committed
        );
        let used = custom_bytes(&conn, "test.small").unwrap();
        assert!(used <= quota, "write {i}: {used}");
        f.clock.advance(MINUTE);
    }
    assert!(set.evictions() > before);
    // No entry was evicted: the partition never went over its budget.
    for i in 0..8 {
        assert!(p.get::<Blob>(&key(i)).is_some(), "entry {i}");
    }
    assert_eq!(
        f.cache.stats().evicted_bytes as i64 + custom_bytes(&conn, "test.small").unwrap(),
        { 128 * unit_size(16 << 10) as i64 }
    );
    // The newest units are left, with their rows.
    let left = units_of(&conn, "test.small");
    let newest: Vec<i64> = conn
        .prepare("SELECT unit FROM s_ext ORDER BY unit")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(newest.len() as i64, left);
    assert_eq!(rows(&conn, "s_def"), 2 * left);
    let oldest_oid: Vec<u8> = conn
        .query_row("SELECT oid FROM s_ext ORDER BY unit LIMIT 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(oldest_oid, oid(128 - left as u64));
    assert_eq!(orphans(&conn), 0);
    assert_eq!(meta_bytes(&conn), sum_sizes(&conn));
    for table in S {
        assert!(has_table(&conn, table));
    }
}

#[test]
fn tables_quota_eviction_per_transaction_follows_the_write() {
    let f = Fixture::new();
    let p = f.repo();
    // 4 MiB of units under the 256 MiB quota, then the quota is 1 MiB.
    for i in 0..16 {
        assert_eq!(
            write_units(
                &p.tables(&tags::TABLES),
                TAGS,
                i * 16..i * 16 + 16,
                16 << 10
            ),
            Written::Committed
        );
    }
    let set = p.tables(&tags_1m::TABLES);
    let conn = raw(p.file());
    let unit = unit_size(16 << 10) as i64;
    let mut used = custom_bytes(&conn, "test.tags").unwrap();
    assert!(used > 4 << 20, "{used}");

    // A small write deletes 256 KiB, give or take a unit.
    let evicted = |f: &Fixture| f.cache.stats().evicted_bytes as i64;
    let before = evicted(&f);
    assert_eq!(write_units(&set, TAGS, 1000..1001, 100), Written::Committed);
    let freed = evicted(&f) - before;
    assert!(
        ((256 << 10)..(256 << 10) + unit).contains(&freed),
        "{freed}"
    );
    let now = custom_bytes(&conn, "test.tags").unwrap();
    assert!(now < used);
    used = now;

    // A larger one deletes what it wrote: the namespace does not grow.
    let before = evicted(&f);
    assert_eq!(
        write_units(&set, TAGS, 2000..2040, 16 << 10),
        Written::Committed
    );
    let freed = evicted(&f) - before;
    let wrote = 40 * unit;
    assert!((wrote..wrote + unit).contains(&freed), "{freed}");
    assert!(custom_bytes(&conn, "test.tags").unwrap() <= used);

    // Small writes get it back under the quota, the oldest first.
    for i in 0..20 {
        assert_eq!(
            write_units(&set, TAGS, 3000 + i..3001 + i, 100),
            Written::Committed
        );
    }
    assert!(custom_bytes(&conn, "test.tags").unwrap() <= 1 << 20);
    assert_eq!(rows(&conn, "t_ext"), units_of(&conn, "test.tags"));
    assert_eq!(orphans(&conn), 0);
    assert_eq!(
        custom_bytes(&conn, "test.tags"),
        Some(sum_sizes_of(&conn, "test.tags", 1))
    );
}

// The eviction generation.

#[test]
fn tables_evictions_follow_rebuilds_and_lost_files() {
    let mut f = Fixture::new();
    let set = f.repo().tables(&tags::TABLES);
    assert_eq!(write_units(&set, TAGS, 0..3, 100), Written::Committed);
    // A new file on the first open of the run lost nothing.
    assert_eq!(set.evictions(), 0);
    // Reads, touches, closing and reopening keep it.
    let units: Vec<Unit> = defs(&set, "def0").unwrap().iter().map(|d| d.2).collect();
    set.touch(&units);
    f.cache.close();
    assert_eq!(defs(&set, "def0").unwrap().len(), 3);
    assert_eq!(set.evictions(), 0);

    // The file deleted while closed: the next open finds a new file.
    f.cache.close();
    for file in super::support::files(f.repo().file()) {
        let _ = std::fs::remove_file(file);
    }
    assert_eq!(defs(&set, "def0"), Some(Vec::new()));
    assert_eq!(set.evictions(), 1);

    // A file of another schema version is rebuilt.
    assert_eq!(write_units(&set, TAGS, 0..3, 100), Written::Committed);
    f.cache.close();
    raw(f.repo().file())
        .execute_batch("PRAGMA user_version = 1")
        .unwrap();
    assert_eq!(defs(&set, "def0"), Some(Vec::new()));
    assert_eq!(set.evictions(), 2);
    assert_eq!(f.cache.stats().rebuilt, 1);

    // Another handle of the same partition shares the generation; another
    // run starts again.
    assert_eq!(f.repo().tables(&tags::TABLES).evictions(), 2);
    f.reopen();
    assert_eq!(f.repo().tables(&tags::TABLES).evictions(), 0);
}

// Rollbacks and crashes.

#[test]
fn tables_failed_write_leaves_no_rows() {
    let f = Fixture::new();
    let p = f.repo();
    let set = p.tables(&tags::TABLES);
    assert_eq!(write_units(&set, TAGS, 0..2, 100), Written::Committed);
    let conn = raw(p.file());
    let before: Vec<i64> = T.iter().map(|t| rows(&conn, t)).collect();

    // The disk fills up in the middle of a large write.
    p.set_max_page_count(Some(1));
    let mut w = set.write();
    for i in 10..400 {
        add_unit(&mut w, TAGS, i, 20, 100);
    }
    assert_eq!(w.commit(), Written::Dropped(Dropped::DiskFull));
    p.set_max_page_count(None);

    let after: Vec<i64> = T.iter().map(|t| rows(&conn, t)).collect();
    assert_eq!(after, before);
    assert_eq!(units_of(&conn, "test.tags"), 2);
    assert_eq!(orphans(&conn), 0);
    assert_eq!(
        custom_bytes(&conn, "test.tags"),
        Some(sum_sizes_of(&conn, "test.tags", 1))
    );
    assert_eq!(meta_bytes(&conn), sum_sizes(&conn));
}

const CRASH_ROOT: &str = "LECTERN_CACHE_TEST_TABLES_CRASH_ROOT";

/// The child process of `tables_crash_in_a_write_leaves_no_rows`. Does
/// nothing unless the parent set `CRASH_ROOT`.
#[test]
fn tables_crash_child_aborts_inside_a_write() {
    let Some(root) = std::env::var_os(CRASH_ROOT) else {
        return;
    };
    let cache = Cache::new(config(root.as_ref(), Budgets::default()));
    let p = cache.repo(Path::new("/work/lectern/.git"));
    let set = p.tables(&tags::TABLES);
    assert_eq!(write_units(&set, TAGS, 0..20, 100), Written::Committed);
    // The next write dies after its rows are written, before the commit.
    p.set_write_hook(Some(Arc::new(|| std::process::abort())));
    let _ = write_units(&set, TAGS, 100..300, 100);
    unreachable!("the write hook aborts");
}

#[test]
fn tables_crash_in_a_write_leaves_no_rows() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cache");
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "tests::tables::tables_crash_child_aborts_inside_a_write",
            "--exact",
            "--test-threads=1",
        ])
        .env(CRASH_ROOT, &root)
        .status()
        .unwrap();
    assert!(!status.success(), "the child aborts: {status:?}");

    let cache = Cache::new(config(&root, Budgets::default()));
    let p = cache.repo(Path::new("/work/lectern/.git"));
    let set = p.tables(&tags::TABLES);
    assert_eq!(defs(&set, "def0").unwrap().len(), 20);
    let conn = raw(p.file());
    assert_eq!(units_of(&conn, "test.tags"), 20);
    assert_eq!(rows(&conn, "t_ext"), 20);
    assert_eq!(rows(&conn, "t_def"), 40);
    assert_eq!(rows(&conn, "t_ref"), 20);
    assert_eq!(orphans(&conn), 0);
    let check: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(check, "ok");
    assert_eq!(
        custom_bytes(&conn, "test.tags"),
        Some(sum_sizes_of(&conn, "test.tags", 1))
    );
    assert_eq!(meta_bytes(&conn), sum_sizes(&conn));
}

#[test]
fn tables_registration_compression_is_unused() {
    // Units have no value; a namespace that asks for compression still works.
    static PACKED: Tables = Tables {
        namespace: Namespace {
            name: "test.packed_tables",
            version: 1,
            compression: Compression::Zstd,
        },
        schema: &[
            "CREATE TABLE z_ext (unit INTEGER PRIMARY KEY REFERENCES entry (id) ON DELETE CASCADE)",
        ],
        statements: &["INSERT INTO z_ext (unit) VALUES (?1)"],
        quota: 1 << 20,
    };
    let f = Fixture::new();
    let set = f.repo().tables(&PACKED);
    let mut w = set.write();
    let unit = w.create(&unit_key(1), 10_000);
    w.execute(PACKED.statements[0], &[Param::New(unit)]);
    assert_eq!(w.commit(), Written::Committed);
    let conn = raw(f.repo().file());
    assert_eq!(rows(&conn, "z_ext"), 1);
}
