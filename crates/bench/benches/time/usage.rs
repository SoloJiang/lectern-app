//! `usage/*` (docs/design/usage.md「性能」), step 1: the store driven by
//! `lectern_usage::testing::Scenario` (no plugin, no executor) on the
//! `heavy_days` fixture (30 heavy days ending yesterday, 20 repositories:
//! about 180 000 fact rows).
//!
//! The fixture is written once per target process; a benchmark that writes
//! (ingest, archive) copies the closed database into its scratch directory.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use std::cell::RefCell;

use lectern_bench::{Bench, BenchError, Ctx, Measured, Samples, Value};
use lectern_services::project::ProjectKey;
use lectern_services::session::{
    Agent, Positioned, RawTokens, Record, Timestamp, TurnId, UsageKey, UsageOrigin, UsageRecord,
};
use lectern_services::usage::{LocalDate, ProjectRef, UsageQuery, UsageSnapshot, Window};
use lectern_usage::testing::{
    HEAVY_NOW, Scenario, TURN_BYTES, heavy_days, heavy_open, heavy_roots, synthetic_turns,
};

const DAY: i64 = 24 * 3_600_000;

/// The written fixture, kept for the target process.
struct Heavy {
    dir: tempfile::TempDir,
    days: u32,
}

fn heavy(ctx: &Ctx) -> Result<Rc<Heavy>, BenchError> {
    let days = ctx.scale(30, 2);
    ctx.shared(&format!("usage/heavy_days_{days}"), || {
        let dir = tempfile::tempdir()?;
        let started = Instant::now();
        heavy_days(dir.path(), days)?;
        eprintln!(
            "usage: {days} heavy days written in {:.1?}",
            started.elapsed()
        );
        Ok(Heavy { dir, days })
    })
}

/// A writable copy of the fixture in `to`, opened.
fn copy(heavy: &Heavy, to: &Path) -> Result<Scenario, BenchError> {
    std::fs::create_dir_all(to)?;
    std::fs::copy(heavy.dir.path().join("usage.db"), to.join("usage.db"))?;
    Ok(heavy_open(to)?)
}

fn note(heavy: &Heavy) -> String {
    format!("{} heavy days, 20 repositories", heavy.days)
}

fn first_repo() -> ProjectKey {
    ProjectKey::Git {
        common_dir: heavy_roots()[0].join(".git").into(),
    }
}

/// About 4 KiB appended to a log at `pos`: a turn start and two usage
/// records, 1.5 KiB apart, at `at`.
fn append_4k(prefix: &str, n: usize, pos: u64, at: i64) -> Vec<Positioned<Record>> {
    let tid = format!("{prefix}-a{n}");
    let mut out = vec![Positioned {
        pos,
        seq: 0,
        inherited: false,
        value: Record::TurnStarted {
            turn: TurnId(tid.clone().into()),
            at: Timestamp(at),
        },
    }];
    for k in 0..2u64 {
        out.push(Positioned {
            pos: pos + 1536 * (k + 1),
            seq: 0,
            inherited: false,
            value: Record::Usage(UsageRecord {
                at: Timestamp(at + 1000 * (k as i64 + 1)),
                turn: Some(TurnId(tid.clone().into())),
                model: Some("model-a".into()),
                tokens: RawTokens::Anthropic {
                    input: 10 + k,
                    output: 200,
                    cache_read: 30_000,
                    cache_write: 400,
                    cache_write_5m: 0,
                    cache_write_1h: 0,
                },
                key: UsageKey::Claude {
                    message_id: format!("{prefix}-a{n}-{k}").into(),
                    request_id: Some(format!("req-{prefix}-a{n}-{k}").into()),
                },
                advisor: None,
                superseded: false,
                sidechain: false,
                origin: UsageOrigin::SessionLog,
            }),
        });
    }
    out
}

/// An active session in the first repository, read to its end: the
/// session the appends go to.
fn live_session(s: &mut Scenario) -> Result<(PathBuf, u64), BenchError> {
    let cwd = heavy_roots()[0].join("src/live");
    let records = synthetic_turns(
        &Agent::ClaudeCode,
        "live",
        &cwd,
        HEAVY_NOW - 3_600_000,
        0..2,
        "model-a",
    );
    s.session(&Agent::ClaudeCode, "live", &cwd, records);
    s.read_all(&Agent::ClaudeCode, "live")?;
    Ok((cwd, 2 * TURN_BYTES + 4096))
}

/// `usage/ingest_append_4k`: an active session's 4 KiB append (a turn
/// start, two usage records) read from the saved cursor, normalized,
/// deduplicated, stored and its cursor committed, on the 30-day fixture.
pub const INGEST_APPEND_4K: Bench = Bench {
    id: "usage/ingest_append_4k",
    run: ingest_append_4k,
};

fn ingest_append_4k(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let heavy = heavy(ctx)?;
    let mut s = copy(&heavy, &ctx.scratch().join("db"))?;
    let (_, mut pos) = live_session(&mut s)?;
    let s = RefCell::new(s);
    let mut n = 0;
    let mut wrong: Option<String> = None;
    let time = ctx.time_with_setup(
        || {
            n += 1;
            let records = append_4k("live", n, pos, HEAVY_NOW - 1_800_000 + n as i64);
            s.borrow()
                .sessions(&Agent::ClaudeCode)
                .push("live", records);
            pos += 4096;
        },
        |()| match s.borrow_mut().read(&Agent::ClaudeCode, "live", 8 << 20) {
            Ok(stored) if stored.sources() == 2 && stored.at_end() => {}
            Ok(stored) => {
                wrong.get_or_insert(format!("{} sources", stored.sources()));
            }
            Err(e) => {
                wrong.get_or_insert(e.to_string());
            }
        },
    );
    if let Some(wrong) = wrong {
        return Err(BenchError::new(wrong));
    }
    Ok(vec![
        Measured::new("usage/ingest_append_4k", Value::Time(time)).with_note(Some(note(&heavy))),
    ])
}

/// Turns in one 8 MiB batch.
const BATCH_TURNS: usize = ((8 << 20) / TURN_BYTES) as usize;

/// `usage/backfill_batch`: one 8 MiB batch of a synthetic session (about
/// 390 turns, 1170 usage records) read and stored, on the 30-day fixture.
pub const BACKFILL_BATCH: Bench = Bench {
    id: "usage/backfill_batch",
    run: backfill_batch,
};

fn backfill_batch(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let heavy = heavy(ctx)?;
    let s = copy(&heavy, &ctx.scratch().join("db"))?;
    let turns = ctx.scale(BATCH_TURNS, 40);
    let cwd = heavy_roots()[1].join("src/backfill");
    let mut n = 0;
    let mut wrong: Option<String> = None;
    let time = ctx.time_with_setup(
        || {
            n += 1;
            let id = format!("backfill-{n}");
            let start = HEAVY_NOW - 10 * DAY;
            let records =
                synthetic_turns(&Agent::ClaudeCode, &id, &cwd, start, 0..turns, "model-b");
            s.session(&Agent::ClaudeCode, &id, &cwd, records);
            id
        },
        |id| match s.store_batch(&Agent::ClaudeCode, &id, 8 << 20) {
            Ok(stored) if stored.sources() == turns * 3 => {}
            Ok(stored) => {
                wrong.get_or_insert(format!("{} sources", stored.sources()));
            }
            Err(e) => {
                wrong.get_or_insert(e.to_string());
            }
        },
    );
    if let Some(wrong) = wrong {
        return Err(BenchError::new(wrong));
    }
    Ok(vec![
        Measured::new("usage/backfill_batch", Value::Time(time))
            .with_note(Some(format!("{turns} turns, {} usage records", turns * 3))),
    ])
}

/// `usage/backfill_large_session`: a 200 MB synthetic session (about 9750
/// turns) backfilled in 8 MiB batches; each batch is a sample. Step 1 has
/// no finalizing (it comes with step 4); the bytes the source handed out
/// over all batches must stay within 1.2 times the log.
pub const BACKFILL_LARGE_SESSION: Bench = Bench {
    id: "usage/backfill_large_session",
    run: backfill_large_session,
};

fn backfill_large_session(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let heavy = heavy(ctx)?;
    let mut s = copy(&heavy, &ctx.scratch().join("db"))?;
    let bytes: u64 = ctx.scale(200 << 20, 4 << 20);
    let turns = (bytes / TURN_BYTES) as usize;
    let cwd = heavy_roots()[2].join("src/large");
    let records = synthetic_turns(
        &Agent::ClaudeCode,
        "large",
        &cwd,
        HEAVY_NOW - 20 * DAY,
        0..turns,
        "model-a",
    );
    s.session(&Agent::ClaudeCode, "large", &cwd, records);
    let mut samples = Vec::new();
    let mut handed = 0u64;
    loop {
        let started = Instant::now();
        let stored = s.read(&Agent::ClaudeCode, "large", 8 << 20)?;
        samples.push(started.elapsed());
        handed += stored.bytes();
        if stored.at_end() {
            break;
        }
    }
    let size = turns as u64 * TURN_BYTES;
    if handed * 10 > size * 12 {
        return Err(BenchError::new(format!(
            "{handed} bytes handed out for a {size}-byte log"
        )));
    }
    Ok(vec![
        Measured::new(
            "usage/backfill_large_session",
            Value::Time(Samples(samples)),
        )
        .with_note(Some(format!(
            "{turns} turns, {size} bytes, {handed} bytes read in batches"
        ))),
    ])
}

fn project_30d_query() -> UsageQuery {
    UsageQuery::Project {
        project: ProjectRef::Open(first_repo()),
        window: Window::D30,
    }
}

/// `usage/project_30d`: one project's 30-day snapshot computed from the
/// facts (the snapshot task's work), on the 30-day fixture.
pub const PROJECT_30D: Bench = Bench {
    id: "usage/project_30d",
    run: project_30d,
};

fn project_30d(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let heavy = heavy(ctx)?;
    let s = heavy_open(heavy.dir.path())?;
    let query = project_30d_query();
    check_total(&s, &query)?;
    let time = ctx.time(|| s.snapshot(&query));
    Ok(vec![
        Measured::new("usage/project_30d", Value::Time(time)).with_note(Some(note(&heavy))),
    ])
}

/// `usage/all_projects_30d`: the 「所有项目」 30-day snapshot, same
/// fixture.
pub const ALL_PROJECTS_30D: Bench = Bench {
    id: "usage/all_projects_30d",
    run: all_projects_30d,
};

fn all_projects_30d(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let heavy = heavy(ctx)?;
    let s = heavy_open(heavy.dir.path())?;
    let query = UsageQuery::AllProjects {
        window: Window::D30,
    };
    check_total(&s, &query)?;
    let time = ctx.time(|| s.snapshot(&query));
    Ok(vec![
        Measured::new("usage/all_projects_30d", Value::Time(time)).with_note(Some(note(&heavy))),
    ])
}

fn check_total(s: &Scenario, query: &UsageQuery) -> Result<(), BenchError> {
    let total = match s.snapshot(query)? {
        Some(UsageSnapshot::Project(p)) => p.total,
        Some(UsageSnapshot::All(a)) => a.total,
        other => return Err(BenchError::debug("snapshot", other)),
    };
    if total.total() == 0 {
        return Err(BenchError::new("an empty snapshot"));
    }
    Ok(())
}

/// `usage/snapshot_increment`: with the first project's 30-day snapshot
/// watched, a stored batch's fact deltas applied and the snapshot
/// replaced. The batch's own ingest is not timed here
/// (`usage/ingest_append_4k`); step 1 has no finalizing.
pub const SNAPSHOT_INCREMENT: Bench = Bench {
    id: "usage/snapshot_increment",
    run: snapshot_increment,
};

fn snapshot_increment(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let heavy = heavy(ctx)?;
    let mut s = copy(&heavy, &ctx.scratch().join("db"))?;
    let (_, mut pos) = live_session(&mut s)?;
    let query = project_30d_query();
    s.watch(query.clone())?;
    let s = RefCell::new(s);
    let mut n = 0;
    let mut wrong: Option<String> = None;
    let time = ctx.time_with_setup(
        || {
            n += 1;
            let records = append_4k("live", n, pos, HEAVY_NOW - 1_800_000 + n as i64);
            let s = s.borrow();
            s.sessions(&Agent::ClaudeCode).push("live", records);
            pos += 4096;
            s.store_batch(&Agent::ClaudeCode, "live", 8 << 20)
        },
        |stored| match stored.map(|stored| s.borrow_mut().replace_snapshots(&stored)) {
            Ok(Ok(1)) => {}
            Ok(Ok(n)) => {
                wrong.get_or_insert(format!("{n} snapshots replaced"));
            }
            Ok(Err(e)) | Err(e) => {
                wrong.get_or_insert(e.to_string());
            }
        },
    );
    if let Some(wrong) = wrong {
        return Err(BenchError::new(wrong));
    }
    // The watched snapshot is the one a full compute gives.
    let s = s.into_inner();
    if s.watched(&query).as_deref() != s.snapshot(&query)?.as_ref() {
        return Err(BenchError::new("the incremental snapshot differs"));
    }
    Ok(vec![
        Measured::new("usage/snapshot_increment", Value::Time(time)).with_note(Some(note(&heavy))),
    ])
}

/// `usage/archive_month`: the fixture's months archived once the clock is
/// 400 days on; each segment written (read, encode, compress, decode and
/// compare, insert, delete, commit) is a sample. The compression ratio is
/// in the note.
pub const ARCHIVE_MONTH: Bench = Bench {
    id: "usage/archive_month",
    run: archive_month,
};

fn archive_month(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let heavy = heavy(ctx)?;
    let mut s = copy(&heavy, &ctx.scratch().join("db"))?;
    let hot = s.db_bytes();
    s.set_now(HEAVY_NOW + 400 * DAY);
    let mut samples = Vec::new();
    let (mut rows, mut raw, mut packed) = (0, 0, 0);
    for month in s.archive_plan()? {
        loop {
            let started = Instant::now();
            let step = s.archive_step(month)?;
            let took = started.elapsed();
            if step.table.is_some() {
                samples.push(took);
                rows += step.rows;
                raw += step.raw_bytes;
                packed += step.packed_bytes;
            }
            if step.finished {
                break;
            }
        }
    }
    if samples.is_empty() {
        return Err(BenchError::new("nothing archived"));
    }
    let ratio = raw as f64 / packed.max(1) as f64;
    Ok(vec![
        Measured::new("usage/archive_month", Value::Time(Samples(samples.clone()))).with_note(
            Some(format!(
                "{} segments, {rows} rows, {raw} bytes encoded, {packed} compressed \
                 (ratio {ratio:.1}:1), hot database {hot} bytes",
                samples.len()
            )),
        ),
    ])
}

/// `usage/cold_read_month`: the history's days of an archived heavy
/// month (October 2026, 21 heavy days in the fixture): its `fact` segments
/// decoded one by one and summed per local day.
pub const COLD_READ_MONTH: Bench = Bench {
    id: "usage/cold_read_month",
    run: cold_read_month,
};

fn cold_read_month(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let heavy = heavy(ctx)?;
    let mut s = copy(&heavy, &ctx.scratch().join("db"))?;
    s.set_now(HEAVY_NOW + 400 * DAY);
    s.archive_all()?;
    let (from, to) = (
        LocalDate {
            year: 2026,
            month: 10,
            day: 1,
        },
        LocalDate {
            year: 2026,
            month: 10,
            day: 31,
        },
    );
    let repo = first_repo();
    let before = s.cold_reads();
    let first = s.history(&repo, from, to, true)?;
    let segments = s.cold_reads() - before;
    let total: u64 = first
        .days
        .iter()
        .flat_map(|d| d.iter())
        .map(|(_, t)| t.total())
        .sum();
    if segments == 0 || total == 0 {
        return Err(BenchError::new(format!(
            "{segments} segments, {total} tokens"
        )));
    }
    let time = ctx.time(|| s.history(&repo, from, to, true));
    Ok(vec![
        Measured::new("usage/cold_read_month", Value::Time(time))
            .with_note(Some(format!("{segments} fact segments decoded"))),
    ])
}

/// `usage/disk_day_heavy`: how much one heavy day grows the database file
/// and its `-wal`, from an empty database. Record only; 396 times it is the
/// hot data's ceiling (「磁盘」).
pub const DISK_DAY_HEAVY: Bench = Bench {
    id: "usage/disk_day_heavy",
    run: disk_day_heavy,
};

fn disk_day_heavy(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let dir = ctx.scratch().join("db");
    let mut s = heavy_open(&dir)?;
    let before = s.db_bytes();
    lectern_usage::testing::heavy_fill(&mut s, 1)?;
    let after = s.db_bytes();
    let wal = s.wal_bytes();
    let day = after.saturating_sub(before);
    Ok(vec![
        Measured::new("usage/disk_day_heavy", Value::Bytes(vec![day])).with_note(Some(format!(
            "one heavy day, {wal} bytes of it in -wal; times 396: {} MiB",
            (day * 396) >> 20
        ))),
    ])
}
