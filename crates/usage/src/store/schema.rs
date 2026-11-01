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
    version: 6,
    create: CREATE_V6,
    migrations: &[
        Migration {
            from: 1,
            sql: COLD_V2,
        },
        Migration {
            from: 2,
            sql: LIMITS_V3,
        },
        Migration {
            from: 3,
            sql: IDENTITY_V4,
        },
        Migration {
            from: 4,
            sql: CALLS_V5,
        },
        Migration {
            from: 5,
            sql: BRANCHES_V6,
        },
    ],
};

/// Version 5 alone, for the migration tests.
#[cfg(test)]
pub(crate) const V5: Schema = Schema {
    version: 5,
    create: CREATE_V5,
    migrations: &[
        Migration {
            from: 1,
            sql: COLD_V2,
        },
        Migration {
            from: 2,
            sql: LIMITS_V3,
        },
        Migration {
            from: 3,
            sql: IDENTITY_V4,
        },
        Migration {
            from: 4,
            sql: CALLS_V5,
        },
    ],
};

/// Version 4 alone, for the migration tests.
#[cfg(test)]
pub(crate) const V4: Schema = Schema {
    version: 4,
    create: CREATE_V4,
    migrations: &[
        Migration {
            from: 1,
            sql: COLD_V2,
        },
        Migration {
            from: 2,
            sql: LIMITS_V3,
        },
        Migration {
            from: 3,
            sql: IDENTITY_V4,
        },
    ],
};

/// Version 1 alone, for the migration tests.
#[cfg(test)]
pub(crate) const V1: Schema = Schema {
    version: 1,
    create: CREATE_V1,
    migrations: &[],
};

/// Version 2 alone, for the migration tests.
#[cfg(test)]
pub(crate) const V2: Schema = Schema {
    version: 2,
    create: CREATE_V2,
    migrations: &[Migration {
        from: 1,
        sql: COLD_V2,
    }],
};

/// Version 3 alone, for the migration tests.
#[cfg(test)]
pub(crate) const V3: Schema = Schema {
    version: 3,
    create: CREATE_V3,
    migrations: &[
        Migration {
            from: 1,
            sql: COLD_V2,
        },
        Migration {
            from: 2,
            sql: LIMITS_V3,
        },
    ],
};

/// Location kinds.
pub(crate) const LOCATION_GIT: i64 = 0;
pub(crate) const LOCATION_DIR: i64 = 1;

/// Project kinds.
pub(crate) const PROJECT_GIT: i64 = 0;
pub(crate) const PROJECT_FOLDER: i64 = 1;

/// Observation tables in the order a salvage copies them, then the derived
/// ones it rebuilds. Every one is a rowid table. The cold tables are
/// observations too: nothing else holds what they hold.
pub(crate) const OBSERVATION_TABLES: &[&str] = &[
    "intern",
    "location",
    "project",
    "session",
    "session_gen",
    "turn",
    "usage_source",
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
    "run",
    "watch_span",
    "head_baseline",
    "reflog",
    "reflog_reset",
    "segment",
    "segment_name",
    "turn_obs",
    "call_obs",
];

/// The hot / cold split (docs/design/usage.md「冷热分层」), table version 2.
///
/// - `cold_part`: one segment, a table's rows of one UTC month (`month` is
///   `year * 12 + month - 1`), encoded by columns and compressed.
/// - `cold_month`: the archived facts per location, local month (`ym`, the
///   same numbering, in the zone of the archive), agent, model and review,
///   in millionths.
/// - `cold_total`: the archived facts per session (`kind` 0) and segment
///   (`kind` 1).
/// - `cold_anchor`: what hot turns of a session follow once the turns they
///   follow are cold (changed turns and their shares, step 4 on).
/// - `cold_seen`: the sessions with archived sources per month, so that a
///   replay does not write those months into the hot tables again.
///
/// Indexes on the sources' and turns' timestamps let the archive find a
/// month's rows.
///
/// `fact_bucket` (derived, with the table version 2): the facts summed per
/// 15-minute bucket, location, agent, model and review, with their row
/// counts, kept in step with `fact` in the same transactions; the window
/// queries read it instead of grouping every fact of the window. Archived
/// months' buckets are dropped when the month is archived.
macro_rules! cold_sql {
    () => {
        "
CREATE TABLE cold_part (
  id         INTEGER PRIMARY KEY,
  month      INTEGER NOT NULL,
  tbl        TEXT NOT NULL,
  seq        INTEGER NOT NULL,
  rows       INTEGER NOT NULL,
  raw_bytes  INTEGER NOT NULL,
  data       BLOB NOT NULL,
  written_at INTEGER NOT NULL,
  UNIQUE (month, tbl, seq)
);

CREATE TABLE cold_month (
  id          INTEGER PRIMARY KEY,
  location    INTEGER NOT NULL,
  ym          INTEGER NOT NULL,
  agent       INTEGER NOT NULL,
  model       INTEGER NOT NULL,
  review      INTEGER NOT NULL,
  input       INTEGER NOT NULL,
  output      INTEGER NOT NULL,
  cache_read  INTEGER NOT NULL,
  cache_write INTEGER NOT NULL,
  UNIQUE (location, ym, agent, model, review)
);

CREATE TABLE cold_total (
  id          INTEGER PRIMARY KEY,
  kind        INTEGER NOT NULL,
  target      INTEGER NOT NULL,
  review      INTEGER NOT NULL,
  input       INTEGER NOT NULL,
  output      INTEGER NOT NULL,
  cache_read  INTEGER NOT NULL,
  cache_write INTEGER NOT NULL,
  UNIQUE (kind, target, review)
);

CREATE TABLE cold_anchor (
  id      INTEGER PRIMARY KEY,
  session INTEGER NOT NULL,
  kind    INTEGER NOT NULL,
  turn    INTEGER NOT NULL,
  shares  BLOB NOT NULL,
  UNIQUE (session, kind)
);

CREATE TABLE cold_seen (
  id      INTEGER PRIMARY KEY,
  session INTEGER NOT NULL,
  month   INTEGER NOT NULL,
  UNIQUE (session, month)
);

CREATE INDEX usage_source_at ON usage_source (at);
CREATE INDEX turn_started ON turn (started_at);

CREATE TABLE fact_bucket (
  bucket      INTEGER NOT NULL,
  location    INTEGER NOT NULL,
  agent       INTEGER NOT NULL,
  model       INTEGER NOT NULL,
  review      INTEGER NOT NULL,
  input       INTEGER NOT NULL,
  output      INTEGER NOT NULL,
  cache_read  INTEGER NOT NULL,
  cache_write INTEGER NOT NULL,
  rows        INTEGER NOT NULL,
  PRIMARY KEY (bucket, location, agent, model, review)
) WITHOUT ROWID;

INSERT INTO fact_bucket
  SELECT bucket, location, agent, model, review, sum(input), sum(output), sum(cache_read),
         sum(cache_write), count(*)
  FROM fact GROUP BY bucket, location, agent, model, review;
"
    };
}

/// Strings and paths are interned (`intern`) and referenced by ID. Sources
/// and turns are `AUTOINCREMENT`: a replay deletes a generation's rows, and
/// a reused ID would let the old row's facts be deleted under the new
/// one's. Columns that later steps fill (`fact.segment`, `fact.kind`,
/// `fact.review`) are here from the start.
macro_rules! v1_sql {
    () => {
        "
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
  repaired    INTEGER NOT NULL DEFAULT 0,
  UNIQUE (session, gen, pos, seq, part)
);
CREATE INDEX usage_source_key ON usage_source (key);
CREATE INDEX usage_source_repaired ON usage_source (id) WHERE repaired = 1;

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
"
    };
}

/// The limit samples (docs/design/usage.md「额度快照」), table version 3: the
/// newest sample of each (agent, source, limit ID, kind), nothing else. A
/// small entity table that stays hot: it is not archived. `agent` is the
/// agent label and `limit_id` the text itself ("" for none), not `intern`
/// IDs, so that a salvage never leaves a sample pointing at a lost string.
macro_rules! limits_sql {
    () => {
        "
CREATE TABLE limit_sample (
  id             INTEGER PRIMARY KEY,
  agent          TEXT NOT NULL,
  source         INTEGER NOT NULL,
  limit_id       TEXT NOT NULL,
  kind           INTEGER NOT NULL,
  used_percent   REAL NOT NULL,
  window_minutes INTEGER,
  resets_at      INTEGER,
  sampled_at     INTEGER NOT NULL,
  UNIQUE (agent, source, limit_id, kind)
);
"
    };
}

/// Project identity (docs/design/usage.md「项目标识」), table version 4.
///
/// - `project` is rebuilt: a location may have several projects over time
///   (a path taken over by an unrelated repository, a repository whose
///   history was rewritten, a temporary project beside the one it may
///   belong to), so `location` is no longer unique. `since` (Unix
///   milliseconds, 0 for the project made when the location first
///   appeared) is when the project starts to own the location's usage: a
///   fact belongs to the project with the largest `since` not after it.
///   `reason` and `candidate` are set for a temporary project
///   (`TemporaryReason` code, `project.id` it may be the same as). The
///   rows are copied with their IDs, which `fact.project` holds.
/// - `fact_bucket` (derived) gets `project` in its key: two projects may
///   have facts at one location, and a window is summed by who owns the
///   facts, so the sums keep them apart. 0 is a fact in no project (a
///   directory under a folder project's root); it is rebuilt from `fact`.
/// - `project_fact`: what a repository showed each time it was identified
///   (root commits as hex joined by spaces, the normalized remote
///   addresses joined by newlines, the remote chosen as primary and its
///   address, the code platform's repository ID, whether it had no commit
///   yet, and the default branch's tip the root commits were found from:
///   the next identification walks the history again only when the tip
///   moved).
/// - `alias`: appended records, never changed. `kind` is an
///   `store::identity::AliasKind` code; `src` is the project it leaves, `dst` the
///   one it joins (none for a confirmation or a promotion), `undoes` the
///   record an undo cancels.
macro_rules! identity_sql {
    () => {
        "
CREATE TABLE project_next (
  id        INTEGER PRIMARY KEY,
  pid       BLOB NOT NULL UNIQUE,
  kind      INTEGER NOT NULL,
  location  INTEGER NOT NULL,
  opened    INTEGER NOT NULL DEFAULT 0,
  name      TEXT,
  since     INTEGER NOT NULL DEFAULT 0,
  reason    INTEGER,
  candidate INTEGER
);
INSERT INTO project_next (id, pid, kind, location, opened, name)
  SELECT id, pid, kind, location, opened, name FROM project;
DROP TABLE project;
ALTER TABLE project_next RENAME TO project;
CREATE INDEX project_location ON project (location, since);

DROP TABLE fact_bucket;
CREATE TABLE fact_bucket (
  bucket      INTEGER NOT NULL,
  location    INTEGER NOT NULL,
  project     INTEGER NOT NULL,
  agent       INTEGER NOT NULL,
  model       INTEGER NOT NULL,
  review      INTEGER NOT NULL,
  input       INTEGER NOT NULL,
  output      INTEGER NOT NULL,
  cache_read  INTEGER NOT NULL,
  cache_write INTEGER NOT NULL,
  rows        INTEGER NOT NULL,
  PRIMARY KEY (bucket, location, project, agent, model, review)
) WITHOUT ROWID;
INSERT INTO fact_bucket
  SELECT bucket, location, coalesce(project, 0), agent, model, review, sum(input), sum(output),
         sum(cache_read), sum(cache_write), count(*)
  FROM fact GROUP BY bucket, location, coalesce(project, 0), agent, model, review;

CREATE TABLE project_fact (
  id           INTEGER PRIMARY KEY,
  project      INTEGER NOT NULL,
  location     INTEGER NOT NULL,
  at           INTEGER NOT NULL,
  roots        TEXT NOT NULL,
  remotes      TEXT NOT NULL,
  primary_name TEXT,
  primary_addr TEXT,
  forge_host   TEXT,
  forge_id     TEXT,
  unborn       INTEGER NOT NULL,
  tip          TEXT
);
CREATE INDEX project_fact_project ON project_fact (project, id);
CREATE INDEX project_fact_location ON project_fact (location, id);

CREATE TABLE alias (
  id      INTEGER PRIMARY KEY,
  kind    INTEGER NOT NULL,
  src     INTEGER NOT NULL,
  dst     INTEGER,
  undoes  INTEGER,
  at      INTEGER NOT NULL
);
CREATE INDEX alias_src ON alias (src);
"
    };
}

/// Calls, changed files and shares (docs/design/usage.md「轮次的观察与改动项」
/// 「分摊」), table version 5.
///
/// - `turn` gets `last_at` (the time of the last record that belongs to the
///   turn: the design's L, or the later result of one of its calls),
///   `closed` (its end marker was read), `done` (settled: its `item` and
///   `share` rows are written) and `changed` (its share comes from changes,
///   its own or its sub-agents', not from following another turn).
/// - A call is named by `cid`, a 64-bit hash of its call ID
///   (`store::calls::hash`), not by an `intern` row: the strings of `intern`
///   are never removed, and a heavy day has 20 000 calls. `session.parent_call`
///   is the `cid` of the call that spawned the session.
/// - `turn.edits` says the turn has `Edit` calls: its changed files are read
///   from the session's turn index when it is settled.
/// - `call`: a row per call that spawns a sub-agent, of a generation
///   (`kind` and `status` are the codes of `store::calls`); the other calls
///   have no row until a step needs one (the `Edit` calls are in `turn.edits`
///   and `call_fact`).
/// - `call_fact`: what the world looked like when an `Edit` call was read:
///   the place and worktree of each file it names (the path below the
///   worktree is the path less the worktree's). Written once, not per
///   generation, so that a replay keeps them after the worktree is gone.
/// - `item`: a changed file of a turn, with its changed lines. `at` is the
///   turn's start (the archive finds the rows by it).
/// - `share` (derived): the settled turn's targets, `weight` out of `total`.
///   `at` is the turn's start.
///
/// The migration reads every session again (cursors back to the start):
/// version 4 stored no calls. A replay in progress goes on from the start
/// of the log into the generation it was filling.
macro_rules! calls_sql {
    () => {
        "
ALTER TABLE turn ADD COLUMN last_at INTEGER;
ALTER TABLE turn ADD COLUMN closed INTEGER NOT NULL DEFAULT 0;
ALTER TABLE turn ADD COLUMN done INTEGER NOT NULL DEFAULT 0;
ALTER TABLE turn ADD COLUMN changed INTEGER NOT NULL DEFAULT 0;
ALTER TABLE turn ADD COLUMN edits INTEGER NOT NULL DEFAULT 0;
CREATE INDEX turn_pending ON turn (session, gen, seq) WHERE done = 0;
CREATE INDEX turn_changed ON turn (session, gen, seq) WHERE done = 1 AND changed = 1;
CREATE INDEX turn_seq ON turn (session, gen, seq);
ALTER TABLE session ADD COLUMN parent_call INTEGER;
CREATE INDEX session_parent ON session (parent_source, parent_sid);
CREATE INDEX usage_source_turn ON usage_source (turn);

CREATE TABLE call (
  id          INTEGER PRIMARY KEY,
  session     INTEGER NOT NULL,
  gen         INTEGER NOT NULL,
  cid         INTEGER NOT NULL,
  turn        INTEGER,
  started_at  INTEGER NOT NULL,
  finished_at INTEGER,
  kind        INTEGER NOT NULL,
  status      INTEGER,
  UNIQUE (session, gen, cid)
);
CREATE INDEX call_started ON call (started_at);

CREATE TABLE call_fact (
  id       INTEGER PRIMARY KEY,
  session  INTEGER NOT NULL,
  cid      INTEGER NOT NULL,
  path     INTEGER NOT NULL,
  location INTEGER NOT NULL,
  worktree INTEGER,
  at       INTEGER NOT NULL,
  UNIQUE (session, cid, path)
);
CREATE INDEX call_fact_at ON call_fact (at);

CREATE TABLE item (
  id       INTEGER PRIMARY KEY,
  session  INTEGER NOT NULL,
  gen      INTEGER NOT NULL,
  turn     INTEGER NOT NULL,
  path     INTEGER NOT NULL,
  location INTEGER NOT NULL,
  lines    INTEGER NOT NULL,
  at       INTEGER NOT NULL
);
CREATE INDEX item_turn ON item (turn);
CREATE INDEX item_at ON item (at);

CREATE TABLE share (
  id       INTEGER PRIMARY KEY,
  turn     INTEGER NOT NULL,
  location INTEGER NOT NULL,
  kind     INTEGER NOT NULL DEFAULT 0,
  weight   INTEGER NOT NULL,
  total    INTEGER NOT NULL,
  at       INTEGER NOT NULL,
  UNIQUE (turn, location, kind)
);
CREATE INDEX share_at ON share (at);

UPDATE session SET cursor = X'01', reached_end = 0, end_stamp = NULL;
"
    };
}

/// Branch observation and attribution (docs/design/usage.md「分支观察」「分摊」),
/// table version 6.
///
/// - `run`: the fs_watch `RunId` of each run of the watcher, in the order
///   they were first seen (`id`); records of different runs are ordered by it.
/// - `watch_span`: the time (nanoseconds, wall clock) a repository was
///   watched from; `end_ns` is NULL while the watch lasts, `last_ns` the
///   time of the last record read in it (a start finds the spans of a run
///   that did not end cleanly and ends them there).
/// - `head_baseline`: the branch (the full reference name,
///   NULL when detached) a worktree's HEAD was on when the watch began. `wt`
///   is the worktree key (the path of its administrative directory) as an
///   `intern` ID.
/// - `reflog`: the records `fs_watch` delivered, parsed: `file` 0 is a
///   worktree's HEAD (`wt` its key), 1 a branch (`wt` the interned reference
///   name), 2 the stash. `at` is the line's own time in milliseconds,
///   `obs_ns` / `aft_ns` the end of the read that found it and the start of
///   the one before (nanoseconds). `act` classifies the line
///   (`store::repo`), `a` / `b` are the names of a switch or rename.
///   The raw line is not kept: the rows hold what the replay reads.
/// - `reflog_reset`: a file got shorter, was removed or could not be read.
/// - `segment`, `segment_name`: a branch's (or a detached HEAD's) stretch of
///   existence, with its names over time; `is_default` is set when an
///   observation found the branch to be the default branch.
/// - `turn_obs`: what was observed about the worktree a turn ran in and the
///   worktrees it went into (`enter`: pairs of location and segment); fixed
///   when the turn is settled, not per generation.
/// - `call_obs`: where HEAD stayed during a command that mentions git, per
///   worktree it touched.
/// - `call` gets the commands that mention git (`git`: a bit mask of what
///   they do, `facts` the targets, never the text).
/// - `call_fact` gets the state of the changed file when its call was
///   observed (`segment`, `state`); `item.cf` is its `call_fact` row.
/// - `share` is rebuilt: its target is a place and a segment (0 for none) or
///   a kind.
/// - `turn` and `session_gen` get the worktree the session ran in and the
///   branch its log named.
///
/// The migration settles every turn again and reads every session again (the
/// turns need their worktree and branch).
macro_rules! branches_sql {
    () => {
        "
ALTER TABLE turn ADD COLUMN worktree INTEGER;
ALTER TABLE turn ADD COLUMN branch INTEGER;
ALTER TABLE session_gen ADD COLUMN worktree INTEGER;
ALTER TABLE session_gen ADD COLUMN branch INTEGER;
ALTER TABLE call ADD COLUMN wt INTEGER;
ALTER TABLE call ADD COLUMN git INTEGER NOT NULL DEFAULT 0;
ALTER TABLE call ADD COLUMN facts BLOB;
ALTER TABLE call_fact ADD COLUMN segment INTEGER;
ALTER TABLE call_fact ADD COLUMN state INTEGER NOT NULL DEFAULT 0;
ALTER TABLE item ADD COLUMN cf INTEGER;
CREATE INDEX fact_segment ON fact (segment) WHERE segment IS NOT NULL;

DROP TABLE share;
CREATE TABLE share (
  id       INTEGER PRIMARY KEY,
  turn     INTEGER NOT NULL,
  location INTEGER NOT NULL,
  segment  INTEGER NOT NULL DEFAULT 0,
  kind     INTEGER NOT NULL DEFAULT 0,
  weight   INTEGER NOT NULL,
  total    INTEGER NOT NULL,
  at       INTEGER NOT NULL,
  UNIQUE (turn, location, segment, kind)
);
CREATE INDEX share_at ON share (at);

CREATE TABLE run (
  id  INTEGER PRIMARY KEY,
  run INTEGER NOT NULL UNIQUE
);

CREATE TABLE watch_span (
  id       INTEGER PRIMARY KEY,
  repo     INTEGER NOT NULL,
  begin_ns INTEGER NOT NULL,
  last_ns  INTEGER NOT NULL,
  end_ns   INTEGER,
  at       INTEGER NOT NULL
);
CREATE INDEX watch_span_repo ON watch_span (repo, begin_ns);
CREATE INDEX watch_span_open ON watch_span (id) WHERE end_ns IS NULL;
CREATE INDEX watch_span_at ON watch_span (at);

CREATE TABLE head_baseline (
  id     INTEGER PRIMARY KEY,
  repo   INTEGER NOT NULL,
  wt     INTEGER NOT NULL,
  at_ns  INTEGER NOT NULL,
  branch TEXT,
  at     INTEGER NOT NULL
);
CREATE INDEX head_baseline_wt ON head_baseline (repo, wt, at_ns);
CREATE INDEX head_baseline_at ON head_baseline (at);

CREATE TABLE reflog (
  id     INTEGER PRIMARY KEY,
  repo   INTEGER NOT NULL,
  file   INTEGER NOT NULL,
  wt     INTEGER NOT NULL,
  run    INTEGER NOT NULL,
  seq    INTEGER NOT NULL,
  off    INTEGER NOT NULL,
  at     INTEGER NOT NULL,
  obs_ns INTEGER NOT NULL,
  aft_ns INTEGER,
  origin INTEGER NOT NULL,
  old    BLOB,
  new    BLOB,
  act    INTEGER NOT NULL,
  a      TEXT,
  b      TEXT
);
CREATE INDEX reflog_file ON reflog (repo, file, wt, obs_ns);
CREATE INDEX reflog_at ON reflog (at);

CREATE TABLE reflog_reset (
  id     INTEGER PRIMARY KEY,
  repo   INTEGER NOT NULL,
  file   INTEGER NOT NULL,
  wt     INTEGER NOT NULL,
  run    INTEGER NOT NULL,
  seq    INTEGER NOT NULL,
  reason INTEGER NOT NULL,
  obs_ns INTEGER NOT NULL,
  at     INTEGER NOT NULL
);
CREATE INDEX reflog_reset_file ON reflog_reset (repo, file, wt, obs_ns);
CREATE INDEX reflog_reset_at ON reflog_reset (at);

CREATE TABLE segment (
  id           INTEGER PRIMARY KEY,
  sid          BLOB NOT NULL UNIQUE,
  repo         INTEGER NOT NULL,
  kind         INTEGER NOT NULL,
  prev         INTEGER,
  wt           INTEGER,
  begin_reason INTEGER NOT NULL,
  begin_at     INTEGER,
  end_reason   INTEGER,
  end_at       INTEGER,
  is_default   INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX segment_repo ON segment (repo, kind, id);

CREATE TABLE segment_name (
  id      INTEGER PRIMARY KEY,
  segment INTEGER NOT NULL,
  name    TEXT NOT NULL,
  at      INTEGER
);
CREATE INDEX segment_name_name ON segment_name (name, segment);
CREATE INDEX segment_name_segment ON segment_name (segment);

CREATE TABLE turn_obs (
  id        INTEGER PRIMARY KEY,
  session   INTEGER NOT NULL,
  tid       INTEGER NOT NULL,
  at        INTEGER NOT NULL,
  flags     INTEGER NOT NULL,
  start_seg INTEGER,
  end_seg   INTEGER,
  stays     BLOB,
  enter     BLOB,
  UNIQUE (session, tid)
);
CREATE INDEX turn_obs_at ON turn_obs (at);

CREATE TABLE call_obs (
  id      INTEGER PRIMARY KEY,
  session INTEGER NOT NULL,
  cid     INTEGER NOT NULL,
  wt      INTEGER NOT NULL,
  at      INTEGER NOT NULL,
  flags   INTEGER NOT NULL,
  end_seg INTEGER,
  stays   BLOB,
  UNIQUE (session, cid, wt)
);
CREATE INDEX call_obs_at ON call_obs (at);

UPDATE turn SET done = 0, changed = 0;
DELETE FROM item;
UPDATE session SET cursor = X'01', reached_end = 0, end_stamp = NULL;
"
    };
}

/// Version 1 (step 1 of docs/design/usage.md「验收」, first part).
#[cfg(test)]
pub(crate) const CREATE_V1: &str = v1_sql!();

/// Version 2: version 1 and the cold tables.
#[cfg(test)]
pub(crate) const CREATE_V2: &str = concat!(v1_sql!(), cold_sql!());

/// Version 3: version 2 and the limit samples.
#[cfg(test)]
pub(crate) const CREATE_V3: &str = concat!(v1_sql!(), cold_sql!(), limits_sql!());

/// Version 4: version 3 and the project identity tables (the `project`
/// table of version 1 is rebuilt by the same statements that migrate it).
#[cfg(test)]
pub(crate) const CREATE_V4: &str = concat!(v1_sql!(), cold_sql!(), limits_sql!(), identity_sql!());

/// Version 5: version 4 and the calls, items and shares.
#[cfg(test)]
pub(crate) const CREATE_V5: &str = concat!(
    v1_sql!(),
    cold_sql!(),
    limits_sql!(),
    identity_sql!(),
    calls_sql!()
);

/// Version 6: version 5 and the branch observations.
pub(crate) const CREATE_V6: &str = concat!(
    v1_sql!(),
    cold_sql!(),
    limits_sql!(),
    identity_sql!(),
    calls_sql!(),
    branches_sql!()
);

/// The step from version 4 to 5.
pub(crate) const CALLS_V5: &str = calls_sql!();

/// The step from version 5 to 6.
pub(crate) const BRANCHES_V6: &str = branches_sql!();

/// The step from version 1 to 2.
pub(crate) const COLD_V2: &str = cold_sql!();

/// The step from version 2 to 3.
pub(crate) const LIMITS_V3: &str = limits_sql!();

/// The step from version 3 to 4.
pub(crate) const IDENTITY_V4: &str = identity_sql!();
