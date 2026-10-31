//! The parallel worktree overview (docs/design/review.md「概览」 step
//! 「并行 worktree 总览」, docs/design/worktrees.md「对其他 crate 的要求」A):
//! for every worktree of a project that is not prunable, the files changed
//! and not committed, the files changed against the default branch since
//! the merge base, their union, and the files two worktrees both changed.
//!
//! - Computed only while a `watch_worktrees` interest lives, on one
//!   `Background` task at a time that compares one worktree after another
//!   (the current one first) and publishes after each; the last interest
//!   cancels it. The counts, the path hashes and the overlaps stay until
//!   the project closes; events only mark them stale.
//! - The current worktree's uncommitted count is the overview's own count
//!   set when an overview interest holds one; every other worktree is
//!   compared with a handle that is dropped afterwards (no stat cache,
//!   zero retention, ADR 0035).
//! - Between two comparisons of a worktree at least `max(2 s, 10 × the
//!   last one's duration)` pass; triggers in between merge.
//! - Locks: `worktrees` is a leaf lock. Nothing else is taken while it is
//!   held and no IO is done under it.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use lectern_executor::Priority;
use lectern_fs_watch::{Coalesce, RepoBatch, RepoSpec, RepoWatch, Sink, WorktreeName};
use lectern_git::{Oid, RepoPath};
use lectern_kernel::BusyGuard;
use lectern_logging::{Ctx, debug, warn};
use lectern_services::project::{ChangedParts, ProjectKey, RelPath, WorktreeKey};
use lectern_services::review::{
    ChangeCount, Overlap, OverviewInterest, ReviewEvent, WorktreeChanges, WorktreesOverview,
};
use lectern_services::session::{SessionKey, Timestamp};

use super::{Engine, RETAIN, lock};
use crate::diag;

mod compute;
mod round;
mod sink;
mod state;

pub(crate) use state::WtStates;
use state::{Marks, Need, Pair, WtState};

#[cfg(test)]
pub(in crate::engine) use compute::hash_path;
#[cfg(test)]
pub(in crate::engine) use round::gap_ms;

/// Changed paths kept for the overlaps, per worktree: past it only the
/// count stays (ADR 0035).
pub(crate) const MAX_PATHS: usize = super::overview::MAX_PATHS;
/// Overlap items per worktree.
const MAX_OVERLAPS: usize = 8;
/// Sample paths per overlap.
const MAX_SAMPLES: usize = 10;
/// A count older than this is made again when a watch starts: a worktree
/// that is not the current one has no file watch.
const STALE_MS: i64 = 5 * 60 * 1000;
/// Between two comparisons of one worktree at least this long.
const MIN_GAP_MS: i64 = 2000;
/// …or this many times the duration of the last comparison.
const GAP_FACTOR: i64 = 10;
/// With more path hashes than this in all, a watch released for
/// `RETAIN` lets them go (512 KiB of `u64`).
const HASH_LIMIT: usize = 65_536;
/// Merge bases remembered per project at most.
const MAX_MERGE_BASES: usize = 256;
/// Sessions marked per project at most; beyond it every worktree's
/// uncommitted count is made again.
const MAX_MARKED_SESSIONS: usize = 256;
