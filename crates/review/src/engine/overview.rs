//! The project overview (docs/design/review.md「概览」): for a project
//! with a watcher, the current worktree's count comparison (zero retention,
//! ADR 0035), the sessions of the window with their claims, the changes
//! outside any session, each session's progress, its Drifted signal from
//! the attribution cache and its critical count. Recomputed on a
//! `Background` round after the triggers of two seconds are merged; held
//! while an `OverviewInterest` lives, released with the last one.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use lectern_executor::Priority;
use lectern_fs_watch::{
    Coalesce, RepoBatch, RepoSpec, RepoWatch, Sink, WorktreeBatch, WorktreeSpec, WorktreeWatch,
};
use lectern_git::{Base, ChangeSet, GitError, HeadReflog, Limits, Refresh, RepoPath};
use lectern_kernel::BusyGuard;
use lectern_logging::{Ctx, debug, warn};
use lectern_services::changes::{ChangePath, ChangeStatus, FileChange, RefreshHint};
use lectern_services::project::{ActiveTree, ChangedParts, ProjectKey, RelPath, Scan, WorktreeKey};
use lectern_services::review::{
    OutsideCount, Overview, OverviewInterest, Progress, ReviewEvent, ReviewTarget, SessionOverview,
};
use lectern_services::session::{SessionKey, SessionSummary, Timestamp};

use super::round::{
    CutoffMode, Git, head_commit, oid_of, other_sessions, relative, session_base, side_oid,
    source_of, target_key,
};
use super::watch::{repo_changed, worktree_hint};
use super::{Engine, lock};
use crate::brief::sensitive_is_critical;
use crate::chain::codec::{self, CachedChain};
use crate::chain::history::{self, HistoryStep};
use crate::chain::{Detail, Fate, Source};
use crate::claims::{self, CachedClaims, ClaimKind, Claims};
use crate::cutoff::{self, Record};
use crate::diag;
use crate::log::SessionId;
use crate::noise;
use crate::seen::{self, SeenKey};

/// Triggers within this long of each other make one round.
pub(crate) const MERGE_WINDOW: Duration = Duration::from_secs(2);
/// Changed paths kept: past it only the count stays (ADR 0035).
pub(crate) const MAX_PATHS: usize = 4000;
/// Sessions in the overview at most.
pub(crate) const MAX_SHOWN: usize = 50;
/// Sessions whose base is not HEAD compared again per round at most.
pub(crate) const MAX_OTHER_BASES: usize = 8;
/// HEAD reflog records read at most (as a review reads them).
const MAX_REFLOG: usize = 4096;

mod packages;
mod round;
mod session;
mod sink;
mod state;

pub(in crate::engine) use round::count_limits;
pub(in crate::engine) use state::Counted;
use state::{Known, Watching};
pub(crate) use state::{Overviews, Trigger};
