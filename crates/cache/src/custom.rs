//! Custom tables at the SQLite level: creating a registration's tables and
//! checking them, running its statements, and the unit operations inside a
//! write transaction. Error handling is in `slot`.

use std::collections::HashSet;

use rusqlite::types::{ToSqlOutput, ValueRef};
use rusqlite::{Connection, ErrorCode, OptionalExtension, Statement, TransactionBehavior, params};

use crate::db::{self, ENTRY_OVERHEAD, Tally};
use crate::tables::{Param, Row, Stored, TableOp, TableOps, Tables};

/// The result of making sure a registration's tables exist.
pub(crate) enum Attach {
    /// The tables of this version exist; `ns` is the units' namespace ID.
    /// `replaced`: another version's tables were dropped to create them.
    Ready { ns: i64, replaced: bool },
    /// The registration breaks a rule; nothing was created.
    Invalid(String),
}

/// The namespace ID and version of the tables registered under `name`.
fn find(conn: &Connection, name: &str) -> rusqlite::Result<Option<(i64, i64)>> {
    conn.prepare_cached("SELECT ns, version FROM custom WHERE name = ?1")?
        .query_row(params![name], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
}

/// Makes sure the tables of `tables` exist at its version, on the write
/// connection: a lookup when they do; otherwise one write transaction that
/// drops another version's tables, creates these and checks them.
pub(crate) fn attach(conn: &mut Connection, tables: &Tables) -> rusqlite::Result<Attach> {
    let name = tables.namespace.name;
    let version = i64::from(tables.namespace.version);
    if let Some((ns, v)) = find(conn, name)?
        && v == version
    {
        return Ok(Attach::Ready {
            ns,
            replaced: false,
        });
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let replaced = match find(&tx, name)? {
        // Another process created them in the meantime.
        Some((ns, v)) if v == version => {
            return Ok(Attach::Ready {
                ns,
                replaced: false,
            });
        }
        Some((old, _)) => {
            drop_tables(&tx, old)?;
            true
        }
        None => false,
    };
    match create(&tx, tables)? {
        Ok(ns) => {
            tx.commit()?;
            Ok(Attach::Ready { ns, replaced })
        }
        // Dropping the transaction rolls it back.
        Err(why) => Ok(Attach::Invalid(why)),
    }
}

/// Drops the tables of namespace `ns` and its index on `entry`, and retires
/// the namespace: its units have no rows left, and write transactions delete
/// them like any retired entry.
fn drop_tables(conn: &Connection, ns: i64) -> rusqlite::Result<()> {
    let tables: String = conn.query_row("SELECT tables FROM custom WHERE ns = ?1", [ns], |r| {
        r.get(0)
    })?;
    for table in tables.lines() {
        conn.execute_batch(&format!("DROP TABLE IF EXISTS {}", quote(table)))?;
    }
    conn.execute_batch(&format!("DROP INDEX IF EXISTS entry_ns{ns}"))?;
    conn.execute("DELETE FROM custom WHERE ns = ?1", [ns])?;
    conn.execute("UPDATE namespace SET retired = 1 WHERE id = ?1", [ns])?;
    Ok(())
}

/// Runs the schema, checks what it created, and records it. `Err(why)` for
/// a registration that breaks a rule.
fn create(conn: &Connection, tables: &Tables) -> rusqlite::Result<Result<i64, String>> {
    let name = tables.namespace.name;
    let before: HashSet<String> = objects(conn)?.into_iter().map(|o| o.name).collect();
    for sql in tables.schema {
        if let Err(e) = conn.execute_batch(sql) {
            return statement_error(e, || format!("{name}: schema failed: {sql}"));
        }
    }
    let created: Vec<Object> = objects(conn)?
        .into_iter()
        .filter(|o| !before.contains(&o.name))
        .collect();
    if let Err(why) = check(conn, &created) {
        return Ok(Err(format!("{name}: {why}")));
    }
    let table_names: Vec<&str> = created
        .iter()
        .filter(|o| o.kind == "table")
        .map(|o| o.name.as_str())
        .collect();

    let ns = db::assign_namespace(conn, &tables.namespace)?;
    // One version per partition: the one created last.
    conn.execute(
        "UPDATE namespace SET retired = (id <> ?1) WHERE name = ?2",
        params![ns, name],
    )?;
    // Units of this version left from before its tables were dropped (two
    // program versions taking turns) have no rows; they stay counted.
    let bytes: i64 = conn.query_row(
        "SELECT coalesce(sum(size), 0) FROM entry WHERE ns = ?1",
        [ns],
        |r| r.get(0),
    )?;
    // For quota eviction: this namespace's units by access time.
    conn.execute_batch(&format!(
        "CREATE INDEX IF NOT EXISTS entry_ns{ns} ON entry (atime) WHERE ns = {ns}"
    ))?;
    conn.execute(
        "INSERT INTO custom (ns, name, version, bytes, tables) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            ns,
            name,
            tables.namespace.version,
            bytes,
            table_names.join("\n")
        ],
    )?;
    for sql in tables.statements {
        if let Err(e) = conn.prepare(sql) {
            return statement_error(e, || format!("{name}: statement does not compile: {sql}"));
        }
    }
    Ok(Ok(ns))
}

/// A registration's mistake becomes `Err(why)`; other failures (busy, full,
/// corrupt) stay errors.
fn statement_error(
    e: rusqlite::Error,
    why: impl FnOnce() -> String,
) -> rusqlite::Result<Result<i64, String>> {
    if is_statement_error(&e) {
        Ok(Err(format!("{}: {e}", why())))
    } else {
        Err(e)
    }
}

struct Object {
    kind: String,
    name: String,
    table: String,
}

fn objects(conn: &Connection) -> rusqlite::Result<Vec<Object>> {
    let mut stmt = conn.prepare("SELECT type, name, tbl_name FROM sqlite_schema")?;
    stmt.query_map([], |r| {
        Ok(Object {
            kind: r.get(0)?,
            name: r.get(1)?,
            table: r.get(2)?,
        })
    })?
    .collect()
}

/// The rules of a schema: only tables and indexes on them; every table has a
/// cascading foreign key to `entry`, with an index on its column.
fn check(conn: &Connection, created: &[Object]) -> Result<(), String> {
    let tables: Vec<&str> = created
        .iter()
        .filter(|o| o.kind == "table")
        .map(|o| o.name.as_str())
        .collect();
    if tables.is_empty() {
        return Err("the schema creates no table".to_owned());
    }
    for o in created {
        let allowed = match o.kind.as_str() {
            "table" => !o.name.contains('\n'),
            "index" => tables.contains(&o.table.as_str()),
            _ => false,
        };
        if !allowed {
            return Err(format!("the schema may not create {} {}", o.kind, o.name));
        }
    }
    for table in tables {
        let column = unit_column(conn, table)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| {
                format!("{table} has no column REFERENCES entry (id) ON DELETE CASCADE")
            })?;
        let plan = plan(
            conn,
            &format!(
                "SELECT 1 FROM {} WHERE {} = 0",
                quote(table),
                quote(&column)
            ),
        )
        .map_err(|e| e.to_string())?;
        if plan.iter().any(|step| step.starts_with("SCAN")) {
            return Err(format!("{table}.{column} has no index"));
        }
    }
    Ok(())
}

/// The column of `table` that references `entry (id)` with `ON DELETE
/// CASCADE`.
fn unit_column(conn: &Connection, table: &str) -> rusqlite::Result<Option<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA foreign_key_list({})", quote(table)))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let parent: String = row.get("table")?;
        let from: String = row.get("from")?;
        let to: Option<String> = row.get("to")?;
        let on_delete: String = row.get("on_delete")?;
        if parent.eq_ignore_ascii_case("entry")
            && on_delete.eq_ignore_ascii_case("CASCADE")
            && to.is_none_or(|to| to.eq_ignore_ascii_case("id"))
        {
            return Ok(Some(from));
        }
    }
    Ok(None)
}

/// The steps of `EXPLAIN QUERY PLAN` for `sql`.
fn plan(conn: &Connection, sql: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
    stmt.query_map([], |r| r.get::<_, String>(3))?.collect()
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Errors of a user's statement rather than of the file: SQLite's generic
/// error (no such table, syntax), constraints (a row for a unit that does
/// not exist), type mismatches, parameter ranges, and rusqlite's own checks
/// (parameter count, rows from `execute`).
pub(crate) fn is_statement_error(e: &rusqlite::Error) -> bool {
    match e {
        rusqlite::Error::SqliteFailure(f, _) => matches!(
            f.code,
            ErrorCode::Unknown
                | ErrorCode::ConstraintViolation
                | ErrorCode::TypeMismatch
                | ErrorCode::ParameterOutOfRange
                | ErrorCode::ApiMisuse
        ),
        _ => true,
    }
}

fn bind<'v>(
    stmt: &mut Statement<'_>,
    values: impl ExactSizeIterator<Item = rusqlite::Result<ValueRef<'v>>>,
) -> rusqlite::Result<()> {
    let expected = stmt.parameter_count();
    if values.len() != expected {
        return Err(rusqlite::Error::InvalidParameterCount(
            values.len(),
            expected,
        ));
    }
    for (i, value) in values.enumerate() {
        stmt.raw_bind_parameter(i + 1, ToSqlOutput::Borrowed(value?))?;
    }
    Ok(())
}

/// Runs a read-only statement on the read connection.
pub(crate) fn query(
    conn: &Connection,
    sql: &str,
    params: &[Param<'_>],
    row: &mut dyn FnMut(&Row<'_>) -> bool,
) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare_cached(sql)?;
    if !stmt.readonly() {
        return Err(rusqlite::Error::InvalidQuery);
    }
    bind(
        &mut stmt,
        params.iter().map(|p| match *p {
            Param::Null => Ok(ValueRef::Null),
            Param::Int(v) => Ok(ValueRef::Integer(v)),
            Param::Bytes(b) => Ok(ValueRef::Blob(b)),
            Param::Text(t) => Ok(ValueRef::Text(t.as_bytes())),
            Param::New(_) => Err(rusqlite::Error::InvalidQuery),
        }),
    )?;
    let mut rows = stmt.raw_query();
    while let Some(r) = rows.next()? {
        if !row(&Row { row: r }) {
            break;
        }
    }
    Ok(())
}

/// Applies the operations of a `TableWrite` on namespace `ns`. A failing
/// user statement sets `statement_failed`.
pub(crate) fn apply(
    conn: &Connection,
    ns: i64,
    ops: &TableOps,
    atime: i64,
    tally: &mut Tally,
    statement_failed: &mut bool,
) -> rusqlite::Result<()> {
    let mut units: Vec<i64> = Vec::with_capacity(ops.units as usize);
    for op in &ops.ops {
        match op {
            TableOp::Create { key, bytes } => {
                let key = key.as_bytes();
                if let Some(size) = conn
                    .prepare_cached("DELETE FROM entry WHERE ns = ?1 AND key = ?2 RETURNING size")?
                    .query_row(params![ns, key], |r| r.get::<_, i64>(0))
                    .optional()?
                {
                    tally.delete(ns, size);
                }
                let size = (key.len() as u64)
                    .saturating_add(*bytes)
                    .saturating_add(ENTRY_OVERHEAD);
                let size = i64::try_from(size).unwrap_or(i64::MAX);
                conn.prepare_cached(
                    "INSERT INTO entry (ns, key, codec, size, atime, value)
                     VALUES (?1, ?2, 0, ?3, ?4, x'')",
                )?
                .execute(params![ns, key, size, atime])?;
                units.push(conn.last_insert_rowid());
                tally.write(ns, size);
            }
            TableOp::Execute { sql, params } => {
                let run = || -> rusqlite::Result<()> {
                    let mut stmt = conn.prepare_cached(sql)?;
                    bind(
                        &mut stmt,
                        ops.params[params.clone()].iter().map(|p| match *p {
                            Stored::Null => Ok(ValueRef::Null),
                            Stored::Int(v) => Ok(ValueRef::Integer(v)),
                            Stored::Bytes(at, len) => Ok(ValueRef::Blob(&ops.data[at..at + len])),
                            Stored::Text(at, len) => Ok(ValueRef::Text(&ops.data[at..at + len])),
                            Stored::New(i) => units
                                .get(i as usize)
                                .map(|id| ValueRef::Integer(*id))
                                .ok_or(rusqlite::Error::InvalidQuery),
                        }),
                    )?;
                    stmt.raw_execute()?;
                    Ok(())
                };
                if let Err(e) = run() {
                    *statement_failed |= is_statement_error(&e);
                    return Err(e);
                }
            }
            TableOp::Remove(key) => {
                if let Some(size) = conn
                    .prepare_cached("DELETE FROM entry WHERE ns = ?1 AND key = ?2 RETURNING size")?
                    .query_row(params![ns, key.as_bytes()], |r| r.get::<_, i64>(0))
                    .optional()?
                {
                    tally.delete(ns, size);
                }
            }
            TableOp::RemoveUnit(id) => {
                if let Some(size) = conn
                    .prepare_cached("DELETE FROM entry WHERE id = ?1 AND ns = ?2 RETURNING size")?
                    .query_row(params![id, ns], |r| r.get::<_, i64>(0))
                    .optional()?
                {
                    tally.delete(ns, size);
                }
            }
        }
    }
    Ok(())
}

/// `custom.bytes` of namespace `ns`; 0 when another process dropped these
/// tables since this session found them (the user's statements then fail).
pub(crate) fn bytes(conn: &Connection, ns: i64) -> rusqlite::Result<i64> {
    Ok(conn
        .prepare_cached("SELECT bytes FROM custom WHERE ns = ?1")?
        .query_row([ns], |r| r.get(0))
        .optional()?
        .unwrap_or(0))
}

/// Deletes the oldest units of namespace `ns` while it is over `target` and
/// fewer than `most` bytes were freed by this transaction (`freed`).
pub(crate) fn evict_quota(
    conn: &Connection,
    ns: i64,
    mut ns_bytes: i64,
    target: i64,
    most: i64,
    freed: &mut i64,
    tally: &mut Tally,
) -> rusqlite::Result<()> {
    let mut oldest = conn.prepare_cached(&format!(
        "SELECT id, size FROM entry INDEXED BY entry_ns{ns} WHERE ns = {ns}
         ORDER BY atime, id LIMIT 64"
    ))?;
    let mut delete = conn.prepare_cached("DELETE FROM entry WHERE id = ?1")?;
    while ns_bytes > target && *freed < most {
        let victims: Vec<(i64, i64)> = oldest
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        if victims.is_empty() {
            break;
        }
        for (id, size) in victims {
            delete.execute([id])?;
            tally.evict(ns, size);
            ns_bytes -= size;
            *freed += size;
            if ns_bytes <= target || *freed >= most {
                break;
            }
        }
    }
    Ok(())
}
