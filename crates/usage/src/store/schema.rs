//! The tables (docs/design/usage.md「数据库」) and their versions.
//!
//! `PRAGMA user_version` is the table version and only moves forward; a
//! migration never deletes observations. `meta.derived` is the version of
//! the algorithm behind the derived tables (`usage_value`, `fact`): when it
//! changes they are rebuilt from the observations.

/// `PRAGMA application_id`: 0x4C435553, "LCUS".
pub(crate) const APPLICATION_ID: i32 = 0x4C43_5553;

/// The version of the derived tables' algorithm, kept in `meta.derived`.
pub(crate) const DERIVED_VERSION: i64 = 1;

/// One step from `from` to `from + 1`.
pub(crate) struct Migration {
    pub(crate) from: i32,
    pub(crate) sql: &'static str,
}

/// A table version: how to create it and how to reach it from older ones.
pub(crate) struct Schema {
    pub(crate) version: i32,
    pub(crate) create: &'static str,
    pub(crate) migrations: &'static [Migration],
}

/// The tables this version writes.
pub(crate) const CURRENT: Schema = Schema {
    version: 1,
    create: CREATE_V1,
    migrations: &[],
};

/// Location kinds.
pub(crate) const LOCATION_GIT: i64 = 0;
pub(crate) const LOCATION_DIR: i64 = 1;

/// Project kinds.
pub(crate) const PROJECT_GIT: i64 = 0;
pub(crate) const PROJECT_FOLDER: i64 = 1;

/// Observation tables in the order a salvage copies them, then the derived
/// ones it rebuilds. Every one is a rowid table.
pub(crate) const OBSERVATION_TABLES: &[&str] = &[
    "intern",
    "location",
    "project",
    "session",
    "session_gen",
    "turn",
    "usage_source",
];

/// Version 1 (step 1 of docs/design/usage.md「验收」). Strings and paths are
/// interned (`intern`) and referenced by ID. Sources and turns are
/// `AUTOINCREMENT`: a replay deletes a generation's rows, and a reused ID
/// would let the old row's facts be deleted under the new one's. Columns that later steps fill
/// (`fact.segment`, `fact.kind`, `fact.review`) are here from the start.
pub(crate) const CREATE_V1: &str = "
CREATE TABLE meta (
  name  TEXT PRIMARY KEY NOT NULL,
  value
) WITHOUT ROWID;

CREATE TABLE intern (
  id   INTEGER PRIMARY KEY,
  text BLOB NOT NULL UNIQUE
);

CREATE TABLE location (
  id   INTEGER PRIMARY KEY,
  kind INTEGER NOT NULL,
  path BLOB NOT NULL,
  UNIQUE (kind, path)
);

CREATE TABLE project (
  id       INTEGER PRIMARY KEY,
  pid      BLOB NOT NULL UNIQUE,
  kind     INTEGER NOT NULL,
  location INTEGER NOT NULL UNIQUE,
  opened   INTEGER NOT NULL DEFAULT 0,
  name     TEXT
);

CREATE TABLE session (
  id                  INTEGER PRIMARY KEY,
  source              INTEGER NOT NULL,
  sid                 INTEGER NOT NULL,
  agent               INTEGER NOT NULL,
  parent_source       INTEGER,
  parent_sid          INTEGER,
  gen                 INTEGER NOT NULL DEFAULT 1,
  replay              INTEGER,
  cursor              BLOB NOT NULL,
  inheritance_unknown INTEGER NOT NULL DEFAULT 0,
  reached_end         INTEGER NOT NULL DEFAULT 0,
  end_stamp           BLOB,
  UNIQUE (source, sid)
);

CREATE TABLE session_gen (
  id      INTEGER PRIMARY KEY,
  session INTEGER NOT NULL,
  gen     INTEGER NOT NULL,
  cwd     INTEGER,
  model   INTEGER,
  turn    INTEGER,
  UNIQUE (session, gen)
);

CREATE TABLE turn (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  session    INTEGER NOT NULL,
  gen        INTEGER NOT NULL,
  tid        INTEGER NOT NULL,
  seq        INTEGER NOT NULL,
  started_at INTEGER NOT NULL,
  ended_at   INTEGER,
  location   INTEGER,
  UNIQUE (session, gen, tid)
);

CREATE TABLE usage_source (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  key         BLOB NOT NULL,
  session     INTEGER NOT NULL,
  gen         INTEGER NOT NULL,
  pos         INTEGER NOT NULL,
  seq         INTEGER NOT NULL,
  part        INTEGER NOT NULL,
  at          INTEGER NOT NULL,
  turn        INTEGER,
  location    INTEGER,
  model       INTEGER NOT NULL,
  input       INTEGER NOT NULL,
  output      INTEGER NOT NULL,
  cache_read  INTEGER NOT NULL,
  cache_write INTEGER NOT NULL,
  sidechain   INTEGER NOT NULL,
  superseded  INTEGER NOT NULL,
  inherited   INTEGER NOT NULL,
  UNIQUE (session, gen, pos, seq, part)
);
CREATE INDEX usage_source_key ON usage_source (key);

CREATE TABLE usage_value (
  key    BLOB PRIMARY KEY NOT NULL,
  source INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE fact (
  id          INTEGER PRIMARY KEY,
  at          INTEGER NOT NULL,
  bucket      INTEGER NOT NULL,
  source      INTEGER NOT NULL,
  location    INTEGER NOT NULL,
  project     INTEGER,
  segment     INTEGER,
  kind        INTEGER NOT NULL DEFAULT 0,
  review      INTEGER NOT NULL DEFAULT 0,
  agent       INTEGER NOT NULL,
  model       INTEGER NOT NULL,
  session     INTEGER NOT NULL,
  turn        INTEGER,
  input       INTEGER NOT NULL,
  output      INTEGER NOT NULL,
  cache_read  INTEGER NOT NULL,
  cache_write INTEGER NOT NULL
);
CREATE INDEX fact_at ON fact (at);
CREATE INDEX fact_location ON fact (location, at);
CREATE INDEX fact_source ON fact (source);
CREATE INDEX fact_session ON fact (session);
";
