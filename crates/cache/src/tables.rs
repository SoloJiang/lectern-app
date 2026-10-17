//! Custom tables ("自定义表" in the design): a user's own SQLite tables in a
//! partition, whose rows belong to units. A unit is an `entry` row with a key
//! and no value; it counts toward the budget and the LRU like any entry, and
//! deleting it deletes its rows through a cascading foreign key in the same
//! statement. No rusqlite type appears in this interface.

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::slot::Writes;
use crate::{Dropped, Key, MAX_BATCH, MAX_KEY, Namespace, Partition, Written};

/// A registered set of custom tables, owned by one namespace. Users define
/// it as a `static` and take handles with [`Partition::tables`].
#[derive(Debug)]
pub struct Tables {
    /// The units' namespace, registered in the design doc's namespace table.
    /// `version` is also the version of the tables: bumped when the tables,
    /// the columns the statements use, or the units' keys change. The old
    /// version's tables are dropped on first use. `compression` is unused:
    /// units have no value.
    pub namespace: Namespace,
    /// `CREATE TABLE` and `CREATE INDEX` statements, run in order in one
    /// write transaction on first use. Each table has a column declared
    /// `REFERENCES entry (id) ON DELETE CASCADE` with an index on it (or
    /// being its `INTEGER PRIMARY KEY`); nothing but these tables and their
    /// indexes may be created.
    pub schema: &'static [&'static str],
    /// The statements [`TableRead::query`] and [`TableWrite::execute`] may
    /// run. Each is compiled once when the tables are created.
    pub statements: &'static [&'static str],
    /// Most bytes (`entry.size`) of this namespace's units in one partition.
    /// Over it, the namespace's own writes evict its oldest units first.
    pub quota: u64,
}

impl Tables {
    fn registers(&self, sql: &'static str) -> bool {
        self.statements
            .iter()
            .any(|s| std::ptr::eq(*s, sql) || *s == sql)
    }
}

/// The ID of a committed unit: its `entry.id`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Unit(i64);

impl Unit {
    /// A unit ID as read from a custom table's unit column.
    pub fn from_id(id: i64) -> Unit {
        Unit(id)
    }

    pub fn id(self) -> i64 {
        self.0
    }
}

/// A unit created by [`TableWrite::create`]; [`Param::New`] stands for its
/// ID until the write commits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NewUnit(u32);

/// A statement parameter, bound in order to `?1`, `?2`, …
#[derive(Clone, Copy, Debug)]
pub enum Param<'a> {
    Null,
    Int(i64),
    Bytes(&'a [u8]),
    Text(&'a str),
    /// The ID of a unit created earlier in the same [`TableWrite`]. Only in
    /// [`TableWrite::execute`]; a query using it fails.
    New(NewUnit),
}

/// A result row of [`TableRead::query`].
pub struct Row<'a> {
    pub(crate) row: &'a rusqlite::Row<'a>,
}

impl Row<'_> {
    fn value(&self, column: usize) -> Option<rusqlite::types::ValueRef<'_>> {
        self.row.get_ref(column).ok()
    }

    /// Column `column` (from 0) as an integer. `None` for NULL, another type
    /// or a column that does not exist; the same for the other accessors.
    pub fn int(&self, column: usize) -> Option<i64> {
        match self.value(column)? {
            rusqlite::types::ValueRef::Integer(v) => Some(v),
            _ => None,
        }
    }

    pub fn bytes(&self, column: usize) -> Option<&[u8]> {
        match self.value(column)? {
            rusqlite::types::ValueRef::Blob(b) => Some(b),
            _ => None,
        }
    }

    /// `None` also for text that is not UTF-8.
    pub fn text(&self, column: usize) -> Option<&str> {
        match self.value(column)? {
            rusqlite::types::ValueRef::Text(t) => std::str::from_utf8(t).ok(),
            _ => None,
        }
    }

    pub fn unit(&self, column: usize) -> Option<Unit> {
        self.int(column).map(Unit)
    }
}

/// The queries of one [`TableSet::read`], in one read transaction.
pub struct TableRead<'a> {
    pub(crate) conn: &'a rusqlite::Connection,
    pub(crate) tables: &'static Tables,
    /// The first failure; later queries do not run.
    pub(crate) failed: Option<rusqlite::Error>,
}

impl TableRead<'_> {
    /// Runs a registered read-only statement and calls `row` for each result
    /// row until it returns false. Returns false when the statement is not
    /// registered or not read-only, or the query failed; later queries of
    /// this read do not run and the read returns `None`.
    pub fn query(
        &mut self,
        sql: &'static str,
        params: &[Param<'_>],
        mut row: impl FnMut(&Row<'_>) -> bool,
    ) -> bool {
        if self.failed.is_some() {
            return false;
        }
        debug_assert!(
            self.tables.registers(sql),
            "lectern-cache: statement not registered in {}: {sql}",
            self.tables.namespace.name
        );
        let result = if self.tables.registers(sql) {
            crate::custom::query(self.conn, sql, params, &mut row)
        } else {
            Err(rusqlite::Error::InvalidQuery)
        };
        match result {
            Ok(()) => true,
            Err(e) => {
                self.failed = Some(e);
                false
            }
        }
    }
}

/// A set of custom tables in one partition. `Clone + Send + Sync`; holds no
/// connection.
#[derive(Clone)]
pub struct TableSet {
    pub(crate) partition: Partition,
    pub(crate) tables: &'static Tables,
    evictions: Arc<AtomicU64>,
}

impl Partition {
    /// The handle of `tables` in this partition. No IO; any thread. The
    /// tables are created by the first read or write through it.
    pub fn tables(&self, tables: &'static Tables) -> TableSet {
        debug_assert!(
            tables.namespace.is_valid(),
            "namespace names use ASCII lowercase letters, digits, '.' and '_'"
        );
        TableSet {
            partition: self.clone(),
            tables,
            evictions: self.slot.evictions(tables.namespace.name),
        }
    }
}

impl TableSet {
    /// The eviction generation: changes when this process deletes units of
    /// this namespace in this partition (eviction, quota, `remove`, a unit
    /// replaced by `create`), drops an old version's tables, or the
    /// partition is deleted and rebuilt. One atomic load; any thread.
    pub fn evictions(&self) -> u64 {
        self.evictions.load(Ordering::Acquire)
    }

    /// Opens a read transaction on the read connection and passes it to `f`.
    /// Blocking; executor tasks only. Creates the tables on first use. `None`
    /// when the partition is unavailable, the tables cannot be created, or a
    /// query of `f` failed: what `f` saw is then incomplete.
    pub fn read<T>(&self, f: impl FnOnce(&mut TableRead<'_>) -> T) -> Option<T> {
        self.partition.slot.read_tables(self.tables, f)
    }

    /// Marks units as used without reading them, as a read hit does: their
    /// access time is written back by the next write transaction, unless it
    /// is under 60 minutes old by then. Usually memory only; when the set of
    /// pending access times is full (4096), it is written back in this call.
    /// Blocking; executor tasks only, after the results a user waits for.
    pub fn touch(&self, units: &[Unit]) {
        let ids: Vec<i64> = units.iter().map(|u| u.0).collect();
        self.partition.slot.touch_units(ids);
    }

    /// Starts a write. No IO.
    pub fn write(&self) -> TableWrite {
        TableWrite {
            set: self.clone(),
            ops: TableOps::default(),
            bytes: 0,
            too_large: false,
            unregistered: false,
        }
    }
}

/// Creates, statements and removals that `commit` applies in one write
/// transaction, all or none. `Send`: a task can build it up and commit it
/// later.
pub struct TableWrite {
    set: TableSet,
    ops: TableOps,
    bytes: u64,
    too_large: bool,
    unregistered: bool,
}

/// The recorded operations; parameters are copied into one buffer.
#[derive(Default)]
pub(crate) struct TableOps {
    pub(crate) ops: Vec<TableOp>,
    pub(crate) params: Vec<Stored>,
    pub(crate) data: Vec<u8>,
    /// Units created so far.
    pub(crate) units: u32,
}

pub(crate) enum TableOp {
    Create {
        key: Key,
        bytes: u64,
    },
    Execute {
        sql: &'static str,
        params: Range<usize>,
    },
    Remove(Key),
    RemoveUnit(i64),
}

/// A parameter as recorded: byte ranges are into `TableOps::data`.
#[derive(Clone, Copy)]
pub(crate) enum Stored {
    Null,
    Int(i64),
    Bytes(usize, usize),
    Text(usize, usize),
    New(u32),
}

impl TableWrite {
    /// Creates a unit under `key`, counted as `len(key) + bytes + 64` bytes.
    /// An existing unit with this key is deleted first, with its rows. No IO.
    pub fn create(&mut self, key: &Key, bytes: u64) -> NewUnit {
        self.too_large |= key.len() > MAX_KEY;
        self.bytes += key.len() as u64;
        let unit = NewUnit(self.ops.units);
        self.ops.units += 1;
        self.ops.ops.push(TableOp::Create {
            key: key.clone(),
            bytes,
        });
        unit
    }

    /// Records a registered statement and copies its parameters; statements
    /// run in the order recorded. No IO.
    pub fn execute(&mut self, sql: &'static str, params: &[Param<'_>]) -> &mut TableWrite {
        debug_assert!(
            self.set.tables.registers(sql),
            "lectern-cache: statement not registered in {}: {sql}",
            self.set.tables.namespace.name
        );
        self.unregistered |= !self.set.tables.registers(sql);
        let start = self.ops.params.len();
        for param in params {
            let stored = match *param {
                Param::Null => Stored::Null,
                Param::Int(v) => {
                    self.bytes += 8;
                    Stored::Int(v)
                }
                Param::Bytes(b) => Stored::Bytes(self.copy(b), b.len()),
                Param::Text(t) => Stored::Text(self.copy(t.as_bytes()), t.len()),
                Param::New(NewUnit(i)) => {
                    self.bytes += 8;
                    Stored::New(i)
                }
            };
            self.ops.params.push(stored);
        }
        self.ops.ops.push(TableOp::Execute {
            sql,
            params: start..self.ops.params.len(),
        });
        self
    }

    fn copy(&mut self, b: &[u8]) -> usize {
        let at = self.ops.data.len();
        self.ops.data.extend_from_slice(b);
        self.bytes += b.len() as u64;
        at
    }

    /// Deletes the unit of `key` and its rows.
    pub fn remove(&mut self, key: &Key) -> &mut TableWrite {
        self.too_large |= key.len() > MAX_KEY;
        self.bytes += key.len() as u64;
        self.ops.ops.push(TableOp::Remove(key.clone()));
        self
    }

    /// Deletes a unit of this namespace and its rows.
    pub fn remove_unit(&mut self, unit: Unit) -> &mut TableWrite {
        self.bytes += 8;
        self.ops.ops.push(TableOp::RemoveUnit(unit.0));
        self
    }

    /// Bytes recorded so far (keys plus parameters, 8 per integer); callers
    /// split writes so they stay within `MAX_BATCH`.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Commits in one write transaction, like `Batch::commit`. Blocking;
    /// executor tasks only. A failing statement of the user (a constraint, a
    /// row for a unit that does not exist, a wrong parameter count) rolls the
    /// whole write back and returns `Dropped::Io` without counting toward
    /// disabling the partition.
    pub fn commit(self) -> Written {
        let slot = &self.set.partition.slot;
        let shared = &slot.shared;
        if !shared.may_block() {
            return slot.dropped(shared.blocked_reason());
        }
        if self.too_large || self.bytes > MAX_BATCH {
            return slot.dropped(Dropped::TooLarge);
        }
        if self.unregistered {
            return slot.dropped(Dropped::Io);
        }
        if self.ops.ops.is_empty() {
            return Written::Committed;
        }
        slot.write(Writes::Tables(self.set.tables, &self.ops))
    }
}
