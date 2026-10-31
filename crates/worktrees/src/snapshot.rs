//! What the views read: the immutable [`WorktreesSnapshot`] and [`build`], the
//! pure function that composes it from the services' snapshots
//! (docs/design/worktrees.md「视图读取的快照」「合成规则」). Nothing here reads a
//! service or the clock, and nothing is formatted: the views turn these values
//! into text with `strings(cx)` when they render.

use std::path::Path;
use std::sync::Arc;

use lectern_services::project::{ProjectKey, RelPath, WorktreeKey};
use lectern_services::review::ChangeCount;
use lectern_services::session::{Agent, SessionKey, SourceId, Timestamp};
use lectern_services::usage::{MrLabel, Tokens};
use lectern_ui::semantic::NodeId;

mod compose;
#[cfg(test)]
pub(crate) mod render;

pub use compose::{Built, Inputs, UsageInputs, build};

/// Immutable; replaced as a whole. Composed by the "worktrees" plugin in a
/// `Background` task; the views hold the `Arc`.
///
/// The `NodeId`s of the probe's semantic tree are built once here (`build`)
/// and only cloned by the views (`ui::semantic`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WorktreesSnapshot {
    pub project: ProjectKey,
    /// The generation of each source this was composed from.
    pub sources: SourceGenerations,
    /// The time of day this was composed at (the sessions' clock, UTC
    /// milliseconds). The views count the relative times back from it.
    pub built_at: Timestamp,
    /// `ProjectSnapshot::active.worktree`.
    pub active: Option<WorktreeKey>,
    /// `ProjectSnapshot::active.files` is no longer `Pending`: the condition
    /// of the milestone (「性能」).
    pub files_settled: bool,
    /// How many times the plugin saw the current worktree change
    /// (`ProjectEvent::Changed` with `parts.active`). The milestone goes by it.
    pub switches: u64,
    /// The base. None while the `WorktreesOverview` has not arrived, and when
    /// the repository has no base.
    pub base: Option<BaseRef>,
    pub active_rows: Arc<[WorktreeRow]>,
    pub idle_rows: Arc<[WorktreeRow]>,
    /// The first instant at which a session that is running now no longer
    /// counts as running: the least of `last_activity + 10 min + 1 ms` of
    /// all the running sessions. None when no session is running.
    pub next_expiry: Option<Timestamp>,
    /// The project is a folder project (it has no worktrees): the screen
    /// shows its empty state.
    pub folder: bool,
}

impl WorktreesSnapshot {
    /// What the views get when the project closes: no rows, so that they
    /// clear.
    pub(crate) fn empty(project: ProjectKey, built_at: Timestamp, switches: u64) -> Self {
        WorktreesSnapshot {
            folder: matches!(project, ProjectKey::Folder { .. }),
            project,
            sources: SourceGenerations {
                project: 0,
                sessions: Arc::from(Vec::new()),
                changes: None,
                usage: 0,
            },
            built_at,
            active: None,
            files_settled: false,
            switches,
            base: None,
            active_rows: Arc::from(Vec::new()),
            idle_rows: Arc::from(Vec::new()),
            next_expiry: None,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SourceGenerations {
    pub project: u64,
    pub sessions: Arc<[(SourceId, u64)]>,
    /// `WorktreesOverview::generation`.
    pub changes: Option<u64>,
    /// The change count of the `UsageLink`.
    pub usage: u64,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BaseRef {
    /// The full reference name (`refs/remotes/origin/main`), handed as it is
    /// to `BaseSpec::MergeBase`.
    pub full: Arc<str>,
    /// For display: without `refs/heads/` or `refs/remotes/`
    /// (`origin/main`).
    pub short: Arc<str>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WorktreeRow {
    pub key: WorktreeKey,
    /// `Worktree::root`, which the inspector shows as it is.
    pub root: Arc<Path>,
    pub head: HeadLabel,
    pub is_active: bool,
    pub is_main: bool,
    pub locked: bool,
    pub prunable: bool,
    /// The sessions of the last 14 days that count for this row, and how many
    /// of them are running (the rules are in `build`).
    pub sessions: u32,
    pub running: u32,
    /// At most 5 sessions, newest `last_activity` first. The row shows the
    /// first, the inspector all.
    pub recent: Arc<[SessionRow]>,
    pub changes: RowChanges,
    pub overlaps: Overlaps,
    pub usage: RowUsage,
    pub node: NodeId,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum HeadLabel {
    /// The short name of the branch: `refs/heads/feat/x` → `feat/x`.
    Branch(Arc<str>),
    /// The first 8 characters of the commit OID.
    Detached(Arc<str>),
    /// The short name of a branch with no commits yet.
    Unborn(Arc<str>),
    Unknown,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SessionRow {
    pub key: SessionKey,
    pub agent: Agent,
    /// None shows `worktrees-untitled-session`.
    pub title: Option<Arc<str>>,
    pub last_activity: Timestamp,
    pub running: bool,
    pub node: NodeId,
}

/// The fields of the same names of `WorktreeChanges`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RowChanges {
    pub uncommitted: ChangeCount,
    pub committed: ChangeCount,
    pub vs_base: ChangeCount,
    pub counted_at: Option<Timestamp>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Overlaps {
    /// The counts have not arrived.
    Pending,
    /// The set of paths of this worktree is not known: its counts have
    /// arrived and `WorktreeChanges::overlaps` is None (too many changes), and
    /// a prunable worktree, which `review` does not compare.
    Unknown,
    List(Arc<[OverlapRow]>),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OverlapRow {
    pub other: WorktreeKey,
    /// The branch label of the other worktree.
    pub other_head: HeadLabel,
    pub files: u32,
    /// `Overlap::paths` (at most 10), which the inspector lists when the item
    /// is expanded.
    pub paths: Arc<[RelPath]>,
    pub node: NodeId,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RowUsage {
    /// There is no `usage` plugin, or the worktree is prunable (nothing is
    /// asked for it): no token and no usage section.
    Absent,
    /// The snapshot is not computed yet (none, or `UsageState::Loading`).
    Pending,
    /// The default branch or a detached HEAD: `WorktreeUsage::cumulative` is
    /// None and `usage` does not count it.
    NotCounted,
    /// P1: the usage of the branch.
    Branch(BranchUsage),
    /// P3: `WorktreeUsage::mrs` is not empty, one item per MR.
    Mrs(Arc<[MrItem]>),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BranchUsage {
    pub cumulative: Tokens,
    pub review: Tokens,
    pub since: Option<Timestamp>,
    /// `SnapshotHeader::may_include_inherited`.
    pub may_include_inherited: bool,
    /// `UsageState::Backfilling`: the history is still being read and the
    /// numbers will grow.
    pub backfilling: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MrItem {
    pub label: MrLabel,
    /// The writing usage of this MR's interval: `WorktreeUsage::cumulative`
    /// when `mrs` has one item, `MrUsage::write` of this MR when it has more;
    /// None while there is no snapshot.
    pub tokens: Option<Tokens>,
    /// The other MRs that share the branch with this one
    /// (`MrUsage::shared_with.len()`; 0 for a single item, and while the
    /// snapshot of this MR is missing).
    pub shared_with: u32,
}
