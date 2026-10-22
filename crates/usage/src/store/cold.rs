//! The hot / cold split (docs/design/usage.md「冷热分层」).
//!
//! The hot window ends at the local midnight 364 days before today. A UTC
//! month wholly before that cutoff minus one day (room for any offset) is
//! archived: its rows of the timestamped tables move into `cold_part`
//! segments, in a fixed table order (the sources with their `usage_value`
//! rows, the turns, the facts last), one segment per transaction. The
//! month's state lives in `meta` (`archive:<month>`): hot (no row),
//! archiving (1) or archived (2). An archiving or archived month is frozen:
//! ingest still writes observations into it, but nothing writes its
//! `usage_value` or `fact` rows except the archive step itself
//! ([`dedup::derive_month`]), so a month half archived when the process
//! ends is neither counted twice nor missing.
//!
//! Each step reads one segment's hot rows, encodes and compresses them,
//! decodes the result and compares it with the rows, then inserts the
//! segment and deletes the rows in the same transaction. The `fact`
//! segments update `cold_month` and `cold_total` in that transaction; the
//! transaction that leaves the month without hot rows marks it archived.
//! A step that fails rolls back; the next run goes on from the first table
//! that still has hot rows.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;

use lectern_logging::{error, info};
use lectern_services::usage::LocalDate;
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

#[cfg(test)]
use super::lock;
use super::segment::{self, Encoder, Segment};
use super::{Store, StoreError, dedup};
use crate::LocalZone;
use crate::window::{self, BUCKET_MS};

/// UTC months since 1970-01: `year * 12 + month - 1`. Local months
/// (`cold_month.ym`) use the same numbering.
pub(crate) type Month = i64;

/// The largest uncompressed segment. A segment's time goes mostly into
/// deleting its hot rows (random B-tree pages of the key and session
/// indexes under a 1 MiB page cache), so 1 MiB keeps one step within
/// `usage/archive_month`'s 150 ms (docs/design/usage.md「与设计的差异」).
pub(crate) const SEGMENT_BYTES: usize = 1 << 20;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// Days the hot window has, today included (docs/design/usage.md「热窗口」:
/// the cutoff is the local midnight 364 days before today).
pub(crate) const HOT_DAYS: u32 = 365;

/// `meta` values of a month's state.
const ARCHIVING: i64 = 1;
const ARCHIVED: i64 = 2;

/// `cold_total.kind`.
pub(crate) const TOTAL_SESSION: i64 = 0;
pub(crate) const TOTAL_SEGMENT: i64 = 1;

/// The archived tables, in their fixed order: observations first, `fact`
/// last (it carries the summaries). The derived `usage_value` has no
/// timestamp and is looked up by key only: its rows go with their sources'
/// segment, in the same transaction ([`read_values`]). Each table with its
/// condition for a month's rows (`?1`, `?2`: the month's UTC range in
/// milliseconds), the order that follows its timestamp index, the column
/// that deletes them, and its timestamp column.
struct Table {
    name: &'static str,
    month: &'static str,
    order: &'static str,
    key: &'static str,
    at: &'static str,
}

const TABLES: &[Table] = &[
    Table {
        name: "usage_source",
        month: "at >= ?1 AND at < ?2",
        order: "at, id",
        key: "id",
        at: "at",
    },
    Table {
        name: "turn",
        month: "started_at >= ?1 AND started_at < ?2",
        order: "started_at, id",
        key: "id",
        at: "started_at",
    },
    Table {
        name: "fact",
        month: "at >= ?1 AND at < ?2",
        order: "at, id",
        key: "id",
        at: "at",
    },
];

/// The extra column of a `fact` segment: the local month its row was summed
/// into (`cold_month.ym`), so a restore takes back exactly what was added.
const COLD_YM: &str = "cold_ym";

pub(crate) fn month_of(at_ms: i64) -> Month {
    ym(window::date_of(at_ms.div_euclid(DAY_MS)))
}

fn ym(d: LocalDate) -> Month {
    i64::from(d.year) * 12 + i64::from(d.month) - 1
}

/// The (year, month) of a month number.
pub(crate) fn year_month(m: Month) -> (i32, u8) {
    let year = m.div_euclid(12);
    (
        i32::try_from(year).unwrap_or(i32::MAX),
        (m.rem_euclid(12) + 1) as u8,
    )
}

fn first_day(m: Month) -> i64 {
    let (year, month) = year_month(m);
    window::day_number(LocalDate {
        year,
        month,
        day: 1,
    })
}

/// A UTC month in milliseconds, half open.
pub(crate) fn month_range(m: Month) -> Range<i64> {
    first_day(m) * DAY_MS..first_day(m + 1) * DAY_MS
}

/// The start of the hot window: the local midnight 364 days before today.
pub(crate) fn cutoff(now_ms: i64, zone: &dyn LocalZone) -> i64 {
    window::last_days(HOT_DAYS, now_ms, zone)[0].1.start
}

/// The frozen months (archiving or archived), merged into ranges.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Frozen {
    ranges: Vec<Range<i64>>,
}

impl Frozen {
    pub(crate) fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    pub(crate) fn contains(&self, at_ms: i64) -> bool {
        let i = self.ranges.partition_point(|r| r.end <= at_ms);
        self.ranges.get(i).is_some_and(|r| r.contains(&at_ms))
    }

    /// An SQL condition that holds when `column` falls in a frozen month
    /// (`0` when none is).
    pub(crate) fn sql(&self, column: &str) -> String {
        if self.ranges.is_empty() {
            return "0".to_owned();
        }
        let parts: Vec<String> = self
            .ranges
            .iter()
            .map(|r| format!("({column} >= {} AND {column} < {})", r.start, r.end))
            .collect();
        format!("({})", parts.join(" OR "))
    }
}

/// The states of every month that is not hot.
fn states(conn: &Connection) -> rusqlite::Result<BTreeMap<Month, i64>> {
    let mut stmt = conn.prepare_cached(
        "SELECT name, value FROM meta WHERE name >= 'archive:' AND name < 'archive;'",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    let mut out = BTreeMap::new();
    for row in rows {
        let (name, value) = row?;
        if let Some(m) = name.strip_prefix("archive:").and_then(|m| m.parse().ok()) {
            out.insert(m, value);
        }
    }
    Ok(out)
}

/// The frozen months, read inside the caller's transaction.
pub(crate) fn frozen(conn: &Connection) -> rusqlite::Result<Frozen> {
    let mut ranges: Vec<Range<i64>> = Vec::new();
    for (m, state) in states(conn)? {
        if state != ARCHIVING && state != ARCHIVED {
            continue;
        }
        let r = month_range(m);
        match ranges.last_mut() {
            Some(last) if last.end == r.start => last.end = r.end,
            _ => ranges.push(r),
        }
    }
    Ok(Frozen { ranges })
}

/// The highest source ID when month `m` was frozen or its late records
/// last counted (`archive_mark:<month>`): sources above it arrived while
/// the month was frozen.
fn mark(conn: &Connection, m: Month) -> rusqlite::Result<Option<i64>> {
    conn.prepare_cached("SELECT value FROM meta WHERE name = ?1")?
        .query_row(params![format!("archive_mark:{m}")], |r| r.get(0))
        .optional()
}

fn set_mark(conn: &Connection, m: Month) -> rusqlite::Result<()> {
    let top: i64 = conn
        .prepare_cached("SELECT coalesce(max(id), 0) FROM usage_source")?
        .query_row([], |r| r.get(0))?;
    super::set_meta(conn, &format!("archive_mark:{m}"), top)
}

fn set_state(conn: &Connection, m: Month, state: Option<i64>) -> rusqlite::Result<()> {
    let name = format!("archive:{m}");
    match state {
        Some(v) => super::set_meta(conn, &name, v),
        None => {
            conn.execute("DELETE FROM meta WHERE name = ?1", params![name])?;
            Ok(())
        }
    }
}

/// Whether a session's records of a frozen month are already in the
/// database and must not be written again: archived ones (`cold_seen`),
/// or, during a replay, hot ones of an older generation.
pub(crate) fn session_has_month(
    conn: &Connection,
    session: i64,
    target: i64,
    replaying: bool,
    m: Month,
) -> rusqlite::Result<bool> {
    let seen: bool = conn
        .prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM cold_seen WHERE session = ?1 AND month = ?2)",
        )?
        .query_row(params![session, m], |r| r.get(0))?;
    if seen || !replaying {
        return Ok(seen);
    }
    let r = month_range(m);
    conn.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM usage_source
                        WHERE session = ?1 AND gen <> ?2 AND at >= ?3 AND at < ?4)",
    )?
    .query_row(params![session, target, r.start, r.end], |r| r.get(0))
}

/// Points where a test can end an archive step, as if the process ended:
/// just after each of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fault {
    /// The segment is inserted, the hot rows not yet deleted.
    Inserted,
    /// The hot rows are deleted, the summaries not yet updated.
    Deleted,
    /// The summaries are updated, nothing committed yet.
    Summarized,
}

/// What one archive step did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Stepped {
    /// The month was set to archiving.
    Started,
    /// One segment was written.
    Segment {
        table: &'static str,
        rows: usize,
        raw_bytes: usize,
        packed_bytes: usize,
        /// The month has no hot rows left and is archived.
        finished: bool,
    },
    /// The month had no hot rows left; it is archived.
    Finished,
    /// The decoded segment differed from the rows: nothing was deleted, the
    /// month stays archiving until a new segment format.
    Mismatch,
}

/// What [`Store::archive_all`] did.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Archived {
    pub(crate) months: usize,
    pub(crate) segments: usize,
    pub(crate) rows: usize,
    pub(crate) raw_bytes: usize,
    pub(crate) packed_bytes: usize,
}

/// When to give free pages back (docs/design/usage.md「空间回收」).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Reclaim {
    /// Free pages must be more than this fraction (numerator, denominator)
    /// of the file...
    pub(crate) fraction: (u64, u64),
    /// ...and more than this many bytes.
    pub(crate) min_bytes: u64,
    /// Pages one step gives back at most.
    pub(crate) pages: u32,
}

pub(crate) const RECLAIM: Reclaim = Reclaim {
    fraction: (1, 4),
    min_bytes: 64 << 20,
    pages: 2048,
};

/// Millionths per kind.
type Micro = [i64; 4];

fn add(into: &mut Micro, from: &Micro, sign: i64) {
    for (a, b) in into.iter_mut().zip(from) {
        *a = a.saturating_add(b.saturating_mul(sign));
    }
}

/// `cold_month` and `cold_total` changes of some fact rows.
#[derive(Default)]
struct Summaries {
    months: HashMap<(i64, i64, i64, i64, i64), Micro>,
    totals: HashMap<(i64, i64, i64), Micro>,
}

impl Summaries {
    /// Adds (`sign` 1) or takes back (-1) one fact row.
    fn fact(&mut self, row: &FactRow, sign: i64) {
        let key = (row.location, row.ym, row.agent, row.model, row.review);
        add(self.months.entry(key).or_default(), &row.micro, sign);
        let session = (TOTAL_SESSION, row.session, row.review);
        add(self.totals.entry(session).or_default(), &row.micro, sign);
        if let Some(segment) = row.segment {
            let key = (TOTAL_SEGMENT, segment, row.review);
            add(self.totals.entry(key).or_default(), &row.micro, sign);
        }
    }

    fn write(&self, conn: &Connection) -> rusqlite::Result<()> {
        let mut months = conn.prepare_cached(
            "INSERT INTO cold_month (location, ym, agent, model, review,
                                     input, output, cache_read, cache_write)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT (location, ym, agent, model, review) DO UPDATE SET
               input = input + excluded.input, output = output + excluded.output,
               cache_read = cache_read + excluded.cache_read,
               cache_write = cache_write + excluded.cache_write",
        )?;
        for (k, m) in &self.months {
            months.execute(params![k.0, k.1, k.2, k.3, k.4, m[0], m[1], m[2], m[3]])?;
        }
        let mut totals = conn.prepare_cached(
            "INSERT INTO cold_total (kind, target, review, input, output, cache_read, cache_write)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (kind, target, review) DO UPDATE SET
               input = input + excluded.input, output = output + excluded.output,
               cache_read = cache_read + excluded.cache_read,
               cache_write = cache_write + excluded.cache_write",
        )?;
        for (k, m) in &self.totals {
            totals.execute(params![k.0, k.1, k.2, m[0], m[1], m[2], m[3]])?;
        }
        // Rows that went back to zero (a restore) are not kept.
        conn.execute_batch(
            "DELETE FROM cold_month WHERE input = 0 AND output = 0 AND cache_read = 0 AND cache_write = 0;
             DELETE FROM cold_total WHERE input = 0 AND output = 0 AND cache_read = 0 AND cache_write = 0;",
        )
    }
}

/// The parts of a fact row the summaries use.
struct FactRow {
    at: i64,
    location: i64,
    ym: i64,
    agent: i64,
    model: i64,
    review: i64,
    session: i64,
    segment: Option<i64>,
    micro: Micro,
}

/// Column indexes of a `fact` row.
struct FactColumns {
    at: usize,
    location: usize,
    agent: usize,
    model: usize,
    review: usize,
    session: usize,
    segment: usize,
    tokens: [usize; 4],
    ym: Option<usize>,
}

impl FactColumns {
    fn of(columns: &[String]) -> Option<FactColumns> {
        let find = |name: &str| columns.iter().position(|c| c == name);
        Some(FactColumns {
            at: find("at")?,
            location: find("location")?,
            agent: find("agent")?,
            model: find("model")?,
            review: find("review")?,
            session: find("session")?,
            segment: find("segment")?,
            tokens: [
                find("input")?,
                find("output")?,
                find("cache_read")?,
                find("cache_write")?,
            ],
            ym: find(COLD_YM),
        })
    }

    fn row(&self, row: &[Value], zone: &dyn LocalZone) -> FactRow {
        let int = |i: usize| match row.get(i) {
            Some(Value::Integer(v)) => Some(*v),
            _ => None,
        };
        let at = int(self.at).unwrap_or(0);
        let ym_now = || ym(window::local_date(at, zone));
        FactRow {
            at,
            location: int(self.location).unwrap_or(0),
            ym: self.ym.and_then(int).unwrap_or_else(ym_now),
            agent: int(self.agent).unwrap_or(0),
            model: int(self.model).unwrap_or(0),
            review: int(self.review).unwrap_or(0),
            session: int(self.session).unwrap_or(0),
            segment: int(self.segment),
            micro: self.tokens.map(|i| int(i).unwrap_or(0)),
        }
    }
}

/// The zone of rows that carry their local month.
#[cfg(any(test, feature = "test-support"))]
struct Utc;

#[cfg(any(test, feature = "test-support"))]
impl LocalZone for Utc {
    fn offset_at(&self, _: i64) -> i32 {
        0
    }
}

fn injected() -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR), None)
}

/// An archived month has no hot facts: its `fact_bucket` rows go.
fn drop_buckets(tx: &Connection, range: &Range<i64>) -> rusqlite::Result<()> {
    tx.prepare_cached("DELETE FROM fact_bucket WHERE bucket >= ?1 AND bucket < ?2")?
        .execute(params![
            range.start.div_euclid(BUCKET_MS),
            range.end.div_euclid(BUCKET_MS)
        ])?;
    Ok(())
}

/// Whether any archived table has hot rows in the month.
fn has_hot_rows(conn: &Connection, range: &Range<i64>) -> rusqlite::Result<bool> {
    for t in TABLES {
        let sql = format!("SELECT EXISTS (SELECT 1 FROM {} WHERE {})", t.name, t.month);
        if conn
            .prepare_cached(&sql)?
            .query_row(params![range.start, range.end], |r| r.get::<_, bool>(0))?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The months with hot rows wholly before `limit`, from the indexes.
fn months_with_rows(conn: &Connection, limit: i64) -> rusqlite::Result<HashSet<Month>> {
    let mut out = HashSet::new();
    for t in TABLES {
        let at = t.at;
        let sql = format!("SELECT min({at}) FROM {} WHERE {at} >= ?1", t.name);
        let mut stmt = conn.prepare_cached(&sql)?;
        let mut from = i64::MIN;
        loop {
            let Some(first): Option<i64> = stmt.query_row(params![from], |r| r.get(0))? else {
                break;
            };
            let m = month_of(first);
            let r = month_range(m);
            if r.end > limit {
                break;
            }
            out.insert(m);
            from = r.end;
        }
    }
    Ok(out)
}

impl Store {
    /// The months to archive now, oldest first: those already archiving,
    /// then every month wholly before the cutoff minus a day that still has
    /// hot rows (archived months among them hold records that arrived
    /// later). A month whose check failed with this segment format waits for
    /// another one. Blocking.
    pub(crate) fn archive_plan(
        &self,
        now_ms: i64,
        zone: &dyn LocalZone,
    ) -> Result<Vec<Month>, StoreError> {
        let limit = cutoff(now_ms, zone) - DAY_MS;
        self.read(|conn| {
            let states = states(conn)?;
            let failed = failed(conn)?;
            let mut months: HashSet<Month> = months_with_rows(conn, limit)?;
            months.extend(
                states
                    .iter()
                    .filter(|(_, s)| **s == ARCHIVING)
                    .map(|(m, _)| *m),
            );
            let mut out: Vec<Month> = months
                .into_iter()
                .filter(|m| failed.get(m) != Some(&i64::from(segment::FORMAT)))
                .collect();
            out.sort_unstable();
            Ok(out)
        })
    }

    /// One step of archiving month `m`: setting it to archiving, or one
    /// segment of the first table that still has hot rows in it. `zone`
    /// places the facts in their local months. Blocking, one transaction.
    pub(crate) fn archive_step(
        &self,
        m: Month,
        now_ms: i64,
        zone: &dyn LocalZone,
    ) -> Result<Stepped, StoreError> {
        let limit = self
            .segment_bytes
            .load(std::sync::atomic::Ordering::Relaxed);
        let version = i64::from(self.schema.version);
        let range = month_range(m);
        let out = self.write(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if states(&tx)?.get(&m) != Some(&ARCHIVING) {
                // Every source so far went through the hot path; the mark
                // stays from an earlier round.
                if mark(&tx, m)?.is_none() {
                    set_mark(&tx, m)?;
                }
                set_state(&tx, m, Some(ARCHIVING))?;
                tx.commit()?;
                return Ok(Stepped::Started);
            }
            // Records that arrived while the month was frozen get their
            // facts now, before their table goes cold.
            let derived = dedup::derive_month(&tx, &range, mark(&tx, m)?.unwrap_or(0))?;
            set_mark(&tx, m)?;
            if derived > 0 {
                info!("late records of an archived month counted"; month = m, keys = derived);
            }
            let mut found = None;
            for t in TABLES {
                let sql = format!("SELECT EXISTS (SELECT 1 FROM {} WHERE {})", t.name, t.month);
                if tx
                    .prepare_cached(&sql)?
                    .query_row(params![range.start, range.end], |r| r.get::<_, bool>(0))?
                {
                    found = Some(t);
                    break;
                }
            }
            let Some(table) = found else {
                set_state(&tx, m, Some(ARCHIVED))?;
                drop_buckets(&tx, &range)?;
                tx.commit()?;
                return Ok(Stepped::Finished);
            };
            let (fault, mismatch) = self.take_faults();
            let parts = read_segment(&tx, table, &range, limit, version, zone)?;
            // Decoded again and compared with the rows read, before any of
            // them is deleted.
            let ok = parts.iter().all(Read::checks);
            if mismatch || !ok {
                drop(tx);
                return Ok(Stepped::Mismatch);
            }
            for part in &parts {
                let seq: i64 = tx
                    .prepare_cached(
                        "SELECT coalesce(max(seq) + 1, 0) FROM cold_part WHERE month = ?1 AND tbl = ?2",
                    )?
                    .query_row(params![m, part.table], |r| r.get(0))?;
                tx.prepare_cached(
                    "INSERT INTO cold_part (month, tbl, seq, rows, raw_bytes, data, written_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )?
                .execute(params![
                    m,
                    part.table,
                    seq,
                    part.segment.rows.len() as i64,
                    part.raw.len() as i64,
                    &part.packed,
                    now_ms
                ])?;
            }
            if fault == Some(Fault::Inserted) {
                return Err(injected());
            }
            for part in &parts {
                part.delete(&tx, table, &range)?;
            }
            if fault == Some(Fault::Deleted) {
                return Err(injected());
            }
            for part in &parts {
                summarize(&tx, part.table, &part.segment, m, zone)?;
            }
            if fault == Some(Fault::Summarized) {
                return Err(injected());
            }
            let finished = !has_hot_rows(&tx, &range)?;
            if finished {
                set_state(&tx, m, Some(ARCHIVED))?;
                drop_buckets(&tx, &range)?;
            }
            tx.commit()?;
            Ok(Stepped::Segment {
                table: table.name,
                rows: parts.iter().map(|p| p.segment.rows.len()).sum(),
                raw_bytes: parts.iter().map(|p| p.raw.len()).sum(),
                packed_bytes: parts.iter().map(|p| p.packed.len()).sum(),
                finished,
            })
        })?;
        if out == Stepped::Mismatch {
            error!("cold segment check failed; the month stays hot"; month = m);
            self.write(|conn| {
                super::set_meta(
                    conn,
                    &format!("archive_failed:{m}"),
                    i64::from(segment::FORMAT),
                )
            })?;
        }
        Ok(out)
    }

    /// The faults a test set for the next segment step.
    #[cfg(test)]
    fn take_faults(&self) -> (Option<Fault>, bool) {
        let mut faults = lock(&self.faults);
        (faults.archive.take(), std::mem::take(&mut faults.mismatch))
    }

    #[cfg(not(test))]
    fn take_faults(&self) -> (Option<Fault>, bool) {
        (None, false)
    }

    /// Archives every month the plan gives, to the end (tests and bench).
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn archive_all(
        &self,
        now_ms: i64,
        zone: &dyn LocalZone,
    ) -> Result<Archived, StoreError> {
        let mut out = Archived::default();
        for m in self.archive_plan(now_ms, zone)? {
            out.months += 1;
            loop {
                match self.archive_step(m, now_ms, zone)? {
                    Stepped::Started => {}
                    Stepped::Segment {
                        rows,
                        raw_bytes,
                        packed_bytes,
                        finished,
                        ..
                    } => {
                        out.segments += 1;
                        out.rows += rows;
                        out.raw_bytes += raw_bytes;
                        out.packed_bytes += packed_bytes;
                        if finished {
                            break;
                        }
                    }
                    Stepped::Finished | Stepped::Mismatch => break,
                }
            }
        }
        Ok(out)
    }

    /// The full rebuild (docs/design/usage.md「重建」): every frozen month's
    /// observations go back into the hot tables, one month per transaction
    /// (its segments deleted, its facts taken back out of the summaries,
    /// the month hot again), then the derived tables are rebuilt. The next
    /// archive steps archive those months again. Blocking. Only tests and
    /// bench call it until the derived algorithm has a second version.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn unarchive_all(&self) -> Result<usize, StoreError> {
        let months: Vec<Month> = self.read(|c| Ok(states(c)?.into_keys().collect()))?;
        for &m in &months {
            self.write(|conn| {
                let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
                restore_month(&tx, m)?;
                tx.commit()
            })?;
            self.cold_reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        self.write(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            dedup::rebuild_in(&tx)?;
            tx.commit()
        })?;
        info!("cold months restored for a full rebuild"; months = months.len());
        Ok(months.len())
    }

    /// Rebuilds the hot window's derived tables from the observations
    /// (frozen months untouched). Blocking, one transaction.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn rebuild_hot(&self) -> Result<(), StoreError> {
        self.write(dedup::rebuild)
    }

    /// Gives back at most `policy.pages` free pages when the free pages are
    /// over both thresholds; returns the pages given back (None: under the
    /// thresholds, nothing done). Blocking.
    pub(crate) fn reclaim(&self, policy: &Reclaim) -> Result<Option<i64>, StoreError> {
        self.write(|conn| {
            let pages: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
            let free: i64 = conn.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
            let size: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
            let (num, den) = policy.fraction;
            let over_fraction = (free as u64) * den > (pages as u64) * num;
            let over_bytes = (free as u64) * (size as u64) > policy.min_bytes;
            if !over_fraction || !over_bytes {
                return Ok(None);
            }
            conn.execute_batch(&format!("PRAGMA incremental_vacuum({});", policy.pages))?;
            let after: i64 = conn.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
            Ok(Some(free - after))
        })
    }

    /// The state of a month, for tests: None hot, 1 archiving, 2 archived.
    #[cfg(test)]
    pub(crate) fn month_state(&self, m: Month) -> Option<i64> {
        self.read(|c| Ok(states(c)?.get(&m).copied())).unwrap()
    }

    /// The history of some locations (docs/design/usage.md「读冷数据」):
    /// per local month, whole months, the hot facts plus `cold_month`; with
    /// `days`, per local day of the range the hot facts plus the cold
    /// `fact` segments, decoded one at a time. The hot sums, the summaries
    /// and the list of segments are read in one transaction, so an archive
    /// step in between counts nothing twice. Blocking.
    pub(crate) fn history(
        &self,
        locations: &[i64],
        from: LocalDate,
        to: LocalDate,
        days: bool,
        zone: &dyn LocalZone,
    ) -> Result<HistoryTotals, StoreError> {
        let first = window::day_number(from);
        let last = window::day_number(to).max(first);
        let range = window::start(from, zone)..window::start(window::date_of(last + 1), zone);
        // The month rows cover whole local months, as `cold_month` does.
        let to_date = window::date_of(last);
        let next_month = if to_date.month == 12 {
            LocalDate {
                year: to_date.year + 1,
                month: 1,
                day: 1,
            }
        } else {
            LocalDate {
                year: to_date.year,
                month: to_date.month + 1,
                day: 1,
            }
        };
        let months_range = window::start(
            LocalDate {
                year: from.year,
                month: from.month,
                day: 1,
            },
            zone,
        )..window::start(next_month, zone);
        let wanted: HashSet<i64> = locations.iter().copied().collect();
        let mut out = HistoryTotals {
            months: BTreeMap::new(),
            days: days.then(|| vec![[0u64; 4]; (last - first + 1) as usize]),
        };
        for m in ym(from)..=ym(window::date_of(last)) {
            out.months.insert(m, [0; 4]);
        }
        let day_of = |at: i64| -> Option<usize> {
            let d = window::day_number(window::local_date(at, zone));
            (first..=last).contains(&d).then(|| (d - first) as usize)
        };
        let mut segments: Vec<i64> = Vec::new();
        let sums = |conn: &Connection| -> rusqlite::Result<()> {
            let mut hot = conn.prepare_cached(
                "SELECT bucket, sum(input), sum(output), sum(cache_read), sum(cache_write)
                 FROM fact WHERE location = ?1 AND at >= ?2 AND at < ?3 GROUP BY bucket",
            )?;
            for &l in locations {
                let rows = hot
                    .query_map(params![l, months_range.start, months_range.end], |r| {
                        Ok((r.get::<_, i64>(0)?, micro(r, 1)?))
                    })?;
                for row in rows {
                    let (bucket, m) = row?;
                    let at = bucket * BUCKET_MS;
                    let month = ym(window::local_date(at, zone));
                    if let Some(slot) = out.months.get_mut(&month) {
                        plus(slot, &m);
                    }
                    if let (Some(days), Some(d)) = (out.days.as_mut(), day_of(at)) {
                        plus(&mut days[d], &m);
                    }
                }
            }
            let mut cold = conn.prepare_cached(
                "SELECT ym, sum(input), sum(output), sum(cache_read), sum(cache_write)
                 FROM cold_month WHERE location = ?1 AND ym >= ?2 AND ym <= ?3 GROUP BY ym",
            )?;
            for &l in locations {
                let rows = cold
                    .query_map(params![l, ym(from), ym(window::date_of(last))], |r| {
                        Ok((r.get::<_, i64>(0)?, micro(r, 1)?))
                    })?;
                for row in rows {
                    let (month, m) = row?;
                    if let Some(slot) = out.months.get_mut(&month) {
                        plus(slot, &m);
                    }
                }
            }
            if days {
                segments = conn
                    .prepare_cached(
                        "SELECT id FROM cold_part WHERE tbl = 'fact' AND month >= ?1 AND month <= ?2
                         ORDER BY month, seq",
                    )?
                    .query_map(
                        params![month_of(range.start), month_of(range.end - 1)],
                        |r| r.get(0),
                    )?
                    .collect::<rusqlite::Result<_>>()?;
            }
            Ok(())
        };
        self.read(|conn| super::read_transaction(conn, sums))?;
        // One segment at a time, nothing of it kept. A listed segment is
        // never rewritten; one a full rebuild removed meanwhile is skipped.
        for id in segments {
            let Some(seg) = self.cold_segment(id)? else {
                continue;
            };
            let Some(cols) = FactColumns::of(&seg.columns) else {
                continue;
            };
            let Some(days) = out.days.as_mut() else { break };
            for row in &seg.rows {
                let f = cols.row(row, zone);
                if !wanted.contains(&f.location) || !range.contains(&f.at) {
                    continue;
                }
                if let Some(d) = day_of(f.at) {
                    let m = f.micro.map(|v| v.max(0) as u64);
                    plus(&mut days[d], &m);
                }
            }
        }
        Ok(out)
    }

    /// One decoded segment by its row ID; None when it does not decode.
    fn cold_segment(&self, id: i64) -> Result<Option<Segment>, StoreError> {
        let packed: Option<Vec<u8>> = self.read(|conn| {
            conn.prepare_cached("SELECT data FROM cold_part WHERE id = ?1")?
                .query_row(params![id], |r| r.get(0))
                .optional()
        })?;
        self.cold_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let seg = packed
            .and_then(|p| segment::decompress(&p))
            .and_then(|r| segment::decode(&r));
        if seg.is_none() {
            error!("cold segment does not decode"; segment = id);
        }
        Ok(seg)
    }

    /// Every cold row of a table, decoded (tests compare them with the hot
    /// rows before the archive).
    #[cfg(test)]
    pub(crate) fn cold_rows(&self, table: &str) -> Vec<Vec<Value>> {
        let ids: Vec<i64> = self
            .read(|c| {
                c.prepare("SELECT id FROM cold_part WHERE tbl = ?1 ORDER BY month, seq")?
                    .query_map(params![table], |r| r.get(0))?
                    .collect()
            })
            .unwrap();
        let mut out = Vec::new();
        for id in ids {
            let seg = self.cold_segment(id).unwrap().expect("decodes");
            let n = if table == "fact" {
                seg.columns.len() - 1
            } else {
                seg.columns.len()
            };
            out.extend(seg.rows.into_iter().map(|mut r| {
                r.truncate(n);
                r
            }));
        }
        out
    }
}

/// History sums in millionths: per local month (`Month` numbering) and,
/// when asked, per day of the range.
#[derive(Debug, Default)]
pub(crate) struct HistoryTotals {
    pub(crate) months: BTreeMap<Month, [u64; 4]>,
    pub(crate) days: Option<Vec<[u64; 4]>>,
}

fn plus(into: &mut [u64; 4], m: &[u64; 4]) {
    for (a, b) in into.iter_mut().zip(m) {
        *a = a.saturating_add(*b);
    }
}

fn micro(r: &rusqlite::Row<'_>, from: usize) -> rusqlite::Result<[u64; 4]> {
    let mut out = [0u64; 4];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = r.get::<_, Option<i64>>(from + i)?.unwrap_or(0).max(0) as u64;
    }
    Ok(out)
}

/// Months whose check failed, with the segment format of the failure.
fn failed(conn: &Connection) -> rusqlite::Result<HashMap<Month, i64>> {
    let mut stmt = conn.prepare_cached(
        "SELECT name, value FROM meta WHERE name >= 'archive_failed:' AND name < 'archive_failed;'",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    let mut out = HashMap::new();
    for row in rows {
        let (name, value) = row?;
        if let Some(m) = name
            .strip_prefix("archive_failed:")
            .and_then(|m| m.parse().ok())
        {
            out.insert(m, value);
        }
    }
    Ok(out)
}

/// Reads one segment of `table`'s hot rows in `range`, in its timestamp
/// order, up to `limit` encoded bytes (with the `usage_value` segment of a
/// `usage_source` one, first). A `fact` row carries the local month it is
/// summed into.
fn read_segment(
    tx: &Connection,
    table: &Table,
    range: &Range<i64>,
    limit: usize,
    version: i64,
    zone: &dyn LocalZone,
) -> rusqlite::Result<Vec<Read>> {
    // Sources come with their counted-source row (`usage_value` can only be
    // looked up by key; an index on its source would be as large as the
    // table): one join, not a lookup per row.
    let values = table.name == "usage_source";
    let sql = if values {
        format!(
            "SELECT s.*, v.source AS counted FROM usage_source s
             LEFT JOIN usage_value v ON v.key = s.key AND v.source = s.id
             WHERE {} ORDER BY {}",
            table.month, table.order
        )
    } else {
        format!(
            "SELECT * FROM {} WHERE {} ORDER BY {}",
            table.name, table.month, table.order
        )
    };
    let mut stmt = tx.prepare_cached(&sql)?;
    let mut columns: Vec<String> = stmt
        .column_names()
        .iter()
        .map(|c| (*c).to_owned())
        .collect();
    if values {
        columns.pop();
    }
    let n = columns.len();
    let id = columns.iter().position(|c| c == table.key).unwrap_or(0);
    let at = columns.iter().position(|c| c == table.at).unwrap_or(0);
    let key = columns.iter().position(|c| c == "key");
    let is_fact = table.name == "fact";
    if is_fact {
        columns.push(COLD_YM.to_owned());
    }
    let value_columns = vec!["key".to_owned(), "source".to_owned()];
    let mut encoder = Encoder::new(table.name, version, columns.clone());
    let mut value_encoder = Encoder::new("usage_value", version, value_columns.clone());
    let mut rows: Vec<Vec<Value>> = Vec::new();
    let mut value_rows: Vec<Vec<Value>> = Vec::new();
    let mut last = (0i64, 0i64);
    let int = |v: &Value| match v {
        Value::Integer(i) => *i,
        _ => 0,
    };
    let mut q = stmt.query(params![range.start, range.end])?;
    while let Some(r) = q.next()? {
        let mut row: Vec<Value> = (0..n).map(|i| r.get(i)).collect::<rusqlite::Result<_>>()?;
        last = (int(&row[at]), int(&row[id]));
        if is_fact {
            row.push(Value::Integer(ym(window::local_date(last.0, zone))));
        }
        if values && let (Some(k), Value::Integer(source)) = (key, r.get::<_, Value>(n)?) {
            let value = vec![row[k].clone(), Value::Integer(source)];
            value_encoder.push(&value);
            value_rows.push(value);
        }
        encoder.push(&row);
        rows.push(row);
        if encoder.bytes() >= limit {
            break;
        }
    }
    drop(q);
    let mut out = vec![Read::finish(
        table.name,
        version,
        columns,
        encoder,
        rows,
        Delete::Range(last),
    )];
    if !value_rows.is_empty() {
        let read = Read::finish(
            "usage_value",
            version,
            value_columns,
            value_encoder,
            value_rows,
            Delete::Values,
        );
        out.insert(0, read);
    }
    Ok(out)
}

/// How a segment's hot rows are deleted.
#[derive(Clone, Copy, Debug)]
enum Delete {
    /// The month's rows up to this (timestamp, ID), in the table's order:
    /// the earlier ones went with the earlier segments.
    Range((i64, i64)),
    /// The segment's own rows, by key: the counted-source rows of the
    /// sources read with it.
    Values,
}

/// One segment read from the hot rows: the rows, how to delete them, the
/// encoded and the compressed bytes.
struct Read {
    table: &'static str,
    segment: Segment,
    delete: Delete,
    raw: Vec<u8>,
    packed: Vec<u8>,
}

impl Read {
    fn finish(
        table: &'static str,
        version: i64,
        columns: Vec<String>,
        encoder: Encoder,
        rows: Vec<Vec<Value>>,
        delete: Delete,
    ) -> Read {
        let raw = encoder.finish();
        let packed = segment::compress(&raw);
        Read {
            table,
            segment: Segment {
                table: table.to_owned(),
                schema: version,
                columns,
                rows,
            },
            delete,
            raw,
            packed,
        }
    }

    /// Deletes the segment's hot rows; the count must be its rows.
    fn delete(&self, tx: &Connection, table: &Table, range: &Range<i64>) -> rusqlite::Result<()> {
        let n = match self.delete {
            Delete::Range((at, id)) => {
                let sql = format!(
                    "DELETE FROM {} WHERE {} AND ({}, {}) <= (?3, ?4)",
                    table.name, table.month, table.at, table.key
                );
                tx.prepare_cached(&sql)?
                    .execute(params![range.start, range.end, at, id])?
            }
            Delete::Values => {
                // In key order: neighbouring keys share pages, so the
                // small page cache holds the ones being changed.
                let mut keys: Vec<(&[u8], i64)> = self
                    .segment
                    .rows
                    .iter()
                    .filter_map(|row| match (&row[0], &row[1]) {
                        (Value::Blob(k), Value::Integer(s)) => Some((&k[..], *s)),
                        _ => None,
                    })
                    .collect();
                keys.sort_unstable();
                let mut delete =
                    tx.prepare_cached("DELETE FROM usage_value WHERE key = ?1 AND source = ?2")?;
                let mut n = 0;
                for (key, source) in keys {
                    n += delete.execute(params![key, source])?;
                }
                n
            }
        };
        if n != self.segment.rows.len() {
            // Never happens in one immediate transaction; refuse rather
            // than lose a row.
            return Err(rusqlite::Error::StatementChangedRows(n));
        }
        Ok(())
    }

    /// Decoded again and compared with the rows read.
    fn checks(&self) -> bool {
        let unpacked = segment::decompress(&self.packed);
        let same_bytes = unpacked.as_deref() == Some(&self.raw[..]);
        same_bytes && unpacked.and_then(|r| segment::decode(&r)).as_ref() == Some(&self.segment)
    }
}

/// The summaries of a segment just archived: `fact` adds to `cold_month`
/// and `cold_total`; `usage_source` records which sessions have cold
/// sources in the month. `cold_anchor` holds changed turns and their
/// shares, which come with step 4: nothing to write yet.
fn summarize(
    tx: &Connection,
    table: &str,
    seg: &Segment,
    m: Month,
    zone: &dyn LocalZone,
) -> rusqlite::Result<()> {
    match table {
        "fact" => {
            let Some(cols) = FactColumns::of(&seg.columns) else {
                return Ok(());
            };
            let mut s = Summaries::default();
            for row in &seg.rows {
                s.fact(&cols.row(row, zone), 1);
            }
            s.write(tx)
        }
        "usage_source" => {
            let Some(i) = seg.column("session") else {
                return Ok(());
            };
            let mut insert = tx.prepare_cached(
                "INSERT OR IGNORE INTO cold_seen (session, month) VALUES (?1, ?2)",
            )?;
            let mut done = HashSet::new();
            for row in &seg.rows {
                if let Value::Integer(session) = row[i]
                    && done.insert(session)
                {
                    insert.execute(params![session, m])?;
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Puts a month's observation rows back into the hot tables and takes its
/// facts back out of the summaries; the month is hot again.
#[cfg(any(test, feature = "test-support"))]
fn restore_month(tx: &Connection, m: Month) -> rusqlite::Result<()> {
    let parts: Vec<(String, Vec<u8>)> = tx
        .prepare("SELECT tbl, data FROM cold_part WHERE month = ?1 ORDER BY id")?
        .query_map(params![m], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut s = Summaries::default();
    for (table, packed) in parts {
        let Some(seg) = segment::decompress(&packed).and_then(|r| segment::decode(&r)) else {
            error!("cold segment does not decode; its rows stay lost"; month = m);
            continue;
        };
        match table.as_str() {
            "fact" => {
                // Each row carries the local month it was summed into; no
                // zone is asked.
                if let Some(cols) = FactColumns::of(&seg.columns) {
                    for row in &seg.rows {
                        s.fact(&cols.row(row, &Utc), -1);
                    }
                }
            }
            // Derived: rebuilt from the restored sources.
            "usage_value" => {}
            name => {
                let marks = vec!["?"; seg.columns.len()].join(", ");
                let sql = format!(
                    "INSERT OR IGNORE INTO {name} ({}) VALUES ({marks})",
                    seg.columns.join(", ")
                );
                let mut insert = tx.prepare(&sql)?;
                for row in &seg.rows {
                    insert.execute(rusqlite::params_from_iter(row.iter()))?;
                }
            }
        }
    }
    s.write(tx)?;
    tx.execute("DELETE FROM cold_part WHERE month = ?1", params![m])?;
    tx.execute("DELETE FROM cold_seen WHERE month = ?1", params![m])?;
    set_state(tx, m, None)?;
    tx.execute(
        "DELETE FROM meta WHERE name IN (?1, ?2)",
        params![format!("archive_failed:{m}"), format!("archive_mark:{m}")],
    )?;
    Ok(())
}
