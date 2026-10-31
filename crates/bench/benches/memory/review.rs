//! `review/resident_notes_bytes` (step 6): the annotation log's index with
//! 280 review notes, in live bytes, read from a file as an open project
//! reads it.
//!
//! `review/resident_cloudprint_bytes`, `review/resident_100k_bytes`
//! (docs/design/review.md「性能」): what this crate holds at rest with a
//! project's overview ready, in live bytes (`CountingAlloc`), the count
//! change set not counted. The engine is driven without the plugin
//! (`testing::Driven`): the bytes of a `Driven` with nothing watched are
//! the baseline; the count set's own bytes are those of the same
//! comparison made once more outside the engine. The fixture (the
//! repository, the sessions' indexes, the projects service) is built
//! before the count. The first sample is a warm-up and is not reported.
//!
//! The cloudprint shape is written whole, 20 sessions each editing one
//! file. The large shape (100 000 paths, 17 worktrees' worth of index) is
//! index-only: every path is missing from the worktree, the comparison
//! lists more than the overview keeps, so the overview is capped and holds
//! no set at all.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use lectern_bench::{Bench, BenchError, CountingAlloc, Ctx, Measured, Value};
use lectern_git::{Base, Limits, WorktreeId};
use lectern_project::testing::Shape;
use lectern_review::testing::{Driven, load_notes_log, write_notes_log};

use crate::review_repo::{self, ReviewRepo};

const SAMPLES: usize = 3;

pub const RESIDENT_CLOUDPRINT: Bench = Bench {
    id: "review/resident_cloudprint_bytes",
    run: resident_cloudprint,
};

pub const RESIDENT_100K: Bench = Bench {
    id: "review/resident_100k_bytes",
    run: resident_100k,
};

pub const RESIDENT_NOTES: Bench = Bench {
    id: "review/resident_notes_bytes",
    run: resident_notes,
};

/// 40 notes not sent, 240 sent in six batches; 200 bytes of text and five
/// lines of excerpt each.
fn resident_notes(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let dir = ctx.scratch().join("notes-log");
    let common = std::path::Path::new("/bench/repo/.git");
    write_notes_log(&dir, common, 40, 240)?;
    let mut samples = Vec::new();
    for round in 0..=ctx.scale(SAMPLES, 1) {
        let before = CountingAlloc::live();
        let held = load_notes_log(&dir, common);
        let bytes = CountingAlloc::live().saturating_sub(before) as u64;
        // 280 notes and six batches.
        if held.records() != 286 {
            return Err(BenchError::debug("records", held.records()));
        }
        drop(held);
        if round > 0 {
            samples.push(bytes);
        }
    }
    Ok(vec![
        Measured::new("review/resident_notes_bytes", Value::Bytes(samples))
            .with_note(Some("280 notes: 40 not sent, 240 sent".to_owned())),
    ])
}

fn resident_cloudprint(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let repo = review_repo::build(&Shape::cloudprint(), true, ctx.scale(20, 4))?;
    let samples = resident(ctx, &repo)?;
    Ok(vec![Measured::new(
        "review/resident_cloudprint_bytes",
        Value::Bytes(samples),
    )])
}

fn resident_100k(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (files, manifests) = ctx.scale((100_000, 2_000), (2_000, 40));
    let repo = review_repo::build(&Shape::monorepo(files, manifests), false, ctx.scale(20, 4))?;
    let samples = resident(ctx, &repo)?;
    Ok(vec![Measured::new(
        "review/resident_100k_bytes",
        Value::Bytes(samples),
    )])
}

/// The count comparison's own live bytes: the set the overview keeps,
/// made once more outside the engine and dropped.
fn count_set_bytes(repo: &ReviewRepo) -> Result<u64, BenchError> {
    let worktree = repo.t.repo().worktree(&WorktreeId::Main)?;
    let limits = Limits {
        retained_bytes: 0,
        total_patch_bytes: 0,
        ..Limits::default()
    };
    let before = CountingAlloc::live();
    let set = worktree.changes(Base::Head, &limits, &AtomicBool::new(false))?;
    let held = CountingAlloc::live().saturating_sub(before) as u64;
    // Past the cap the overview keeps no set.
    let kept = if set.files().len() > 4000 { 0 } else { held };
    drop(set);
    Ok(kept)
}

fn resident(ctx: &mut Ctx, repo: &ReviewRepo) -> Result<Vec<u64>, BenchError> {
    let set_bytes = count_set_bytes(repo)?;
    let mut samples = Vec::new();
    for round in 0..=ctx.scale(SAMPLES, 1) {
        let dir = ctx.scratch().join(format!("resident-{round}"));
        let before = CountingAlloc::live();
        let driven = Driven::new(
            Arc::clone(&repo.projects),
            vec![Arc::clone(&repo.sessions)],
            &dir,
            repo.now,
        );
        let idle = CountingAlloc::live().saturating_sub(before) as u64;
        let project = driven.project();
        let interest = driven.watch_overview(&project);
        driven.run();
        if driven.overview(&project).is_none() {
            return Err(BenchError::new("no overview after the first round"));
        }
        let ready = CountingAlloc::live().saturating_sub(before) as u64;
        drop(interest);
        driven.run();
        driven.close_cache();
        drop(driven);
        if round > 0 {
            samples.push(ready.saturating_sub(idle).saturating_sub(set_bytes));
        }
    }
    Ok(samples)
}
