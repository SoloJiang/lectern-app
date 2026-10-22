//! `review/*` (docs/design/review.md「性能」). Step 1: the annotation log's
//! whole-file read, with the fixture of `lectern_review::testing`. The
//! system file cache is warm. Step 2: one file's change chain. Step 3: the
//! review engine driven by `testing::Driven` (no plugin): opening a
//! session's review on the fixture of「每天的仓库规模」(2000 commits, 300
//! reflog records, 50 sessions), cold and warm, until it settles; the
//! cutoff table from scratch; the first file's rows; a seen mark on 5000
//! files; the other files' attribution.

use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use lectern_bench::{Bench, BenchError, Ctx, Measured, Mode, Samples, Value};
use lectern_review::testing::{
    ChainRun, CutoffRun, Driven, LogLoad, MarkSeenFixture, SessionFixture, chain_fixture, load_log,
    write_log,
};
use lectern_services::review::{
    AttributionState, LoadState, Need, ReviewCommand, ReviewEvent, ReviewId, RowsKey,
};

/// Reading a 16 000-record log (about 5.3 MiB) whole and building its
/// index, as opening a project does in the background.
pub const LOG_LOAD_16K: Bench = Bench {
    id: "review/log_load_16k",
    run: log_load_16k,
};

/// One file's change chain: 3000 lines, 40 `Replace` steps in 8 turns,
/// each step's `line_diff` and version OID included, the edits' `load`
/// not (they are in memory). Record only.
pub const CHAIN_3000_LINES: Bench = Bench {
    id: "review/chain_3000_lines",
    run: chain_3000_lines,
};

/// From `open` to the first `Ready` snapshot with every `review.*`
/// namespace empty (a new cache directory, its database files already
/// open).
pub const OPEN_SESSION_50: Bench = Bench {
    id: "review/open_session_50",
    run: open_session_50,
};

/// The same with `review.attribution`, `review.tree_changes` and
/// `review.claims` warm (a new engine on the same cache).
pub const OPEN_SESSION_50_WARM: Bench = Bench {
    id: "review/open_session_50_warm",
    run: open_session_50_warm,
};

/// Cold, from `open` to the last snapshot of the background. Record only.
pub const OPEN_SESSION_SETTLED: Bench = Bench {
    id: "review/open_session_settled",
    run: open_session_settled,
};

/// The cutoff table of the 50 files for the 300 records, every
/// `tree_changes` a miss. Record only.
pub const CUTOFFS_COLD: Bench = Bench {
    id: "review/cutoffs_cold",
    run: cutoffs_cold,
};

/// `Prepare` of the first file's final rows to `Prepared`.
pub const ROWS_FIRST: Bench = Bench {
    id: "review/rows_first",
    run: rows_first,
};

/// `command(MarkSeen)` to the replaced snapshot on a 5000-file review.
pub const MARK_SEEN_5000: Bench = Bench {
    id: "review/mark_seen_5000",
    run: mark_seen_5000,
};

/// The chains of the other 49 files built and attributed, every step's
/// edits loaded (the cache's reads and writes not included). Record only.
pub const ATTRIBUTE_SESSION_50: Bench = Bench {
    id: "review/attribute_session_50",
    run: attribute_session_50,
};

fn log_load_16k(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let dir = ctx.scratch().join("review");
    let repo = Path::new("/work/lectern/.git");
    let records = ctx.scale(16_000, 1_000);
    let bytes = write_log(&dir, repo, records)?;
    let mut wrong: Option<LogLoad> = None;
    let time = ctx.time(|| {
        let load = load_log(&dir, repo);
        if load.records != records || load.bad_lines != 0 || !load.complete {
            wrong.get_or_insert(load);
        }
        load
    });
    if let Some(load) = wrong {
        return Err(BenchError::debug("load_log", load));
    }
    Ok(vec![
        Measured::new("review/log_load_16k", Value::Time(time))
            .with_note(Some(format!("{records} records, {bytes} bytes"))),
    ])
}

fn chain_3000_lines(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (lines, replaces) = (3000, 40);
    let fixture = chain_fixture(lines, replaces);
    let expected = ChainRun {
        applied: replaces,
        aligned: true,
        groups: replaces / 5,
    };
    let mut wrong: Option<ChainRun> = None;
    let time = ctx.time(|| {
        let run = fixture.run();
        if run != expected {
            wrong.get_or_insert(run);
        }
        run
    });
    if let Some(run) = wrong {
        return Err(BenchError::debug("chain", run));
    }
    Ok(vec![
        Measured::new("review/chain_3000_lines", Value::Time(time))
            .with_note(Some(format!("{lines} lines, {replaces} Replace steps"))),
    ])
}

/// The fixture of「每天的仓库规模」, shared by the session benches (they
/// only read it; each has its own cache directories).
fn fixture(ctx: &Ctx) -> Result<Rc<SessionFixture>, BenchError> {
    let (commits, records, sessions) = ctx.scale((2000, 300, 50), (300, 60, 10));
    ctx.shared(
        &format!("review/session_{commits}_{records}_{sessions}"),
        || Ok(SessionFixture::new(commits, records, sessions, 50)),
    )
}

fn note(ctx: &Ctx) -> String {
    let (commits, records, sessions) = ctx.scale((2000, 300, 50), (300, 60, 10));
    format!(
        "{commits} commits, {records} reflog records, {sessions} sessions, 50 files of 300 lines"
    )
}

/// One open on `driven`: the times from `open` to the first `Ready`
/// snapshot and to the last snapshot, and the review.
fn open_once(
    driven: &Driven,
    f: &SessionFixture,
) -> Result<(Duration, Duration, ReviewId), BenchError> {
    driven.touch_cache(f.repo.common_dir());
    driven.take_events();
    let started = Instant::now();
    let id = driven.open(f.target());
    driven.run();
    let events = driven.take_events();
    let changed: Vec<Instant> = events
        .iter()
        .filter(|(e, _)| matches!(e, ReviewEvent::Changed { .. }))
        .map(|(_, at)| *at)
        .collect();
    let (Some(first), Some(last)) = (changed.first(), changed.last()) else {
        return Err(BenchError::new("no snapshot"));
    };
    let snapshot = driven
        .snapshot(id)
        .ok_or_else(|| BenchError::new("closed"))?;
    if snapshot.state != LoadState::Ready || snapshot.files.len() != 50 {
        return Err(BenchError::debug(
            "snapshot",
            (&snapshot.state, snapshot.files.len()),
        ));
    }
    let file = &snapshot.files[0].attribution;
    if file.state != AttributionState::Lines || file.drifted_hunks != 0 {
        return Err(BenchError::debug("first file", file));
    }
    Ok((
        first.saturating_duration_since(started),
        last.saturating_duration_since(started),
        id,
    ))
}

/// Samples `one` the way `Ctx::time_with_setup` does, by hand: each sample
/// is a duration the closure measured itself (part of what it did).
fn sample(
    ctx: &Ctx,
    mut one: impl FnMut(usize) -> Result<Duration, BenchError>,
) -> Result<Samples, BenchError> {
    if ctx.mode() == Mode::Smoke {
        return Ok(Samples(vec![one(0)?]));
    }
    one(0)?;
    one(1)?;
    let started = Instant::now();
    let mut samples = Vec::new();
    let mut n = 2;
    while samples.len() < 5 || (samples.len() < 20 && started.elapsed() < Duration::from_secs(10)) {
        samples.push(one(n)?);
        n += 1;
    }
    Ok(Samples(samples))
}

fn open_session_50(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let f = fixture(ctx)?;
    let scratch = ctx.scratch().to_path_buf();
    let time = sample(ctx, |n| {
        let driven = f.driven(&scratch.join(format!("cold-{n}")));
        Ok(open_once(&driven, &f)?.0)
    })?;
    Ok(vec![
        Measured::new("review/open_session_50", Value::Time(time)).with_note(Some(note(ctx))),
    ])
}

fn open_session_settled(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let f = fixture(ctx)?;
    let scratch = ctx.scratch().to_path_buf();
    let time = sample(ctx, |n| {
        let driven = f.driven(&scratch.join(format!("settled-{n}")));
        Ok(open_once(&driven, &f)?.1)
    })?;
    Ok(vec![
        Measured::new("review/open_session_settled", Value::Time(time)).with_note(Some(note(ctx))),
    ])
}

fn open_session_50_warm(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let f = fixture(ctx)?;
    let dir = ctx.scratch().join("warm");
    {
        let driven = f.driven(&dir);
        let (_, _, id) = open_once(&driven, &f)?;
        driven.close(id);
        driven.close_cache();
    }
    let time = sample(ctx, |_| {
        let driven = f.driven(&dir);
        let (first, _, id) = open_once(&driven, &f)?;
        driven.close(id);
        driven.close_cache();
        Ok(first)
    })?;
    Ok(vec![
        Measured::new("review/open_session_50_warm", Value::Time(time)).with_note(Some(note(ctx))),
    ])
}

fn cutoffs_cold(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let f = fixture(ctx)?;
    let scratch = ctx.scratch().to_path_buf();
    let mut n = 0;
    let mut wrong: Option<CutoffRun> = None;
    let time = ctx.time_with_setup(
        || {
            n += 1;
            scratch.join(format!("cutoffs-{n}"))
        },
        |dir| {
            let run = f.cutoffs(&dir);
            if run.unknown != 0 || run.records == 0 {
                wrong.get_or_insert(run);
            }
            run
        },
    );
    if let Some(run) = wrong {
        return Err(BenchError::debug("cutoffs", run));
    }
    Ok(vec![
        Measured::new("review/cutoffs_cold", Value::Time(time)).with_note(Some(note(ctx))),
    ])
}

fn rows_first(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let f = fixture(ctx)?;
    let driven = f.driven(&ctx.scratch().join("rows"));
    let (_, _, id) = open_once(&driven, &f)?;
    let file = driven.snapshot(id).unwrap().files[0].key.clone();
    let mut context = 3;
    let mut wrong = false;
    let time = ctx.time(|| {
        // Another context each time, so that the rows are computed again.
        context = if context == 3 { 4 } else { 3 };
        let key = RowsKey::Final {
            file: file.clone(),
            context,
        };
        driven.command(
            id,
            ReviewCommand::Prepare(vec![Need::Rows(key.clone())].into()),
        );
        driven.run();
        let rows = driven.rows(id, &key);
        wrong |= rows.as_ref().is_none_or(|r| r.rows.is_empty());
        rows
    });
    if wrong {
        return Err(BenchError::new("no rows prepared"));
    }
    Ok(vec![
        Measured::new("review/rows_first", Value::Time(time))
            .with_note(Some("the first file, 300 lines".to_owned())),
    ])
}

fn mark_seen_5000(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let files = ctx.scale(5000, 500);
    let fixture = MarkSeenFixture::new(files);
    let driven = fixture.driven(&ctx.scratch().join("seen"));
    let id = driven.open(driven.outside());
    driven.run();
    let snapshot = driven
        .snapshot(id)
        .ok_or_else(|| BenchError::new("closed"))?;
    if snapshot.files.len() != files {
        return Err(BenchError::debug("files", snapshot.files.len()));
    }
    let keys: Vec<_> = snapshot.files.iter().map(|f| f.key.clone()).collect();
    let mut n = 0;
    let time = ctx.time(|| {
        n = (n + 1) % keys.len();
        driven.command(id, ReviewCommand::MarkSeen(keys[n].clone()));
    });
    // The log's appends, queued meanwhile.
    driven.run();
    let seen = driven.snapshot(id).map_or(0, |s| s.progress.seen);
    if seen == 0 {
        return Err(BenchError::new("nothing marked"));
    }
    Ok(vec![
        Measured::new("review/mark_seen_5000", Value::Time(time))
            .with_note(Some(format!("{files} files, the seen array replaced"))),
    ])
}

fn attribute_session_50(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let f = fixture(ctx)?;
    let scratch = ctx.scratch().to_path_buf();
    let mut n = 0;
    let mut wrong = None;
    let time = ctx.time_with_setup(
        || {
            n += 1;
            let driven = f.driven(&scratch.join(format!("attribute-{n}")));
            let id = open_once(&driven, &f).map(|(_, _, id)| id);
            (driven, id)
        },
        |(driven, id)| {
            let lines = id.map(|id| driven.attribute_uncached(id, 1));
            if !matches!(lines, Ok(49)) {
                wrong.get_or_insert(format!("{lines:?}"));
            }
            driven
        },
    );
    if let Some(wrong) = wrong {
        return Err(BenchError::new(format!("attribution: {wrong}")));
    }
    Ok(vec![
        Measured::new("review/attribute_session_50", Value::Time(time)).with_note(Some(note(ctx))),
    ])
}
