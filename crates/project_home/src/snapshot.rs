//! What the views read: the immutable [`HomeSnapshot`] and [`build`], the
//! pure function that composes it from the services' snapshots
//! (docs/design/project_home.md「视图读取的快照」). Nothing here reads a
//! service or the clock, and nothing is formatted: the views turn these
//! values into text with `strings(cx)` when they render.

use std::sync::Arc;

use lectern_services::project::{ProjectKey, RelPath, WorktreeKey};
use lectern_services::review::Progress;
use lectern_services::session::{Agent, SessionKey, SourceId, Timestamp};
use lectern_ui::semantic::NodeId;

mod compose;
#[cfg(test)]
pub(crate) mod render;

pub use compose::{Built, Inputs, UsageInputs, build};

/// Immutable; replaced as a whole. Composed by the "project-home" plugin in a
/// `Background` task; the views hold the `Arc`.
///
/// The `NodeId`s of the probe's semantic tree are built once here (`build`) and
/// only cloned by the views (`ui::semantic`, ui.md「探针」).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HomeSnapshot {
    pub project: ProjectKey,
    /// The generation of each source this was composed from.
    pub sources: SourceGenerations,
    /// `ProjectSnapshot::name`, the toolbar title, shown as written.
    pub name: Arc<str>,
    /// The time of day this was composed at. The views count the relative
    /// times ("3 min ago") back from it, so a frame is a function of the
    /// snapshot and the time shown only moves with the next snapshot
    /// (「已知限制」).
    pub now: Timestamp,
    /// The short name of the current worktree's HEAD (`refs/heads/main` →
    /// `main`; a detached HEAD is the first 8 characters of the OID); None
    /// for a folder project.
    pub branch: Option<Arc<str>>,
    pub active_worktree: Option<WorktreeKey>,
    /// The length of `ProjectSnapshot::worktrees`: the toolbar subtitle's
    /// "N worktrees"; 0 for a folder project.
    pub worktree_count: u32,
    /// `ProjectSnapshot::active.files` is no longer `Pending`: the condition
    /// of the milestone (「性能」).
    pub files_settled: bool,
    pub sidebar: SidebarModel,
    pub main: MainModel,
    /// None when the `usage` plugin is not there.
    pub usage: Option<UsageModel>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SourceGenerations {
    pub project: u64,
    pub sessions: Arc<[(SourceId, u64)]>,
    pub overview: Option<u64>,
    /// The usage snapshot's generation, or the limits' when there is no
    /// snapshot yet.
    pub usage: Option<u64>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SidebarModel {
    /// In the order of `ProjectSnapshot::worktrees` (the main one first); a
    /// folder project has one group with `worktree` None; sessions placed in a
    /// worktree that is not in the list (prunable) are in the last group,
    /// `Other`. A worktree without sessions has no group.
    pub groups: Arc<[WorktreeGroup]>,
    /// The number of session rows in the sidebar.
    pub session_rows: u32,
    pub packages: Arc<[PackageRow]>,
    /// Whether today's change counts are available: false when no counts were
    /// given or the overview is `Capped`; then only the names show.
    pub package_counts: bool,
    /// The rows of the sidebar's scrolling area that come from the snapshot,
    /// flat, in display order: the Sessions heading, then all the session
    /// rows (or the one `SessionsEmpty` row), then the Packages. The
    /// sessions of all the worktrees are mixed, newest first; folding them
    /// into "N more" is the view's state, not the snapshot's. The
    /// contributed entries are not here.
    pub rows: Arc<[SidebarRow]>,
}

/// One row of the sidebar's scrolling area; the indices point into
/// `groups` and `packages`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SidebarRow {
    SessionsHeader,
    Session {
        group: u32,
        index: u32,
    },
    SessionsEmpty,
    PackagesHeader,
    Package(u32),
    /// One skeleton row while the sub-projects are `Pending`.
    PackagesSkeleton,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WorktreeGroup {
    pub worktree: Option<WorktreeKey>,
    pub kind: GroupKind,
    pub sessions: Arc<[SessionRow]>,
    pub node: NodeId,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum GroupKind {
    Worktree { branch: Arc<str>, is_active: bool },
    Folder,
    Other,
}

/// A sidebar row and a main-area card share it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SessionRow {
    pub key: SessionKey,
    pub agent: Agent,
    /// None shows `project-home-untitled-session`.
    pub title: Option<Arc<str>>,
    pub last_activity: Timestamp,
    /// The probe node of the row where it is shown: a sidebar row's, or the
    /// card's in Needs Review or Recently Done.
    pub node: NodeId,
    /// `open_turn` and `last_activity` within 10 minutes of the composing
    /// time: stricter than `agents`' "active", so that a session whose last
    /// turn never ended but that has been silent for days (an interrupted
    /// one) is not "running".
    pub running: bool,
    pub tail_pending: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PackageRow {
    pub dir: RelPath,
    pub name: Arc<str>,
    /// How many enclosing sub-projects (along `Package::parent`); the sidebar
    /// indents by it.
    pub depth: u8,
    /// Files changed today; 0 when `package_counts` is false.
    pub changed_today: u32,
    pub node: NodeId,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MainModel {
    /// The sessions of the project that are running (`running` of the rows
    /// of `SessionList::in_project`, in any worktree): the large title's
    /// second sentence.
    pub running: u32,
    /// `Review::overview` has a first result; before that the main area shows
    /// skeletons (`loading_phase`).
    pub overview_ready: bool,
    /// The number in the large title is `needs_review.len()`.
    pub needs_review: Arc<[Card]>,
    pub outside: OutsideRow,
    pub done: Arc<[Card]>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Card {
    pub session: SessionRow,
    /// The short branch name of the worktree the session is placed in; None
    /// for a folder project and for a session in no listed worktree. The
    /// first line of the card shows it.
    pub branch: Option<Arc<str>>,
    /// The status word at the end of the first line. Only `Running` (from
    /// `session.running`) until `agents` tells how a session ended
    /// (project_home.md「对其他 crate 的要求」F); None shows no word.
    pub status: Option<SessionStatus>,
    /// `SessionOverview::progress`; None draws no ring.
    pub progress: Option<Progress>,
    pub signals: Signals,
}

/// How a session stands, the status word of a card's first line. Only
/// `Running` has a source today.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SessionStatus {
    Running,
    /// Waiting for the user (not produced yet).
    Waiting,
    /// The session ended (not produced yet).
    Ended,
}

/// The sources of the second grey line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Signals {
    /// `SessionOverview::drifted == Some(true)`.
    pub drifted: bool,
    pub critical: u32,
    /// `Progress::changed_since_seen`.
    pub changed_since_seen: u32,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum OutsideRow {
    /// A folder project has no base, or nothing changed outside the
    /// sessions: no row.
    Hidden,
    Pending,
    /// `names` are the first 3 changed paths (the card's second line);
    /// `OutsideCount` only has the count (project_home.md「对其他 crate 的
    /// 要求」F), so they are empty until `review` gives paths.
    Files {
        count: u32,
        names: Arc<[RelPath]>,
    },
    /// `OutsideCount::Capped(n)`: all changed paths, not told apart.
    Capped(u32),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct UsageModel {
    /// The total of the last 7 days (`ProjectUsage::total.total()`); None
    /// while the snapshot is not computed.
    pub tokens_7d: Option<u64>,
    /// The window `Limits::tightest` points at.
    pub tightest: Option<TightestLimit>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TightestLimit {
    pub agent: Agent,
    /// `used_percent` rounded into 0..=100.
    pub used_percent: u8,
    /// The length of the limit window ("5-hour"); None when the source did
    /// not say.
    pub window_minutes: Option<u32>,
    pub resets_at: Option<Timestamp>,
}
