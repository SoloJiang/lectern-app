//! Storage of extractions (docs/design/code_intel.md「存储表」): one
//! extraction is one unit of `cache`'s custom tables in the namespace
//! `code_intel.tags`, keyed by (blob OID, language, grammar version, query
//! version, part), with its rows in `ci_ext`, `ci_def` and `ci_ref`. Every
//! read is limited to the current (language, grammar version, query version)
//! triples (the version filter), so units of an old grammar or query are
//! never returned and only wait for eviction.
//!
//! The part is -1 for the whole file's extraction, the window's start for a
//! window unit of an opened large file (ADR 0034, 「大文件」). Existence
//! checks and hints look at the whole extraction only; lookups by name and
//! the name table read window units like any other `Done` unit.
//!
//! The dependency tier's units (ADR 0033, 「依赖层」) are in a second
//! namespace, `code_intel.deps`, with the same tables under the prefix `cd_`
//! (`cd_ext`, `cd_def`, `cd_ref`) and three more: the module specifiers of
//! each unit (`cd_spec`, the closure's edges) and the packages' path tables
//! (`cd_pkg`, `cd_pkg_file`, `deps::table`). Both sets of statements come
//! from one macro by prefix ([`Sql`]); the code here serves either, by the
//! [`Store`]'s `TableSet`.
//!
//! All calls but [`Store::new`], [`Store::evictions`] and [`Store::write`]
//! block on SQLite: executor tasks only. SQL runs through `lectern-cache`'s
//! custom-table interface; no rusqlite type appears here.

use lectern_cache::{
    Compression, Key, Namespace, Param, Partition, Row, TableRead, TableSet, TableWrite, Tables,
    Unit, Written,
};
use lectern_services::project::BlobId;

use crate::extract::{Def, ExtractStatus, Extraction, Lang, SpecKind, Specifier, Visibility};
use crate::{DEF_KINDS, DefKind, LangName};

/// The units' namespace. The version changes with the tables or the units'
/// key; a change of extraction rules changes `EXTRACTOR_VERSION` instead.
/// 2: `ci_def_name` is (name NOCASE, unit, kind) instead of (name): the
/// lookups by name read it in pages from a (unit, kind, rowid) cursor and
/// in given units by (name, unit), and the name table reads it in index
/// order without the table. Version 1's tables are dropped on first use and
/// its units deleted over later writes (`lectern-cache`「自定义表」): the
/// units are extracted again, not migrated. 3 (ADR 0033, 0034): `ci_ext`
/// gains the part (with the window's end, line and column) and whether the
/// bytes extracted are the OID's content (`canonical`), status 4
/// (`Windowed`); the key and the unique index gain the part; `ci_def` gains
/// the name's byte offset.
pub const NAMESPACE: Namespace = Namespace {
    name: "code_intel.tags",
    version: 3,
    compression: Compression::None,
};

/// The dependency tier's namespace (ADR 0033): the units of dependency
/// files, their module specifiers and the packages' path tables.
pub const DEPS_NAMESPACE: Namespace = Namespace {
    name: "code_intel.deps",
    version: 1,
    compression: Compression::None,
};

/// The part of a whole file's extraction.
pub const WHOLE: i64 = -1;

/// Most bytes of `code_intel.tags` units in one partition: 224 MiB, with
/// `code_intel.deps`'s [`DEPS_QUOTA`] the 256 MiB of ADR 0033.
pub const QUOTA: u64 = 224 << 20;

/// Most bytes of `code_intel.deps` units in one partition (ADR 0033); the
/// dependency units evict only dependency units.
pub const DEPS_QUOTA: u64 = 32 << 20;

/// OIDs per existence-check query: the fixed `IN (…)` of [`EXISTS`].
pub const EXISTS_BATCH: usize = 500;

/// Version triples per by-name query: a compatibility group of up to this
/// many languages is one query, a larger one several.
pub const GROUP_TRIPLES: usize = 4;

/// Index rows per page of a lookup by name ([`Store::next_defs`],
/// [`Store::next_refs`]): the rows one statement reads, whatever passes,
/// a few milliseconds whatever the name.
pub const NAME_PAGE: usize = 256;

/// Index entries per statement of the name table's scan
/// ([`Store::def_names`]); `stop` is asked between statements.
pub const NAMES_SCAN_PAGE: usize = 4096;

/// Definitions per statement of a lookup by name in given units
/// ([`Store::defs_by_name_in`]).
pub const NAME_IN_LIMIT: usize = 256;

/// `head`, then [`EXISTS_BATCH`] placeholders `?` separated by commas and
/// closed by `)`, then `tail`: a statement's static text with all of an
/// `IN (…)` list's placeholders (`N` is its length).
const fn with_placeholders<const N: usize>(head: &str, tail: &str) -> [u8; N] {
    let mut out = [0u8; N];
    let head = head.as_bytes();
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
    let tail = tail.as_bytes();
    let mut k = 0;
    while k < tail.len() {
        out[i] = tail[k];
        i += 1;
        k += 1;
    }
    out
}

/// The tables of one namespace, by table-name prefix (`ci` for
/// `code_intel.tags`, `cd` for `code_intel.deps`), and any more statements.
macro_rules! schema_for {
    ($p:literal $(, $extra:expr)* $(,)?) => {
        &[
            // `part`: -1 for the whole file's extraction, the window's start
            // for a window unit; `part_end`, `part_line`, `part_col`: a
            // window's end (the next window's start) and the line and byte
            // column of its start, for resuming the chain; on the `Windowed`
            // marker `part_end` is the file's length (where a complete chain
            // ends). `canonical`: the bytes extracted are `oid`'s content, so
            // `ci_def.byte` goes to views.
            concat!(
                "CREATE TABLE ", $p, "_ext (
       unit      INTEGER PRIMARY KEY REFERENCES entry (id) ON DELETE CASCADE,
       oid       BLOB    NOT NULL,
       lang      TEXT    NOT NULL,
       grammar   INTEGER NOT NULL,
       query     INTEGER NOT NULL,
       status    INTEGER NOT NULL,
       part      INTEGER NOT NULL,
       part_end  INTEGER,
       part_line INTEGER,
       part_col  INTEGER,
       canonical INTEGER NOT NULL,
       UNIQUE (oid, lang, grammar, query, part))"
            ),
            concat!(
                "CREATE TABLE ", $p, "_def (
       unit     INTEGER NOT NULL REFERENCES entry (id) ON DELETE CASCADE,
       name     TEXT    NOT NULL,
       kind     INTEGER NOT NULL,
       line     INTEGER NOT NULL,
       col      INTEGER NOT NULL,
       byte     INTEGER NOT NULL,
       end_line INTEGER NOT NULL,
       scope    TEXT,
       flags    INTEGER NOT NULL,
       arity    INTEGER)"
            ),
            // With the unit and the kind: a name's rows in (unit, kind,
            // rowid) order without reading the table (the pages of a lookup,
            // the count of the rows left), one seek per unit for the lookup
            // in given units, and the name table's (name, unit, kind) in
            // index order.
            concat!(
                "CREATE INDEX ", $p, "_def_name ON ", $p,
                "_def (name COLLATE NOCASE, unit, kind)"
            ),
            concat!("CREATE INDEX ", $p, "_def_unit ON ", $p, "_def (unit)"),
            concat!(
                "CREATE TABLE ", $p, "_ref (
       name TEXT    NOT NULL,
       unit INTEGER NOT NULL REFERENCES entry (id) ON DELETE CASCADE,
       PRIMARY KEY (name, unit)) WITHOUT ROWID"
            ),
            concat!("CREATE INDEX ", $p, "_ref_unit ON ", $p, "_ref (unit)"),
            $($extra,)*
        ]
    };
}

/// The statements of one namespace's tables (`schema_for!`), by prefix.
macro_rules! sql_for {
    ($p:literal, specs: $specs:expr) => {
        Sql {
            specs: $specs,
            insert_ext: concat!(
                "INSERT INTO ", $p, "_ext
       (unit, oid, lang, grammar, query, status, part, part_end, part_line, part_col, canonical)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"
            ),
            insert_def: concat!(
                "INSERT INTO ", $p, "_def (unit, name, kind, line, col, byte, end_line, scope, flags, arity)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"
            ),
            insert_ref: concat!(
                "INSERT OR IGNORE INTO ", $p, "_ref (name, unit) VALUES (?1, ?2)"
            ),
            exists: {
                const HEAD: &str = concat!(
                    "SELECT unit, oid, status, part_end FROM ", $p, "_ext
     WHERE part = -1 AND lang = ?1 AND grammar = ?2 AND query = ?3 AND oid IN ("
                );
                // `?` takes the number after the largest so far: `?4` …
                // `?503`.
                const LEN: usize = HEAD.len() + 2 * EXISTS_BATCH;
                const BYTES: [u8; LEN] = with_placeholders::<LEN>(HEAD, "");
                match std::str::from_utf8(&BYTES) {
                    Ok(sql) => sql,
                    Err(_) => panic!("the existence check is ASCII"),
                }
            },
            defs_page: concat!(
                "SELECT d.name, d.kind, d.line, d.col, d.byte, d.end_line, d.scope, d.flags, d.arity,
            e.oid, e.lang, e.grammar, e.query, raw.unit, raw.kind, raw.id, e.canonical
     FROM (SELECT rowid AS id, unit, kind FROM ", $p, "_def INDEXED BY ", $p, "_def_name
           WHERE name = ?1 COLLATE NOCASE AND (unit, kind, rowid) > (?2, ?3, ?4)
           ORDER BY unit, kind, rowid LIMIT ?5) AS raw
     LEFT JOIN ", $p, "_ext e ON e.unit = raw.unit AND e.status = 0
     LEFT JOIN ", $p, "_def d ON d.rowid = raw.id AND e.unit IS NOT NULL"
            ),
            defs_left: concat!(
                "SELECT count(*) FROM ", $p, "_def INDEXED BY ", $p, "_def_name
     WHERE name = ?1 COLLATE NOCASE AND (unit, kind, rowid) > (?2, ?3, ?4)"
            ),
            defs_in: {
                // `?14 > 0` (the limit, always positive) names `?14` before
                // the OIDs, so their bare `?` number from 15.
                const HEAD: &str = concat!(
                    "SELECT d.name, d.kind, d.line, d.col, d.byte, d.end_line, d.scope, d.flags, d.arity,
            e.oid, e.lang, d.unit, e.canonical
     FROM ", $p, "_ext e CROSS JOIN ", $p, "_def d INDEXED BY ", $p, "_def_name ON d.unit = e.unit
     WHERE d.name = ?1 COLLATE NOCASE AND e.status = 0
       AND ((e.lang = ?2 AND e.grammar = ?3 AND e.query = ?4)
         OR (e.lang = ?5 AND e.grammar = ?6 AND e.query = ?7)
         OR (e.lang = ?8 AND e.grammar = ?9 AND e.query = ?10)
         OR (e.lang = ?11 AND e.grammar = ?12 AND e.query = ?13))
       AND ?14 > 0 AND e.oid IN ("
                );
                const TAIL: &str = " LIMIT ?14";
                // `?` takes the number after the largest so far: `?15` …
                // `?514`.
                const LEN: usize = HEAD.len() + 2 * EXISTS_BATCH + TAIL.len();
                const BYTES: [u8; LEN] = with_placeholders::<LEN>(HEAD, TAIL);
                match std::str::from_utf8(&BYTES) {
                    Ok(sql) => sql,
                    Err(_) => panic!("the lookup in units is ASCII"),
                }
            },
            refs_page: concat!(
                "SELECT e.oid, e.lang, e.grammar, e.query, raw.unit,
            e.part, e.part_end, e.part_line, e.part_col, e.canonical
     FROM (SELECT unit FROM ", $p, "_ref WHERE name = ?1 AND unit > ?2 ORDER BY unit LIMIT ?3) AS raw
     LEFT JOIN ", $p, "_ext e ON e.unit = raw.unit AND e.status = 0"
            ),
            refs_left: concat!(
                "SELECT count(*) FROM ", $p, "_ref WHERE name = ?1 AND unit > ?2"
            ),
            units: concat!(
                "SELECT unit, oid, lang, grammar, query FROM ", $p, "_ext WHERE status = 0"
            ),
            names_page: concat!(
                "SELECT name, unit, kind, rowid FROM ", $p, "_def INDEXED BY ", $p, "_def_name
       WHERE name = ?1 COLLATE NOCASE AND (unit, kind, rowid) > (?2, ?3, ?4)
     UNION ALL
     SELECT name, unit, kind, rowid FROM ", $p, "_def INDEXED BY ", $p, "_def_name
       WHERE name > ?1 COLLATE NOCASE
     ORDER BY 1 COLLATE NOCASE, 2, 3, 4 LIMIT ?5"
            ),
            ext_of_unit: concat!("SELECT status FROM ", $p, "_ext WHERE unit = ?1"),
            defs_of_unit: concat!(
                "SELECT name, kind, line, col, byte, end_line, scope, flags, arity FROM ", $p, "_def
     WHERE unit = ?1 ORDER BY rowid"
            ),
            refs_of_unit: concat!(
                "SELECT name FROM ", $p, "_ref WHERE unit = ?1 ORDER BY name"
            ),
            windows: concat!(
                "SELECT unit, part, part_end, part_line, part_col, status, canonical FROM ", $p, "_ext
     WHERE oid = ?1 AND lang = ?2 AND grammar = ?3 AND query = ?4 AND part >= 0
     ORDER BY part"
            ),
        }
    };
}

/// One namespace's statements over its tables ([`schema_for!`]): the same
/// statements for `code_intel.tags` (`ci_*`) and `code_intel.deps`
/// (`cd_*`), so the code of this module serves both.
pub(crate) struct Sql {
    /// `code_intel.deps`: a unit's specifiers are written with it
    /// (`cd_spec`).
    pub specs: bool,
    pub insert_ext: &'static str,
    pub insert_def: &'static str,
    /// References are deduplicated by the extraction; `OR IGNORE` keeps a
    /// duplicate from rolling the whole batch back.
    pub insert_ref: &'static str,
    /// The existence check: whole-file units (`part = -1`) of one version
    /// triple among up to [`EXISTS_BATCH`] OIDs, with a `Windowed` marker's
    /// file length. Static text with all 500 placeholders; unused ones are
    /// bound to NULL, which matches nothing.
    pub exists: &'static str,
    /// One page of the definitions by name (jump step 3, `Named`): the next
    /// `?5` index rows of the name (any case, version or status) after the
    /// cursor (unit, kind, rowid) `?2, ?3, ?4`, in `ci_def_name`'s order.
    /// The limit counts the rows the index gives, not those that pass:
    /// every raw row comes out once (the joins are `LEFT`), its unit's
    /// `ci_ext` row when it is `Done` and its definition then. The caller
    /// keeps the exact name and the current versions, and goes on after the
    /// last raw row; fewer than `?5` raw rows end the lookup. So one
    /// statement reads at most `?5` rows of the name whatever is stale, in
    /// another language or another case (plus, within one (unit, kind), the
    /// rows before the cursor: SQLite seeks (unit, kind) and filters the
    /// rowid).
    pub defs_page: &'static str,
    /// The rows named `?1` after the cursor `?2, ?3, ?4`, whatever their
    /// version and status: an upper bound of what a lookup cut short did
    /// not read. Read from `ci_def_name` alone.
    pub defs_left: &'static str,
    /// The definitions by name in the units of up to [`EXISTS_BATCH`] OIDs
    /// (`?15` …, unused ones NULL), at most `?14` rows: the jump's and
    /// `Named`'s first batch, from the files nearest the query (「候选的分层
    /// 与过滤」). Each OID is one seek in `ci_ext`'s key and one in
    /// `ci_def_name` (name, unit): the cost follows the files asked for, not
    /// how many units define the name.
    pub defs_in: &'static str,
    /// One page of the units referring to a name (references step 1), exact
    /// names: the next `?3` rows of `ci_ref`'s key (name, unit) after unit
    /// `?2`, each with its unit's `ci_ext` row when `Done` (`LEFT`: every
    /// raw row comes out once). As `defs_page`: the limit bounds the rows
    /// read, the caller keeps the current versions, fewer than `?3` end the
    /// lookup.
    pub refs_page: &'static str,
    /// The units referring to `?1` after unit `?2`, whatever their version
    /// and status: what a references lookup cut short by its time box did
    /// not read (an upper bound of those units). From `ci_ref`'s key alone.
    pub refs_left: &'static str,
    /// Every `Done` unit with its version (the name table's first pass): one
    /// scan of `ci_ext`, the versions kept by the caller.
    pub units: &'static str,
    /// The next `?5` entries of `ci_def_name` after the cursor (name, unit,
    /// kind, rowid) `?1` … `?4`, in the index's order (the name table's
    /// second pass, in pages): the rest of the cursor's name, then the names
    /// after it, merged in order without a sort. Covering: no table row is
    /// read.
    pub names_page: &'static str,
    pub ext_of_unit: &'static str,
    /// In insertion order, which is the extraction's order.
    pub defs_of_unit: &'static str,
    pub refs_of_unit: &'static str,
    /// The window units of one key (`part >= 0`), by start: the unique
    /// index's order, no sort. A chain resumes at its first gap; a
    /// `Windowed` marker is believed only while its windows reach the
    /// file's end.
    pub windows: &'static str,
}

/// `code_intel.tags`'s statements.
pub(crate) const TAGS_SQL: Sql = sql_for!("ci", specs: false);
/// `code_intel.deps`'s statements over the same tables.
pub(crate) const DEPS_SQL: Sql = sql_for!("cd", specs: true);

pub(crate) const SCHEMA: &[&str] = schema_for!("ci");

/// `code_intel.tags`'s statements by name, for the tests' own
/// registrations (extra statements, smaller quotas).
#[cfg(test)]
mod tags_sql {
    use super::TAGS_SQL;
    pub(crate) const INSERT_EXT: &str = TAGS_SQL.insert_ext;
    pub(crate) const INSERT_DEF: &str = TAGS_SQL.insert_def;
    pub(crate) const INSERT_REF: &str = TAGS_SQL.insert_ref;
    pub(crate) const EXISTS: &str = TAGS_SQL.exists;
    pub(crate) const DEFS_PAGE: &str = TAGS_SQL.defs_page;
    pub(crate) const DEFS_LEFT: &str = TAGS_SQL.defs_left;
    pub(crate) const DEFS_IN: &str = TAGS_SQL.defs_in;
    pub(crate) const REFS_PAGE: &str = TAGS_SQL.refs_page;
    pub(crate) const REFS_LEFT: &str = TAGS_SQL.refs_left;
    pub(crate) const UNITS: &str = TAGS_SQL.units;
    pub(crate) const NAMES_PAGE: &str = TAGS_SQL.names_page;
    pub(crate) const EXT_OF_UNIT: &str = TAGS_SQL.ext_of_unit;
    pub(crate) const DEFS_OF_UNIT: &str = TAGS_SQL.defs_of_unit;
    pub(crate) const REFS_OF_UNIT: &str = TAGS_SQL.refs_of_unit;
    pub(crate) const WINDOWS: &str = TAGS_SQL.windows;
}
#[cfg(test)]
pub(crate) use tags_sql::*;

/// Every statement of one [`Sql`].
macro_rules! statements_of {
    ($sql:expr $(, $extra:expr)* $(,)?) => {
        &[
            $sql.insert_ext,
            $sql.insert_def,
            $sql.insert_ref,
            $sql.exists,
            $sql.defs_page,
            $sql.defs_left,
            $sql.defs_in,
            $sql.refs_page,
            $sql.refs_left,
            $sql.units,
            $sql.names_page,
            $sql.ext_of_unit,
            $sql.defs_of_unit,
            $sql.refs_of_unit,
            $sql.windows,
            $($extra,)*
        ]
    };
}

pub(crate) const STATEMENTS: &[&str] = statements_of!(TAGS_SQL);

/// The registration of `code_intel.tags`.
pub static TABLES: Tables = Tables {
    namespace: NAMESPACE,
    schema: SCHEMA,
    statements: STATEMENTS,
    quota: QUOTA,
};

/// The module specifiers of a dependency unit (「依赖层」: the closure's
/// edges, by content): one row per specifier as written, `kind` its
/// [`SpecKind`] code. Written with the unit, so a unit found stored gives
/// its edges without being parsed again.
pub(crate) const DEPS_SPEC_TABLE: &str = "CREATE TABLE cd_spec (
       unit INTEGER NOT NULL REFERENCES entry (id) ON DELETE CASCADE,
       kind INTEGER NOT NULL,
       spec TEXT    NOT NULL)";
pub(crate) const DEPS_SPEC_UNIT: &str = "CREATE INDEX cd_spec_unit ON cd_spec (unit)";
/// A package's path table (「身份与包的路径表」): one row per (ecosystem,
/// name, version, identity, package directory), with the five stats of the
/// installed `package.json` (`dev`, `ino`, `size`, `mtime`, `ctime` in
/// nanoseconds) and when it was recorded (nanoseconds of the wall clock);
/// `types`: the `@types` package that gave this one's declarations.
pub(crate) const DEPS_PKG_TABLE: &str = "CREATE TABLE cd_pkg (
       unit     INTEGER PRIMARY KEY REFERENCES entry (id) ON DELETE CASCADE,
       eco      INTEGER NOT NULL,
       name     TEXT    NOT NULL,
       version  TEXT,
       identity BLOB    NOT NULL,
       dev      INTEGER NOT NULL,
       ino      INTEGER NOT NULL,
       size     INTEGER NOT NULL,
       mtime    INTEGER NOT NULL,
       ctime    INTEGER NOT NULL,
       dir      TEXT    NOT NULL,
       recorded INTEGER NOT NULL,
       types    TEXT)";
pub(crate) const DEPS_PKG_NAME: &str = "CREATE INDEX cd_pkg_name ON cd_pkg (name, eco, recorded)";
/// The files of a path table, by their order in the closure (`seq`): the
/// path relative to the dependency root's directory, the OID (NULL for a
/// file never read: `TooLarge`), its five stats and status.
pub(crate) const DEPS_PKG_FILE_TABLE: &str = "CREATE TABLE cd_pkg_file (
       unit   INTEGER NOT NULL REFERENCES entry (id) ON DELETE CASCADE,
       seq    INTEGER NOT NULL,
       path   TEXT    NOT NULL,
       oid    BLOB,
       dev    INTEGER NOT NULL,
       ino    INTEGER NOT NULL,
       size   INTEGER NOT NULL,
       mtime  INTEGER NOT NULL,
       ctime  INTEGER NOT NULL,
       status INTEGER NOT NULL)";
pub(crate) const DEPS_PKG_FILE_UNIT: &str =
    "CREATE INDEX cd_pkg_file_unit ON cd_pkg_file (unit, seq)";

pub(crate) const INSERT_SPEC: &str = "INSERT INTO cd_spec (unit, kind, spec) VALUES (?1, ?2, ?3)";
/// A unit's specifiers in the order written (the extraction's).
pub(crate) const SPECS_OF_UNIT: &str =
    "SELECT kind, spec FROM cd_spec WHERE unit = ?1 ORDER BY rowid";
pub(crate) const INSERT_PKG: &str = "INSERT INTO cd_pkg
       (unit, eco, name, version, identity, dev, ino, size, mtime, ctime, dir, recorded, types)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)";
pub(crate) const INSERT_PKG_FILE: &str = "INSERT INTO cd_pkg_file
       (unit, seq, path, oid, dev, ino, size, mtime, ctime, status)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)";
/// The path table of one key (`?3` the version, NULL for none).
pub(crate) const PKG_BY_KEY: &str =
    "SELECT unit, version, identity, dev, ino, size, mtime, ctime, dir, recorded, types
     FROM cd_pkg WHERE name = ?1 AND eco = ?2 AND version IS ?3 AND identity = ?4 AND dir = ?5";
/// The newest path table of a package name: the stat table when the
/// identity changed or is not known.
pub(crate) const PKG_LATEST: &str =
    "SELECT unit, version, identity, dev, ino, size, mtime, ctime, dir, recorded, types
     FROM cd_pkg WHERE name = ?1 AND eco = ?2 ORDER BY recorded DESC LIMIT 1";
pub(crate) const PKG_FILES: &str =
    "SELECT seq, path, oid, dev, ino, size, mtime, ctime, status FROM cd_pkg_file
     WHERE unit = ?1 ORDER BY seq";

pub(crate) const DEPS_SCHEMA: &[&str] = schema_for!(
    "cd",
    DEPS_SPEC_TABLE,
    DEPS_SPEC_UNIT,
    DEPS_PKG_TABLE,
    DEPS_PKG_NAME,
    DEPS_PKG_FILE_TABLE,
    DEPS_PKG_FILE_UNIT,
);
pub(crate) const DEPS_STATEMENTS: &[&str] = statements_of!(
    DEPS_SQL,
    INSERT_SPEC,
    SPECS_OF_UNIT,
    INSERT_PKG,
    INSERT_PKG_FILE,
    PKG_BY_KEY,
    PKG_LATEST,
    PKG_FILES,
);

/// The registration of `code_intel.deps` (ADR 0033).
pub static DEPS_TABLES: Tables = Tables {
    namespace: DEPS_NAMESPACE,
    schema: DEPS_SCHEMA,
    statements: DEPS_STATEMENTS,
    quota: DEPS_QUOTA,
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

/// The unit's key: `Key::new().bytes(oid).str(lang).u64(grammar).u64(query)`
/// and the part, `part` [`WHOLE`] or a window's start, as its 64 bits
/// (`lectern-cache`'s `Key` has no signed field: -1 is `u64::MAX`, which
/// no window starts at).
pub fn unit_key(oid: BlobId, version: &Version, part: i64) -> Key {
    Key::new()
        .bytes(oid.as_bytes())
        .str(&version.lang.0)
        .u64(version.grammar)
        .u64(version.query)
        .u64(part as u64)
}

/// Where a window unit's window lies in its file (「大文件」): from `start`
/// to `end` (the next window's start), `start` at `line` and byte `column`
/// of the whole file.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Part {
    pub start: u64,
    pub end: u64,
    pub line: u32,
    pub column: u32,
}

/// What a unit is besides its extraction ([`StoreWrite::add`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct UnitMeta {
    /// A window unit's window; None for the whole file's extraction.
    pub part: Option<Part>,
    /// The bytes extracted are the OID's content: the raw bytes hash to it,
    /// or the content comes from the object database or a dependency. Not
    /// when only `canonical_oid` made them that content (a CRLF worktree
    /// file): then no byte offset goes to views (「字节偏移」).
    pub canonical: bool,
    /// The file's length, on a `Windowed` marker: where a complete chain of
    /// its windows ends.
    pub len: Option<u64>,
}

impl UnitMeta {
    /// The whole file's extraction.
    pub fn whole(canonical: bool) -> UnitMeta {
        UnitMeta {
            part: None,
            canonical,
            len: None,
        }
    }
}

/// One window unit as stored ([`Store::windows`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WindowRow {
    pub unit: Unit,
    pub part: Part,
    /// `Done`, or `TimedOut` (the window has no rows, its chain is not
    /// complete).
    pub status: ExtractStatus,
    pub canonical: bool,
}

/// Whether `rows` (by start) chain from 0 to `len` without a gap or a
/// window that timed out: a `Windowed` marker holds the file's content only
/// then.
pub fn chain_complete(rows: &[WindowRow], len: u64) -> bool {
    let mut at = 0;
    for row in rows {
        if row.part.start != at {
            continue;
        }
        if row.status != ExtractStatus::Done {
            return false;
        }
        at = row.part.end;
    }
    at == len && len > 0
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

/// The bytes a dependency unit's specifiers add (`cd_spec`, a row and its
/// index entry each): `Σ (len + 32)`.
pub fn specs_bytes(extraction: &Extraction) -> u64 {
    if extraction.status != ExtractStatus::Done {
        return 0;
    }
    extraction
        .specs
        .iter()
        .map(|s| s.text.len() as u64 + 32)
        .sum()
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

    /// Queues more units read under `generation` (a `Windowed` hit's
    /// window units: marked as used with their marker).
    pub fn push_units(&mut self, generation: u64, units: &[Unit]) {
        if generation != self.generation {
            self.units.clear();
            self.generation = generation;
        }
        self.units.extend_from_slice(units);
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
    /// A window unit's window (by-name references only; the name table
    /// does not read it).
    pub part: Option<Part>,
    /// [`UnitMeta::canonical`] (by-name references only).
    pub canonical: bool,
}

/// A definition found by name, with its unit's OID and language.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StoredDef {
    pub unit: Unit,
    pub oid: BlobId,
    pub lang: LangName,
    pub def: Def,
    /// [`UnitMeta::canonical`]: `def.byte` may go to a view.
    pub canonical: bool,
}

impl StoredDef {
    /// The definition's byte offset for a view (「字节偏移」).
    pub fn byte(&self) -> Option<u64> {
        self.canonical.then_some(self.def.byte)
    }
}

/// Where a lookup of a name in pages stands ([`Store::next_defs`],
/// [`Store::next_refs`]): the versions it keeps and the last raw index row
/// read, (unit, kind, rowid) for definitions, the unit for references. One
/// raw scan covers every version. Holds no connection; a query keeps it
/// between its batches.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct NamePager {
    name: Box<str>,
    versions: Vec<Version>,
    after: (i64, i64, i64),
    finished: bool,
}

impl NamePager {
    pub fn new(name: &str, versions: &[Version]) -> NamePager {
        NamePager {
            name: name.into(),
            versions: versions.to_vec(),
            after: (i64::MIN, i64::MIN, i64::MIN),
            finished: versions.is_empty(),
        }
    }

    /// Every page is read.
    pub fn done(&self) -> bool {
        self.finished
    }

    fn finish(&mut self) {
        self.finished = true;
    }

    fn version_at(&self, row: &Row<'_>, at: usize) -> Option<LangName> {
        version_in(&self.versions, row, at)
    }
}

/// The language of a row's (language, grammar, query) at columns `at` …
/// `at + 2` when that triple is one of `versions`.
fn version_in(versions: &[Version], row: &Row<'_>, at: usize) -> Option<LangName> {
    let (lang, grammar, query) = (row.text(at)?, row.int(at + 1)?, row.int(at + 2)?);
    versions
        .iter()
        .find(|v| &*v.lang.0 == lang && v.grammar as i64 == grammar && v.query as i64 == query)
        .map(|v| v.lang.clone())
}

// Raw index rows each page statement read, by statement (tests: the bound
// per statement).
#[cfg(test)]
thread_local! {
    pub(crate) static PAGE_ROWS: std::cell::RefCell<Vec<(&'static str, usize)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn record_page(_what: &'static str, _rows: usize) {
    #[cfg(test)]
    PAGE_ROWS.with(|p| p.borrow_mut().push((_what, _rows)));
}

/// `code_intel.tags` (or `code_intel.deps`, [`Store::deps`]) in one
/// partition (`cache.repo(common_dir)`, or `cache.folder(root)` for a
/// folder project). `Clone + Send + Sync`; holds no connection.
#[derive(Clone)]
pub struct Store {
    pub(crate) set: TableSet,
    sql: &'static Sql,
}

impl Store {
    /// No IO; any thread. The tables are created by the first read or write.
    pub fn new(partition: &Partition) -> Store {
        Store::with_tables(partition, &TABLES)
    }

    /// `code_intel.deps` in the partition (ADR 0033): the same reads and
    /// writes over the `cd_*` tables, plus the specifiers and the path
    /// tables (`deps::table`). No IO.
    pub fn deps(partition: &Partition) -> Store {
        Store::deps_with_tables(partition, &DEPS_TABLES)
    }

    /// The same store under another registration of the namespace (tests: a
    /// smaller quota, extra statements).
    pub(crate) fn with_tables(partition: &Partition, tables: &'static Tables) -> Store {
        Store {
            set: partition.tables(tables),
            sql: &TAGS_SQL,
        }
    }

    /// [`Store::deps`] under another registration of `code_intel.deps`
    /// (tests: a smaller quota).
    pub(crate) fn deps_with_tables(partition: &Partition, tables: &'static Tables) -> Store {
        Store {
            set: partition.tables(tables),
            sql: &DEPS_SQL,
        }
    }

    /// The eviction generation of the namespace in this partition
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
            sql: self.sql,
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
        self.exists_with_windows(version, oids)
            .map(|(found, _)| found)
    }

    /// [`exists`](Self::exists), and the window units of its `Windowed`
    /// hits, to be marked as used with them. A `Windowed` hit costs one
    /// more statement ([`WINDOWS`]): its windows must still chain from 0 to
    /// the file's end without a timed-out one, else (a window evicted) the
    /// hit is `TooLarge`: the file counts as not searched, and opening it
    /// resumes its chain.
    pub fn exists_with_windows(
        &self,
        version: &Version,
        oids: &[BlobId],
    ) -> Option<(Vec<Found>, Vec<Unit>)> {
        let lang = &*version.lang.0;
        let sql = self.sql;
        read_counted(&self.set, |r| {
            let mut found = Vec::new();
            let mut lens = Vec::new();
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
                let ok = counted(r).query(sql.exists, &params, |row| {
                    let hit = row.unit(0).zip(oid_at(row, 1)).zip(status_at(row, 2));
                    if let Some(((unit, oid), status)) = hit {
                        found.push(Found { oid, unit, status });
                        lens.push(row.int(3).and_then(|n| u64::try_from(n).ok()));
                    }
                    true
                });
                if !ok {
                    break;
                }
            }
            let mut windows = Vec::new();
            for (f, len) in found.iter_mut().zip(lens) {
                if f.status != ExtractStatus::Windowed {
                    continue;
                }
                let rows = windows_in(sql, r, f.oid, version);
                if len.is_some_and(|len| chain_complete(&rows, len)) {
                    windows.extend(rows.iter().map(|w| w.unit));
                } else {
                    f.status = ExtractStatus::TooLarge;
                }
            }
            (found, windows)
        })
    }

    /// The window units stored for `oid` under `version`, by start.
    pub fn windows(&self, oid: BlobId, version: &Version) -> Option<Vec<WindowRow>> {
        read_counted(&self.set, |r| windows_in(self.sql, r, oid, version))
    }

    /// The content of a `Windowed` file: its `Done` windows' definitions
    /// (by start) and reference names (deduplicated, in byte order), as one
    /// extraction, with whether every window is canonical. None when the
    /// partition cannot be read.
    pub fn load_windows(&self, oid: BlobId, version: &Version) -> Option<(Extraction, bool)> {
        let sql = self.sql;
        read_counted(&self.set, |r| {
            let rows = windows_in(sql, r, oid, version);
            let mut defs = Vec::new();
            let mut refs: Vec<Box<str>> = Vec::new();
            let mut canonical = true;
            for row in rows.iter().filter(|w| w.status == ExtractStatus::Done) {
                canonical &= row.canonical;
                if let Some(e) = load(sql, r, row.unit) {
                    defs.extend(e.defs);
                    refs.extend(e.refs);
                }
            }
            refs.sort_unstable();
            refs.dedup();
            (
                Extraction {
                    status: ExtractStatus::Done,
                    defs,
                    refs,
                    specs: Vec::new(),
                },
                canonical,
            )
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

    /// The next page of a lookup by name ([`NamePager`]): the definitions
    /// named exactly `name` in `Done` units of the pager's versions among
    /// the next `limit` index rows of the name (any case, version or status)
    /// after the pager's cursor ([`DEFS_PAGE`]). One statement of at most
    /// `limit` raw rows; a page may hold no definition. Fewer raw rows than
    /// `limit` end the lookup. `None` when the partition cannot be read (the
    /// pager is then done: nothing more can be learned).
    pub fn next_defs(&self, pager: &mut NamePager, limit: usize) -> Option<Vec<StoredDef>> {
        if pager.done() {
            return Some(Vec::new());
        }
        let limit = limit.max(1);
        let (unit, kind, id) = pager.after;
        let p: &NamePager = pager;
        let read = read_counted(&self.set, |r| {
            let mut out = Vec::new();
            let mut raw = 0usize;
            let mut last = p.after;
            let params = [
                Param::Text(&p.name),
                Param::Int(unit),
                Param::Int(kind),
                Param::Int(id),
                Param::Int(limit as i64),
            ];
            let ok = counted(r).query(self.sql.defs_page, &params, |row| {
                raw += 1;
                if let Some(at) = row.int(13).zip(row.int(14)).zip(row.int(15)) {
                    let at = (at.0.0, at.0.1, at.1);
                    last = last.max(at);
                }
                // The exact name (`NOCASE` reads the other cases too), a
                // `Done` unit of a kept version.
                if row.text(0) == Some(&*p.name)
                    && let Some((((def, oid), lang), unit)) = def_at(row, 0)
                        .zip(oid_at(row, 9))
                        .zip(p.version_at(row, 10))
                        .zip(row.unit(13))
                {
                    out.push(StoredDef {
                        unit,
                        oid,
                        lang,
                        def,
                        canonical: row.int(16) == Some(1),
                    });
                }
                true
            });
            record_page("defs", raw);
            ok.then_some((out, raw, last))
        })
        .flatten();
        let Some((out, raw, last)) = read else {
            pager.finish();
            return None;
        };
        pager.after = last;
        if raw < limit {
            pager.finish();
        }
        Some(out)
    }

    /// The rows named `name` (any case, version or status) that a lookup
    /// cut short at `pager` has not read: those after its cursor. An upper
    /// bound of the definitions it left out (index only, no table row
    /// read). 0 when the partition cannot be read.
    pub fn defs_left(&self, pager: &NamePager) -> u64 {
        if pager.done() {
            return 0;
        }
        let (unit, kind, id) = pager.after;
        read_counted(&self.set, |r| {
            let mut n = 0u64;
            counted(r).query(
                self.sql.defs_left,
                &[
                    Param::Text(&pager.name),
                    Param::Int(unit),
                    Param::Int(kind),
                    Param::Int(id),
                ],
                |row| {
                    n = row.int(0).map_or(0, |c| c.max(0) as u64);
                    false
                },
            );
            n
        })
        .unwrap_or(0)
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
                    let ok = counted(r).query(self.sql.defs_in, &params, |row| {
                        if row.text(0) != Some(name) {
                            return true;
                        }
                        let found = def_at(row, 0)
                            .zip(oid_at(row, 9))
                            .zip(lang_at(row, 10, group))
                            .zip(row.unit(11));
                        if let Some((((def, oid), lang), unit)) = found {
                            out.push(StoredDef {
                                unit,
                                oid,
                                lang,
                                def,
                                canonical: row.int(12) == Some(1),
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

    /// The next page of the units referring to a name ([`NamePager`]): the
    /// `Done` units of the pager's versions among the next `limit` rows of
    /// `ci_ref`'s key for the name after the pager's unit ([`REFS_PAGE`]).
    /// One statement of at most `limit` raw rows; a page may hold no unit.
    /// `None` when the partition cannot be read (the pager is then done).
    pub fn next_refs(&self, pager: &mut NamePager, limit: usize) -> Option<Vec<StoredUnit>> {
        if pager.done() {
            return Some(Vec::new());
        }
        let limit = limit.max(1);
        let p: &NamePager = pager;
        let read = read_counted(&self.set, |r| {
            let mut out = Vec::new();
            let mut last = p.after.0;
            let mut raw = 0usize;
            let params = [
                Param::Text(&p.name),
                Param::Int(p.after.0),
                Param::Int(limit as i64),
            ];
            let ok = counted(r).query(self.sql.refs_page, &params, |row| {
                raw += 1;
                last = row.int(4).map_or(last, |u| last.max(u));
                push_unit(&mut out, row, 4, 0, p.version_at(row, 1));
                if let Some(u) = out.last_mut().filter(|u| Some(u.unit) == row.unit(4)) {
                    u.part = part_at(row, 5);
                    u.canonical = row.int(9) == Some(1);
                }
                true
            });
            record_page("refs", raw);
            ok.then_some((out, last, raw))
        })
        .flatten();
        let Some((out, last, raw)) = read else {
            pager.finish();
            return None;
        };
        pager.after.0 = last;
        if raw < limit {
            pager.finish();
        }
        Some(out)
    }

    /// The units referring to the pager's name that a references lookup cut
    /// short at `pager` has not read: those after its unit, any version or
    /// status ([`REFS_LEFT`]). An upper bound; 0 when done or unreadable.
    pub fn refs_left(&self, pager: &NamePager) -> u64 {
        if pager.done() {
            return 0;
        }
        read_counted(&self.set, |r| {
            let mut n = 0u64;
            counted(r).query(
                self.sql.refs_left,
                &[Param::Text(&pager.name), Param::Int(pager.after.0)],
                |row| {
                    n = row.int(0).map_or(0, |c| c.max(0) as u64);
                    false
                },
            );
            n
        })
        .unwrap_or(0)
    }

    /// Every `Done` unit of `versions` (the name table's first pass, before
    /// filtering by the map): one scan of `ci_ext`.
    pub fn units(&self, versions: &[Version]) -> Option<Vec<StoredUnit>> {
        read_counted(&self.set, |r| {
            let mut out = Vec::new();
            counted(r).query(self.sql.units, &[], |row| {
                push_unit(&mut out, row, 0, 1, version_in(versions, row, 2));
                true
            });
            out
        })
    }

    /// Calls `f` once with each distinct definition name (exact case) of
    /// `units` (sorted by ID) and the union of its kinds' bits
    /// (`def_kind_bit`), in `ci_def_name`'s order: names equal but for
    /// ASCII case are next to each other, so a name is compared with the
    /// run it falls in, and nothing is allocated per definition. Read in
    /// statements of [`NAMES_SCAN_PAGE`] index entries, `stop` asked
    /// between them. False when the partition cannot be read or `stop` said
    /// so; `f` then saw part of the names.
    pub fn def_names(
        &self,
        units: &[Unit],
        stop: &dyn Fn() -> bool,
        mut f: impl FnMut(&str, u16),
    ) -> bool {
        debug_assert!(units.is_sorted(), "def_names takes sorted units");
        let wanted = UnitSet::of(units);
        // The run of names equal but for ASCII case: each spelling once.
        let mut run: Vec<(String, u16)> = Vec::new();
        let mut cursor = (String::new(), i64::MIN, i64::MIN, i64::MIN);
        loop {
            if stop() {
                return false;
            }
            let read = self.set.read(|r| {
                let mut raw = 0usize;
                let mut next: Option<(i64, i64, i64)> = None;
                let mut next_name: Option<String> = None;
                let params = [
                    Param::Text(&cursor.0),
                    Param::Int(cursor.1),
                    Param::Int(cursor.2),
                    Param::Int(cursor.3),
                    Param::Int(NAMES_SCAN_PAGE as i64),
                ];
                let ok = counted(r).query(self.sql.names_page, &params, |row| {
                    raw += 1;
                    if raw == NAMES_SCAN_PAGE
                        && let (Some(name), Some(id), Some(code), Some(rowid)) =
                            (row.text(0), row.int(1), row.int(2), row.int(3))
                    {
                        next = Some((id, code, rowid));
                        next_name = Some(name.to_owned());
                    }
                    // In index order the units change from row to row: the
                    // unit is looked up first, the name read only for those
                    // wanted.
                    if !row.int(1).is_some_and(|id| wanted.contains(id)) {
                        return true;
                    }
                    let (Some(name), Some(kind)) = (row.text(0), kind_at(row, 2)) else {
                        return true;
                    };
                    let bit = crate::def_kind_bit(kind);
                    if run
                        .first()
                        .is_some_and(|(n, _)| !n.eq_ignore_ascii_case(name))
                    {
                        for (n, kinds) in run.drain(..) {
                            f(&n, kinds);
                        }
                    }
                    match run.iter_mut().find(|(n, _)| n == name) {
                        Some((_, kinds)) => *kinds |= bit,
                        None => run.push((name.to_owned(), bit)),
                    }
                    true
                });
                record_page("names", raw);
                crate::phases::count("names: index entries", raw as u64);
                ok.then_some(next.zip(next_name))
            });
            match read.flatten() {
                None => return false,
                Some(Some(((id, code, rowid), name))) => cursor = (name, id, code, rowid),
                Some(None) => break,
            }
        }
        for (n, kinds) in run.drain(..) {
            f(&n, kinds);
        }
        true
    }

    /// One unit's extraction as stored: definitions in the extraction's
    /// order, reference names in byte order. `None` when the unit is gone or
    /// the partition cannot be read.
    pub fn load(&self, unit: Unit) -> Option<Extraction> {
        read_counted(&self.set, |r| load(self.sql, r, unit)).flatten()
    }

    /// The module specifiers stored with each of `units`, in order
    /// (`code_intel.deps` only: the closure's edges of units found stored,
    /// 「依赖层」). One read transaction; None when the partition cannot be
    /// read.
    pub(crate) fn specs_of(&self, units: &[Unit]) -> Option<Vec<Vec<Specifier>>> {
        read_counted(&self.set, |r| {
            let mut out = Vec::with_capacity(units.len());
            for unit in units {
                let mut specs = Vec::new();
                counted(r).query(SPECS_OF_UNIT, &[Param::Int(unit.id())], |row| {
                    if let (Some(kind), Some(text)) =
                        (row.int(0).and_then(SpecKind::from_code), row.text(1))
                    {
                        specs.push(Specifier {
                            kind,
                            text: text.into(),
                        });
                    }
                    true
                });
                out.push(specs);
            }
            out
        })
    }
}

/// A set of unit IDs asked once per index row: a bitset over the IDs when
/// they are dense enough (the largest at most [`UnitSet::BITS_UP_TO`]),
/// else the sorted list, searched.
enum UnitSet<'a> {
    Bits(Vec<u64>),
    Sorted(&'a [Unit]),
}

impl<'a> UnitSet<'a> {
    /// 16 Mi IDs: a 2 MiB bitset at most, freed with the build.
    const BITS_UP_TO: i64 = 1 << 24;

    fn of(units: &'a [Unit]) -> UnitSet<'a> {
        let max = units.last().map_or(0, |u| u.id());
        if units.first().is_some_and(|u| u.id() < 0) || max > Self::BITS_UP_TO {
            return UnitSet::Sorted(units);
        }
        let mut bits = vec![0u64; (max as usize) / 64 + 1];
        for u in units {
            let id = u.id() as usize;
            bits[id / 64] |= 1 << (id % 64);
        }
        UnitSet::Bits(bits)
    }

    fn contains(&self, id: i64) -> bool {
        match self {
            UnitSet::Bits(bits) => usize::try_from(id)
                .ok()
                .and_then(|id| bits.get(id / 64).map(|w| w & (1 << (id % 64)) != 0))
                .unwrap_or(false),
            UnitSet::Sorted(units) => units.binary_search_by_key(&id, |u| u.id()).is_ok(),
        }
    }
}

/// One unit's extraction, without its specifiers (`Store::specs_of`).
fn load(sql: &Sql, r: &mut TableRead<'_>, unit: Unit) -> Option<Extraction> {
    let id = [Param::Int(unit.id())];
    let mut status = None;
    counted(r).query(sql.ext_of_unit, &id, |row| {
        status = status_at(row, 0);
        false
    });
    let status = status?;
    let mut defs = Vec::new();
    counted(r).query(sql.defs_of_unit, &id, |row| {
        defs.extend(def_at(row, 0));
        true
    });
    let mut refs = Vec::new();
    counted(r).query(sql.refs_of_unit, &id, |row| {
        refs.extend(row.text(0).map(Box::from));
        true
    });
    Some(Extraction {
        status,
        defs,
        refs,
        specs: Vec::new(),
    })
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
        out.push(StoredUnit {
            unit,
            oid,
            lang,
            part: None,
            canonical: false,
        });
    }
}

/// A window from the columns `part, part_end, part_line, part_col` at
/// `at`; None for a whole file's unit.
fn part_at(row: &Row<'_>, at: usize) -> Option<Part> {
    let start = u64::try_from(row.int(at)?).ok()?;
    Some(Part {
        start,
        end: u64::try_from(row.int(at + 1)?).ok()?,
        line: u32::try_from(row.int(at + 2)?).ok()?,
        column: u32::try_from(row.int(at + 3)?).ok()?,
    })
}

/// [`Store::windows`] in a read transaction.
fn windows_in(sql: &Sql, r: &mut TableRead<'_>, oid: BlobId, version: &Version) -> Vec<WindowRow> {
    let mut rows = Vec::new();
    counted(r).query(
        sql.windows,
        &[
            Param::Bytes(oid.as_bytes()),
            Param::Text(&version.lang.0),
            Param::Int(version.grammar as i64),
            Param::Int(version.query as i64),
        ],
        |row| {
            let found = row.unit(0).zip(part_at(row, 1)).zip(status_at(row, 5));
            if let Some(((unit, part), status)) = found {
                rows.push(WindowRow {
                    unit,
                    part,
                    status,
                    canonical: row.int(6) == Some(1),
                });
            }
            true
        },
    );
    rows
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
        4 => Some(ExtractStatus::Windowed),
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

/// A definition from the columns `name, kind, line, col, byte, end_line,
/// scope, flags, arity` starting at `at`.
fn def_at(row: &Row<'_>, at: usize) -> Option<Def> {
    let int = |c: usize| row.int(at + c).and_then(|v| u32::try_from(v).ok());
    Some(Def {
        name: row.text(at)?.into(),
        kind: kind_at(row, at + 1)?,
        line: int(2)?,
        column: int(3)?,
        byte: row.int(at + 4).and_then(|v| u64::try_from(v).ok())?,
        end_line: int(5)?,
        scope: row.text(at + 6).map(Box::from),
        visibility: visibility(row.int(at + 7)?),
        arity: row.int(at + 8).map(|a| a.clamp(0, 255) as u8),
    })
}

/// Units recorded for one write transaction (one batch). `Send`: a task
/// can build it up and commit it later.
pub struct StoreWrite {
    pub(crate) write: TableWrite,
    sql: &'static Sql,
    pub(crate) units: usize,
}

impl StoreWrite {
    /// Records the whole-file extraction of the blob `oid` under `version`
    /// from bytes that are `oid`'s content ([`add_unit`](Self::add_unit)
    /// with [`UnitMeta::whole`]`(true)`).
    pub fn add(&mut self, oid: BlobId, version: &Version, extraction: &Extraction) -> bool {
        self.add_unit(oid, version, extraction, UnitMeta::whole(true))
    }

    /// Records the extraction of the blob `oid` under `version`, replacing
    /// a unit of the same key (the part included: `meta.part`). The key must
    /// come from the bytes actually extracted (「存储表」). A status other
    /// than `Done` stores the `ci_ext` row only; `Cancelled`, `Unsupported`
    /// and `Parked` are not stored and return false. No IO.
    pub fn add_unit(
        &mut self,
        oid: BlobId,
        version: &Version,
        extraction: &Extraction,
        meta: UnitMeta,
    ) -> bool {
        let Some(status) = extraction.status.code() else {
            return false;
        };
        let w = &mut self.write;
        let sql = self.sql;
        let part = meta.part.map_or(WHOLE, |p| p.start as i64);
        let bytes = unit_bytes(extraction)
            + if sql.specs {
                specs_bytes(extraction)
            } else {
                0
            };
        let unit = w.create(&unit_key(oid, version, part), bytes);
        let unit = Param::New(unit);
        let int_or_null = |v: Option<i64>| v.map_or(Param::Null, Param::Int);
        w.execute(
            sql.insert_ext,
            &[
                unit,
                Param::Bytes(oid.as_bytes()),
                Param::Text(&version.lang.0),
                Param::Int(version.grammar as i64),
                Param::Int(version.query as i64),
                Param::Int(i64::from(status)),
                Param::Int(part),
                int_or_null(
                    meta.part
                        .map(|p| p.end as i64)
                        .or(meta.len.map(|n| n as i64)),
                ),
                int_or_null(meta.part.map(|p| i64::from(p.line))),
                int_or_null(meta.part.map(|p| i64::from(p.column))),
                Param::Int(i64::from(meta.canonical)),
            ],
        );
        if extraction.status == ExtractStatus::Done {
            for def in &extraction.defs {
                w.execute(
                    sql.insert_def,
                    &[
                        unit,
                        Param::Text(&def.name),
                        Param::Int(def.kind as i64),
                        Param::Int(i64::from(def.line)),
                        Param::Int(i64::from(def.column)),
                        Param::Int(def.byte as i64),
                        Param::Int(i64::from(def.end_line)),
                        def.scope.as_deref().map_or(Param::Null, Param::Text),
                        Param::Int(i64::from(def.visibility.code())),
                        def.arity.map_or(Param::Null, |a| Param::Int(i64::from(a))),
                    ],
                );
            }
            for name in &extraction.refs {
                w.execute(sql.insert_ref, &[Param::Text(name), unit]);
            }
            // A dependency unit's edges (「依赖层」): with the unit, by
            // content. Only a `Done` extraction has them.
            if sql.specs {
                for spec in &extraction.specs {
                    w.execute(
                        INSERT_SPEC,
                        &[
                            unit,
                            Param::Int(i64::from(spec.kind.code())),
                            Param::Text(&spec.text),
                        ],
                    );
                }
            }
        }
        self.units += 1;
        true
    }

    /// Deletes the unit of `oid` under `version` (tests: a cache that lost
    /// some units).
    #[cfg(test)]
    pub(crate) fn remove(&mut self, oid: BlobId, version: &Version) {
        self.write.remove(&unit_key(oid, version, WHOLE));
        self.units += 1;
    }

    /// Deletes the window unit of `oid` starting at `start` (tests: an
    /// evicted window).
    #[cfg(test)]
    pub(crate) fn remove_window(&mut self, oid: BlobId, version: &Version, start: u64) {
        self.write.remove(&unit_key(oid, version, start as i64));
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
