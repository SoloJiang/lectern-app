//! Storage of extractions (docs/design/code_intel.md「存储表」): one
//! extraction is one unit of `cache`'s custom tables in the namespace
//! `code_intel.tags`, keyed by (blob OID, language, grammar version, query
//! version), with its rows in `ci_ext`, `ci_def` and `ci_ref`. Every read is
//! limited to the current (language, grammar version, query version) triples
//! (the version filter), so units of an old grammar or query are never
//! returned and only wait for eviction.
//!
//! All calls but [`Store::new`], [`Store::evictions`] and [`Store::write`]
//! block on SQLite: executor tasks only. SQL runs through `lectern-cache`'s
//! custom-table interface; no rusqlite type appears here.

use lectern_cache::{
    Compression, Key, Namespace, Param, Partition, Row, TableRead, TableSet, TableWrite, Tables,
    Unit, Written,
};
use lectern_services::project::BlobId;

use crate::extract::{Def, ExtractStatus, Extraction, Lang, Visibility};
use crate::{DEF_KINDS, DefKind, LangName};

/// The units' namespace. The version changes with the tables or the units'
/// key; a change of extraction rules changes `EXTRACTOR_VERSION` instead.
/// 2: `ci_def_name` covers the unit (the paged and tiered lookups by name).
pub const NAMESPACE: Namespace = Namespace {
    name: "code_intel.tags",
    version: 2,
    compression: Compression::None,
};

/// Most bytes of `code_intel.tags` units in one partition.
pub const QUOTA: u64 = 256 << 20;

/// OIDs per existence-check query: the fixed `IN (…)` of [`EXISTS`].
pub const EXISTS_BATCH: usize = 500;

/// Version triples per by-name query: a compatibility group of up to this
/// many languages is one query, a larger one several.
pub const GROUP_TRIPLES: usize = 4;

/// Definitions per page of a lookup by name ([`Store::next_defs`]): the
/// rows one statement reads, a few milliseconds whatever the name.
pub const NAME_PAGE: usize = 256;

/// Definitions per statement of a lookup by name in given units
/// ([`Store::defs_by_name_in`]).
pub const NAME_IN_LIMIT: usize = 256;

pub(crate) const SCHEMA: &[&str] = &[
    "CREATE TABLE ci_ext (
       unit    INTEGER PRIMARY KEY REFERENCES entry (id) ON DELETE CASCADE,
       oid     BLOB    NOT NULL,
       lang    TEXT    NOT NULL,
       grammar INTEGER NOT NULL,
       query   INTEGER NOT NULL,
       status  INTEGER NOT NULL,
       UNIQUE (oid, lang, grammar, query))",
    "CREATE TABLE ci_def (
       unit     INTEGER NOT NULL REFERENCES entry (id) ON DELETE CASCADE,
       name     TEXT    NOT NULL,
       kind     INTEGER NOT NULL,
       line     INTEGER NOT NULL,
       col      INTEGER NOT NULL,
       end_line INTEGER NOT NULL,
       scope    TEXT,
       flags    INTEGER NOT NULL,
       arity    INTEGER)",
    // With the unit: a name's rows in unit order without reading the table
    // (the pages of a lookup, the count of the rows left), and one seek per
    // unit for the lookup in given units.
    "CREATE INDEX ci_def_name ON ci_def (name COLLATE NOCASE, unit)",
    "CREATE INDEX ci_def_unit ON ci_def (unit)",
    "CREATE TABLE ci_ref (
       name TEXT    NOT NULL,
       unit INTEGER NOT NULL REFERENCES entry (id) ON DELETE CASCADE,
       PRIMARY KEY (name, unit)) WITHOUT ROWID",
    "CREATE INDEX ci_ref_unit ON ci_ref (unit)",
];

pub(crate) const INSERT_EXT: &str = "INSERT INTO ci_ext (unit, oid, lang, grammar, query, status)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6)";
pub(crate) const INSERT_DEF: &str =
    "INSERT INTO ci_def (unit, name, kind, line, col, end_line, scope, flags, arity)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)";
/// References are deduplicated by the extraction; `OR IGNORE` keeps a
/// duplicate from rolling the whole batch back.
pub(crate) const INSERT_REF: &str = "INSERT OR IGNORE INTO ci_ref (name, unit) VALUES (?1, ?2)";

const EXISTS_HEAD: &str = "SELECT unit, oid, status FROM ci_ext
     WHERE lang = ?1 AND grammar = ?2 AND query = ?3 AND oid IN (";
/// `?` takes the number after the largest so far: `?4` … `?503`.
const EXISTS_LEN: usize = EXISTS_HEAD.len() + 2 * EXISTS_BATCH;
const EXISTS_BYTES: [u8; EXISTS_LEN] = {
    let mut out = [0u8; EXISTS_LEN];
    let head = EXISTS_HEAD.as_bytes();
    let mut i = 0;
    while i < head.len() {
        out[i] = head[i];
        i += 1;
    }
    let mut j = 0;
    while j < EXISTS_BATCH {
        out[i] = b'?';
        out[i + 1] = if j + 1 < EXISTS_BATCH { b',' } else { b')' };
        i += 2;
        j += 1;
    }
    out
};
/// The existence check: units of one version triple among up to
/// [`EXISTS_BATCH`] OIDs. Static text with all 500 placeholders; unused
/// ones are bound to NULL, which matches nothing.
pub(crate) const EXISTS: &str = match std::str::from_utf8(&EXISTS_BYTES) {
    Ok(sql) => sql,
    Err(_) => panic!("the existence check is ASCII"),
};

/// One page of the definitions by name (jump step 3, `Named`): those of
/// units after `?14`, in unit order, at most `?15` rows, in the units of up
/// to [`GROUP_TRIPLES`] version triples. `NOCASE` and the unit range use
/// `ci_def_name` (name, unit) in index order: a page reads its rows and no
/// more, whatever the name's count. Names are compared exactly afterwards.
pub(crate) const DEFS_PAGE: &str =
    "SELECT d.name, d.kind, d.line, d.col, d.end_line, d.scope, d.flags, d.arity,
            e.oid, e.lang, d.unit
     FROM ci_def d INDEXED BY ci_def_name CROSS JOIN ci_ext e ON e.unit = d.unit
     WHERE d.name = ?1 COLLATE NOCASE AND d.unit > ?14 AND e.status = 0
       AND ((e.lang = ?2 AND e.grammar = ?3 AND e.query = ?4)
         OR (e.lang = ?5 AND e.grammar = ?6 AND e.query = ?7)
         OR (e.lang = ?8 AND e.grammar = ?9 AND e.query = ?10)
         OR (e.lang = ?11 AND e.grammar = ?12 AND e.query = ?13))
     ORDER BY d.unit LIMIT ?15";

/// The rows named `?1` of units after `?2`, whatever their version and
/// status: an upper bound of what a lookup cut short did not read. Read
/// from `ci_def_name` alone.
pub(crate) const DEFS_LEFT: &str = "SELECT count(*) FROM ci_def INDEXED BY ci_def_name
     WHERE name = ?1 COLLATE NOCASE AND unit > ?2";

// `?14 > 0` (the limit, always positive) names `?14` before the OIDs, so
// their bare `?` number from 15.
const DEFS_IN_HEAD: &str =
    "SELECT d.name, d.kind, d.line, d.col, d.end_line, d.scope, d.flags, d.arity,
            e.oid, e.lang, d.unit
     FROM ci_ext e CROSS JOIN ci_def d INDEXED BY ci_def_name ON d.unit = e.unit
     WHERE d.name = ?1 COLLATE NOCASE AND e.status = 0
       AND ((e.lang = ?2 AND e.grammar = ?3 AND e.query = ?4)
         OR (e.lang = ?5 AND e.grammar = ?6 AND e.query = ?7)
         OR (e.lang = ?8 AND e.grammar = ?9 AND e.query = ?10)
         OR (e.lang = ?11 AND e.grammar = ?12 AND e.query = ?13))
       AND ?14 > 0 AND e.oid IN (";
const DEFS_IN_TAIL: &str = " LIMIT ?14";
/// `?` takes the number after the largest so far: `?15` … `?514`.
const DEFS_IN_LEN: usize = DEFS_IN_HEAD.len() + 2 * EXISTS_BATCH + DEFS_IN_TAIL.len();
const DEFS_IN_BYTES: [u8; DEFS_IN_LEN] = {
    let mut out = [0u8; DEFS_IN_LEN];
    let head = DEFS_IN_HEAD.as_bytes();
    let mut i = 0;
    while i < head.len() {
        out[i] = head[i];
        i += 1;
    }
    let mut j = 0;
    while j < EXISTS_BATCH {
        out[i] = b'?';
        out[i + 1] = if j + 1 < EXISTS_BATCH { b',' } else { b')' };
        i += 2;
        j += 1;
    }
    let tail = DEFS_IN_TAIL.as_bytes();
    let mut k = 0;
    while k < tail.len() {
        out[i] = tail[k];
        i += 1;
        k += 1;
    }
    out
};
/// The definitions by name in the units of up to [`EXISTS_BATCH`] OIDs
/// (`?15` …, unused ones NULL), at most `?14` rows: the jump's and
/// `Named`'s first batch, from the files nearest the query (「候选的分层与
/// 过滤」). Each OID is one seek in `ci_ext`'s key and one in `ci_def_name`
/// (name, unit): the cost follows the files asked for, not how many units
/// define the name.
pub(crate) const DEFS_IN: &str = match std::str::from_utf8(&DEFS_IN_BYTES) {
    Ok(sql) => sql,
    Err(_) => panic!("the lookup in units is ASCII"),
};
/// One page of the units referring to a name (references step 1), exact
/// names: those after unit `?14`, in unit order, at most `?15`. The key
/// (name, unit) of `ci_ref` gives the order: a page reads its rows only.
pub(crate) const REFS_PAGE: &str = "SELECT e.oid, e.lang, e.unit
     FROM ci_ref r CROSS JOIN ci_ext e ON e.unit = r.unit
     WHERE r.name = ?1 AND r.unit > ?14 AND e.status = 0
       AND ((e.lang = ?2 AND e.grammar = ?3 AND e.query = ?4)
         OR (e.lang = ?5 AND e.grammar = ?6 AND e.query = ?7)
         OR (e.lang = ?8 AND e.grammar = ?9 AND e.query = ?10)
         OR (e.lang = ?11 AND e.grammar = ?12 AND e.query = ?13))
     ORDER BY r.unit LIMIT ?15";
/// Every `Done` unit of one version triple (the name table's first pass).
pub(crate) const UNITS: &str = "SELECT unit, oid FROM ci_ext
     WHERE lang = ?1 AND grammar = ?2 AND query = ?3 AND status = 0";
/// Every definition's unit, name and kind, in table order (the name
/// table's second pass): one scan of `ci_def`, no index lookups.
pub(crate) const DEF_NAMES: &str = "SELECT unit, name, kind FROM ci_def";
pub(crate) const EXT_OF_UNIT: &str = "SELECT status FROM ci_ext WHERE unit = ?1";
/// In insertion order, which is the extraction's order.
pub(crate) const DEFS_OF_UNIT: &str =
    "SELECT name, kind, line, col, end_line, scope, flags, arity FROM ci_def
     WHERE unit = ?1 ORDER BY rowid";
pub(crate) const REFS_OF_UNIT: &str = "SELECT name FROM ci_ref WHERE unit = ?1 ORDER BY name";

pub(crate) const STATEMENTS: &[&str] = &[
    INSERT_EXT,
    INSERT_DEF,
    INSERT_REF,
    EXISTS,
    DEFS_PAGE,
    DEFS_LEFT,
    DEFS_IN,
    REFS_PAGE,
    UNITS,
    DEF_NAMES,
    EXT_OF_UNIT,
    DEFS_OF_UNIT,
    REFS_OF_UNIT,
];

/// The registration of `code_intel.tags`.
pub static TABLES: Tables = Tables {
    namespace: NAMESPACE,
    schema: SCHEMA,
    statements: STATEMENTS,
    quota: QUOTA,
};

/// The current version triple of a language: the part of the key besides
/// the OID, and the version filter of every read.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Version {
    pub lang: LangName,
    pub grammar: u64,
    pub query: u64,
}

impl Version {
    pub fn new(lang: LangName, grammar: u64, query: u64) -> Version {
        Version {
            lang,
            grammar,
            query,
        }
    }

    /// The triple of a language in the current language table.
    pub fn of(lang: &Lang) -> Version {
        Version::new(
            lang.name().clone(),
            lang.grammar_version(),
            lang.query_version(),
        )
    }
}

/// The unit's key: `Key::new().bytes(oid).str(lang).u64(grammar).u64(query)`.
pub fn unit_key(oid: BlobId, version: &Version) -> Key {
    Key::new()
        .bytes(oid.as_bytes())
        .str(&version.lang.0)
        .u64(version.grammar)
        .u64(version.query)
}

/// The bytes a unit declares, near its rows plus two index entries each:
/// `64 + Σ definitions (2 × name + scope + 40) + Σ references (2 × name + 24)`.
/// Only a `Done` extraction has rows.
pub fn unit_bytes(extraction: &Extraction) -> u64 {
    if extraction.status != ExtractStatus::Done {
        return 64;
    }
    let defs: u64 = extraction
        .defs
        .iter()
        .map(|d| 2 * d.name.len() as u64 + d.scope.as_deref().map_or(0, str::len) as u64 + 40)
        .sum();
    let refs: u64 = extraction
        .refs
        .iter()
        .map(|r| 2 * r.len() as u64 + 24)
        .sum();
    64 + defs + refs
}

/// Units per [`Store::touch_pending`] call: one write-back's worth for
/// `lectern-cache`.
pub const TOUCH_CHUNK: usize = 1024;

/// Hits of existence checks waiting to be marked as used, off the warm
/// path. Kept with the project's batch state and dropped with it: when the
/// project closes, or the worktree changes, before the queue is drained,
/// those units keep their old access times. That changes only which units
/// eviction takes first, never a result.
#[derive(Default, Debug)]
pub struct PendingTouches {
    units: Vec<Unit>,
    /// The eviction generation the queued IDs were read under.
    generation: u64,
}

impl PendingTouches {
    pub fn new() -> PendingTouches {
        PendingTouches::default()
    }

    /// Queues the hits of an existence check. `generation` is
    /// [`Store::evictions`] read before that check; a queue holding IDs of
    /// another generation is emptied first.
    pub fn push(&mut self, generation: u64, found: &[Found]) {
        if generation != self.generation {
            self.units.clear();
            self.generation = generation;
        }
        self.units.extend(found.iter().map(|f| f.unit));
    }

    pub fn len(&self) -> usize {
        self.units.len()
    }

    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
    }
}

/// A unit found by [`Store::exists`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Found {
    pub oid: BlobId,
    pub unit: Unit,
    /// Not only `Done`: a timed-out or too-large blob is stored too, so it is
    /// not retried on every open.
    pub status: ExtractStatus,
}

/// A `Done` unit of a current version: by-name references, the name table.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StoredUnit {
    pub unit: Unit,
    pub oid: BlobId,
    pub lang: LangName,
}

/// A definition found by name, with its unit's OID and language.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StoredDef {
    pub unit: Unit,
    pub oid: BlobId,
    pub lang: LangName,
    pub def: Def,
}

/// Where a lookup of a name's definitions in pages stands
/// ([`Store::next_defs`]): the version groups of its versions
/// ([`GROUP_TRIPLES`] each), the current one, and the last unit read in
/// it. Holds no connection; a query keeps it between its batches.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct NamePager {
    name: Box<str>,
    versions: Vec<Version>,
    group: usize,
    after: i64,
    finished: bool,
}

impl NamePager {
    pub fn new(name: &str, versions: &[Version]) -> NamePager {
        NamePager {
            name: name.into(),
            versions: versions.to_vec(),
            group: 0,
            after: 0,
            finished: versions.is_empty(),
        }
    }

    /// Every page is read.
    pub fn done(&self) -> bool {
        self.finished
    }

    fn groups(&self) -> usize {
        self.versions.len().div_ceil(GROUP_TRIPLES)
    }

    fn next_group(&mut self) {
        self.group += 1;
        self.after = 0;
        if self.group >= self.groups() {
            self.finished = true;
        }
    }

    fn finish(&mut self) {
        self.finished = true;
    }
}

/// `code_intel.tags` in one partition (`cache.repo(common_dir)`, or
/// `cache.folder(root)` for a folder project). `Clone + Send + Sync`; holds
/// no connection.
#[derive(Clone)]
pub struct Store {
    set: TableSet,
}

impl Store {
    /// No IO; any thread. The tables are created by the first read or write.
    pub fn new(partition: &Partition) -> Store {
        Store::with_tables(partition, &TABLES)
    }

    /// The same store under another registration of the namespace (tests: a
    /// smaller quota, extra statements).
    pub(crate) fn with_tables(partition: &Partition, tables: &'static Tables) -> Store {
        Store {
            set: partition.tables(tables),
        }
    }

    /// The eviction generation of `code_intel.tags` in this partition
    /// (「输入代数与派生状态」): changes when units are evicted or deleted,
    /// or the partition is rebuilt. One atomic load; any thread, never
    /// blocks. Unit IDs read before a change are not reused after it.
    pub fn evictions(&self) -> u64 {
        self.set.evictions()
    }

    /// Starts a write. No IO.
    pub fn write(&self) -> StoreWrite {
        StoreWrite {
            write: self.set.write(),
            units: 0,
        }
    }

    /// Marks units as used (access time, 1-hour precision), without reading
    /// them. Called after the results a user waits for are delivered.
    pub fn touch(&self, units: &[Unit]) {
        if !units.is_empty() {
            self.set.touch(units);
        }
    }

    /// The existence check: the units among `oids` stored under exactly
    /// `version`, whatever their status, in [`EXISTS_BATCH`]-OID queries.
    /// Read only: it marks nothing as used, so a warm start is not held up
    /// by access-time writes. The caller queues the hits in
    /// [`PendingTouches`] and marks them later with [`Store::touch_pending`].
    /// `None` when the partition cannot be read: treat every OID as missing.
    pub fn exists(&self, version: &Version, oids: &[BlobId]) -> Option<Vec<Found>> {
        let lang = &*version.lang.0;
        read_counted(&self.set, |r| {
            let mut found = Vec::new();
            let mut params = [Param::Null; 3 + EXISTS_BATCH];
            params[0] = Param::Text(lang);
            params[1] = Param::Int(version.grammar as i64);
            params[2] = Param::Int(version.query as i64);
            for chunk in oids.chunks(EXISTS_BATCH) {
                for (slot, oid) in params[3..]
                    .iter_mut()
                    .zip(chunk.iter().map(Some).chain(std::iter::repeat(None)))
                {
                    *slot = oid.map_or(Param::Null, |o| Param::Bytes(o.as_bytes()));
                }
                let ok = counted(r).query(EXISTS, &params, |row| {
                    let hit = row.unit(0).zip(oid_at(row, 1)).zip(status_at(row, 2));
                    if let Some(((unit, oid), status)) = hit {
                        found.push(Found { oid, unit, status });
                    }
                    true
                });
                if !ok {
                    break;
                }
            }
            found
        })
    }

    /// Marks up to [`TOUCH_CHUNK`] queued units as used and returns how many
    /// are still queued. The batch thread calls it at batch boundaries after
    /// the warm pass, with the usual pause and cancel checks between calls.
    /// Units queued under an older eviction generation are dropped unmarked:
    /// their IDs may now belong to other units.
    pub fn touch_pending(&self, pending: &mut PendingTouches) -> usize {
        if pending.generation != self.evictions() {
            pending.units.clear();
            return 0;
        }
        let at = pending.units.len().saturating_sub(TOUCH_CHUNK);
        let chunk = pending.units.split_off(at);
        self.touch(&chunk);
        if pending.units.is_empty() {
            // A warm start queues every unit: its buffer does not stay.
            pending.units = Vec::new();
        }
        pending.units.len()
    }

    /// The definitions named exactly `name` (case-sensitive) in `Done` units
    /// of `versions` (the current triples of a compatibility group), every
    /// page of them ([`next_defs`](Self::next_defs)). Not marked as used:
    /// the caller touches what it delivers.
    pub fn defs_by_name(&self, name: &str, versions: &[Version]) -> Option<Vec<StoredDef>> {
        let mut pager = NamePager::new(name, versions);
        let mut out = Vec::new();
        while !pager.done() {
            out.extend(self.next_defs(&mut pager, NAME_PAGE)?);
        }
        Some(out)
    }

    /// The next page of a lookup by name ([`NamePager`]): up to `limit`
    /// definitions named exactly `name` in `Done` units of the pager's
    /// versions, in unit order, after the last unit the pager read. One
    /// statement of at most `limit` rows. `None` when the partition cannot
    /// be read (the pager is then done: nothing more can be learned).
    pub fn next_defs(&self, pager: &mut NamePager, limit: usize) -> Option<Vec<StoredDef>> {
        if pager.done() {
            return Some(Vec::new());
        }
        let limit = limit.max(1);
        let group = pager.versions[pager.group * GROUP_TRIPLES..]
            .iter()
            .take(GROUP_TRIPLES)
            .cloned()
            .collect::<Vec<Version>>();
        let name = &*pager.name;
        let after = pager.after;
        // Every row's unit (to go on after it), with its definition when
        // the name matches exactly (`NOCASE` reads the other cases too).
        let read = read_counted(&self.set, |r| {
            let mut rows: Vec<(i64, Option<StoredDef>)> = Vec::new();
            let mut params = [Param::Null; 15];
            params[..13].copy_from_slice(&group_params(name, &group));
            params[13] = Param::Int(after);
            params[14] = Param::Int(limit as i64);
            let ok = counted(r).query(DEFS_PAGE, &params, |row| {
                let Some(unit) = row.unit(10) else {
                    return true;
                };
                let def = (row.text(0) == Some(name))
                    .then(|| {
                        def_at(row, 0)
                            .zip(oid_at(row, 8))
                            .zip(lang_at(row, 9, &group))
                    })
                    .flatten()
                    .map(|((def, oid), lang)| StoredDef {
                        unit,
                        oid,
                        lang,
                        def,
                    });
                rows.push((unit.id(), def));
                true
            });
            ok.then_some(rows)
        })
        .flatten();
        let Some(mut rows) = read else {
            pager.finish();
            return None;
        };
        // A full page may end inside a unit (overloads): that unit's rows go
        // to the next page, unless the whole page is one unit (the rest of
        // that unit's rows of the name is then not read).
        if rows.len() >= limit {
            let last = rows.last().map(|(u, _)| *u);
            if rows.first().map(|(u, _)| *u) != last {
                rows.retain(|(u, _)| Some(*u) != last);
            }
            if let Some((u, _)) = rows.last() {
                pager.after = *u;
            }
        } else {
            pager.next_group();
        }
        Some(rows.into_iter().filter_map(|(_, d)| d).collect())
    }

    /// The rows named `name` (any case, version or status) that a lookup
    /// cut short at `pager` has not read: those of units after its last
    /// unit in the current group, and all of them for each group left. An
    /// upper bound of the definitions it left out (index only, no table
    /// row read). 0 when the partition cannot be read.
    pub fn defs_left(&self, pager: &NamePager) -> u64 {
        if pager.done() {
            return 0;
        }
        let count = |after: i64| -> u64 {
            read_counted(&self.set, |r| {
                let mut n = 0u64;
                counted(r).query(
                    DEFS_LEFT,
                    &[Param::Text(&pager.name), Param::Int(after)],
                    |row| {
                        n = row.int(0).map_or(0, |c| c.max(0) as u64);
                        false
                    },
                );
                n
            })
            .unwrap_or(0)
        };
        let groups_left = pager.groups().saturating_sub(pager.group + 1) as u64;
        count(pager.after) + groups_left * count(0)
    }

    /// The definitions named exactly `name` in `Done` units of `versions`
    /// whose OID is among `oids`: one statement per [`EXISTS_BATCH`] OIDs
    /// and version group, each at most `limit` rows (what one statement
    /// returns beyond them is left to the paged lookup). The cost follows
    /// the OIDs, not the name's count.
    pub fn defs_by_name_in(
        &self,
        name: &str,
        versions: &[Version],
        oids: &[BlobId],
        limit: usize,
    ) -> Option<Vec<StoredDef>> {
        read_counted(&self.set, |r| {
            let mut out = Vec::new();
            let mut params = [Param::Null; 14 + EXISTS_BATCH];
            params[13] = Param::Int(limit.max(1) as i64);
            for group in versions.chunks(GROUP_TRIPLES) {
                params[..13].copy_from_slice(&group_params(name, group));
                for chunk in oids.chunks(EXISTS_BATCH) {
                    for (slot, oid) in params[14..]
                        .iter_mut()
                        .zip(chunk.iter().map(Some).chain(std::iter::repeat(None)))
                    {
                        *slot = oid.map_or(Param::Null, |o| Param::Bytes(o.as_bytes()));
                    }
                    let ok = counted(r).query(DEFS_IN, &params, |row| {
                        if row.text(0) != Some(name) {
                            return true;
                        }
                        let found = def_at(row, 0)
                            .zip(oid_at(row, 8))
                            .zip(lang_at(row, 9, group))
                            .zip(row.unit(10));
                        if let Some((((def, oid), lang), unit)) = found {
                            out.push(StoredDef {
                                unit,
                                oid,
                                lang,
                                def,
                            });
                        }
                        true
                    });
                    if !ok {
                        return out;
                    }
                }
            }
            out
        })
    }

    /// The `Done` units of `versions` whose reference names include `name`
    /// exactly, every page of them ([`next_refs`](Self::next_refs)). Not
    /// marked as used.
    pub fn refs_by_name(&self, name: &str, versions: &[Version]) -> Option<Vec<StoredUnit>> {
        let mut pager = NamePager::new(name, versions);
        let mut out = Vec::new();
        while !pager.done() {
            out.extend(self.next_refs(&mut pager, NAME_PAGE)?);
        }
        Some(out)
    }

    /// The next page of the units referring to a name ([`NamePager`]): up
    /// to `limit` `Done` units of the pager's versions whose reference names
    /// include its name exactly, in unit order, after the last unit read.
    /// One statement. `None` when the partition cannot be read (the pager
    /// is then done).
    pub fn next_refs(&self, pager: &mut NamePager, limit: usize) -> Option<Vec<StoredUnit>> {
        if pager.done() {
            return Some(Vec::new());
        }
        let limit = limit.max(1);
        let group: Vec<Version> = pager.versions[pager.group * GROUP_TRIPLES..]
            .iter()
            .take(GROUP_TRIPLES)
            .cloned()
            .collect();
        let (name, after) = (&*pager.name, pager.after);
        let read = read_counted(&self.set, |r| {
            let mut out = Vec::new();
            let mut last = None;
            let mut rows = 0usize;
            let mut params = [Param::Null; 15];
            params[..13].copy_from_slice(&group_params(name, &group));
            params[13] = Param::Int(after);
            params[14] = Param::Int(limit as i64);
            let ok = counted(r).query(REFS_PAGE, &params, |row| {
                rows += 1;
                last = row.unit(2).map(Unit::id).or(last);
                push_unit(&mut out, row, 2, 0, lang_at(row, 1, &group));
                true
            });
            ok.then_some((out, last, rows))
        })
        .flatten();
        let Some((out, last, rows)) = read else {
            pager.finish();
            return None;
        };
        // One row per (name, unit): a full page ends at a unit boundary.
        match last {
            Some(unit) if rows >= limit => pager.after = unit,
            _ => pager.next_group(),
        }
        Some(out)
    }

    /// Every `Done` unit of `versions` (the name table's first pass, before
    /// filtering by the map).
    pub fn units(&self, versions: &[Version]) -> Option<Vec<StoredUnit>> {
        read_counted(&self.set, |r| {
            let mut out = Vec::new();
            for version in versions {
                let params = [
                    Param::Text(&version.lang.0),
                    Param::Int(version.grammar as i64),
                    Param::Int(version.query as i64),
                ];
                let ok = counted(r).query(UNITS, &params, |row| {
                    push_unit(&mut out, row, 0, 1, Some(version.lang.clone()));
                    true
                });
                if !ok {
                    break;
                }
            }
            out
        })
    }

    /// Calls `f` with each definition name and kind of `units` (sorted by
    /// ID), in table order: one scan of `ci_def`, the rows of other units
    /// skipped. A unit's rows are written together, so the decision is
    /// cached for consecutive rows. False when the partition cannot be read;
    /// `f` then saw part of the names.
    pub fn def_names(&self, units: &[Unit], mut f: impl FnMut(Unit, &str, DefKind)) -> bool {
        debug_assert!(units.is_sorted(), "def_names takes sorted units");
        self.set
            .read(|r| {
                let mut last: Option<(i64, bool)> = None;
                counted(r).query(DEF_NAMES, &[], |row| {
                    let Some(id) = row.int(0) else {
                        return true;
                    };
                    let wanted = match last {
                        Some((at, wanted)) if at == id => wanted,
                        _ => {
                            let wanted = units.binary_search_by_key(&id, |u| u.id()).is_ok();
                            last = Some((id, wanted));
                            wanted
                        }
                    };
                    if wanted && let Some((name, kind)) = row.text(1).zip(kind_at(row, 2)) {
                        f(Unit::from_id(id), name, kind);
                    }
                    true
                });
            })
            .is_some()
    }

    /// One unit's extraction as stored: definitions in the extraction's
    /// order, reference names in byte order. `None` when the unit is gone or
    /// the partition cannot be read.
    pub fn load(&self, unit: Unit) -> Option<Extraction> {
        read_counted(&self.set, |r| load(r, unit)).flatten()
    }
}

fn load(r: &mut TableRead<'_>, unit: Unit) -> Option<Extraction> {
    let id = [Param::Int(unit.id())];
    let mut status = None;
    counted(r).query(EXT_OF_UNIT, &id, |row| {
        status = status_at(row, 0);
        false
    });
    let status = status?;
    let mut defs = Vec::new();
    counted(r).query(DEFS_OF_UNIT, &id, |row| {
        defs.extend(def_at(row, 0));
        true
    });
    let mut refs = Vec::new();
    counted(r).query(REFS_OF_UNIT, &id, |row| {
        refs.extend(row.text(0).map(Box::from));
        true
    });
    Some(Extraction { status, defs, refs })
}

/// `?1` the name, then one (language, grammar, query) triple per version;
/// the triples past `group` are NULL and match nothing.
fn group_params<'a>(name: &'a str, group: &'a [Version]) -> [Param<'a>; 1 + 3 * GROUP_TRIPLES] {
    let mut params = [Param::Null; 1 + 3 * GROUP_TRIPLES];
    params[0] = Param::Text(name);
    for (i, version) in group.iter().enumerate() {
        params[1 + 3 * i] = Param::Text(&version.lang.0);
        params[2 + 3 * i] = Param::Int(version.grammar as i64);
        params[3 + 3 * i] = Param::Int(version.query as i64);
    }
    params
}

fn push_unit(
    out: &mut Vec<StoredUnit>,
    row: &Row<'_>,
    unit: usize,
    oid: usize,
    lang: Option<LangName>,
) {
    if let Some(((unit, oid), lang)) = row.unit(unit).zip(oid_at(row, oid)).zip(lang) {
        out.push(StoredUnit { unit, oid, lang });
    }
}

fn oid_at(row: &Row<'_>, column: usize) -> Option<BlobId> {
    row.bytes(column).and_then(BlobId::from_bytes)
}

/// The language of a row, shared with the version it matched.
fn lang_at(row: &Row<'_>, column: usize, group: &[Version]) -> Option<LangName> {
    let lang = row.text(column)?;
    group
        .iter()
        .find(|v| &*v.lang.0 == lang)
        .map(|v| v.lang.clone())
}

fn status_at(row: &Row<'_>, column: usize) -> Option<ExtractStatus> {
    match row.int(column)? {
        0 => Some(ExtractStatus::Done),
        1 => Some(ExtractStatus::TimedOut),
        2 => Some(ExtractStatus::TooLarge),
        3 => Some(ExtractStatus::Binary),
        _ => None,
    }
}

fn kind_at(row: &Row<'_>, column: usize) -> Option<DefKind> {
    let code = usize::try_from(row.int(column)?).ok()?;
    Some(DEF_KINDS.get(code).copied().unwrap_or(DefKind::Other))
}

fn visibility(flags: i64) -> Visibility {
    match flags & 0b111 {
        0 => Visibility::Public,
        1 => Visibility::File,
        2 => Visibility::Package,
        3 => Visibility::Module,
        _ => Visibility::Unknown,
    }
}

/// A definition from the columns `name, kind, line, col, end_line, scope,
/// flags, arity` starting at `at`.
fn def_at(row: &Row<'_>, at: usize) -> Option<Def> {
    let int = |c: usize| row.int(at + c).and_then(|v| u32::try_from(v).ok());
    Some(Def {
        name: row.text(at)?.into(),
        kind: kind_at(row, at + 1)?,
        line: int(2)?,
        column: int(3)?,
        end_line: int(4)?,
        scope: row.text(at + 5).map(Box::from),
        visibility: visibility(row.int(at + 6)?),
        arity: row.int(at + 7).map(|a| a.clamp(0, 255) as u8),
    })
}

/// Units recorded for one write transaction (one batch). `Send`: a task
/// can build it up and commit it later.
pub struct StoreWrite {
    write: TableWrite,
    units: usize,
}

impl StoreWrite {
    /// Records the extraction of the blob `oid` under `version`, replacing
    /// a unit of the same key. The key must come from the bytes actually
    /// extracted (「存储表」). A status other than `Done` stores the
    /// `ci_ext` row only; `Cancelled` and `Unsupported` are not stored and
    /// return false. No IO.
    pub fn add(&mut self, oid: BlobId, version: &Version, extraction: &Extraction) -> bool {
        let Some(status) = extraction.status.code() else {
            return false;
        };
        let w = &mut self.write;
        let unit = w.create(&unit_key(oid, version), unit_bytes(extraction));
        let unit = Param::New(unit);
        w.execute(
            INSERT_EXT,
            &[
                unit,
                Param::Bytes(oid.as_bytes()),
                Param::Text(&version.lang.0),
                Param::Int(version.grammar as i64),
                Param::Int(version.query as i64),
                Param::Int(i64::from(status)),
            ],
        );
        if extraction.status == ExtractStatus::Done {
            for def in &extraction.defs {
                w.execute(
                    INSERT_DEF,
                    &[
                        unit,
                        Param::Text(&def.name),
                        Param::Int(def.kind as i64),
                        Param::Int(i64::from(def.line)),
                        Param::Int(i64::from(def.column)),
                        Param::Int(i64::from(def.end_line)),
                        def.scope.as_deref().map_or(Param::Null, Param::Text),
                        Param::Int(i64::from(def.visibility.code())),
                        def.arity.map_or(Param::Null, |a| Param::Int(i64::from(a))),
                    ],
                );
            }
            for name in &extraction.refs {
                w.execute(INSERT_REF, &[Param::Text(name), unit]);
            }
        }
        self.units += 1;
        true
    }

    /// Deletes the unit of `oid` under `version` (tests: a cache that lost
    /// some units).
    #[cfg(test)]
    pub(crate) fn remove(&mut self, oid: BlobId, version: &Version) {
        self.write.remove(&unit_key(oid, version));
        self.units += 1;
    }

    /// Units recorded.
    pub fn len(&self) -> usize {
        self.units
    }

    pub fn is_empty(&self) -> bool {
        self.units == 0
    }

    /// Bytes recorded so far; a batch stays within `lectern_cache::MAX_BATCH`.
    pub fn bytes(&self) -> u64 {
        self.write.bytes()
    }

    /// Commits in one write transaction, all or none. Blocking; executor
    /// tasks only. On `Dropped` the caller keeps its extractions in memory
    /// for the current query and extracts again when next needed.
    pub fn commit(self) -> Written {
        self.write.commit()
    }

    /// Deletes `unit`, as an eviction does: the namespace's eviction
    /// generation grows (tests: an eviction at a chosen moment).
    #[cfg(test)]
    pub fn remove_unit(&mut self, unit: Unit) {
        self.write.remove_unit(unit);
    }
}

/// `r`, counting one statement (`phases`).
fn counted<'r, 'a>(r: &'r mut TableRead<'a>) -> &'r mut TableRead<'a> {
    crate::phases::count("sql statements", 1);
    r
}

/// A read transaction, counted (`phases`).
fn read_counted<T>(set: &TableSet, f: impl FnOnce(&mut TableRead<'_>) -> T) -> Option<T> {
    crate::phases::count("sql read transactions", 1);
    set.read(f)
}
