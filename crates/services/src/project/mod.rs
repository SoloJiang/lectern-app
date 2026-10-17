//! The project model service: repository → worktree → sub-project
//! (docs/design/project.md「接口」「数据结构」).
//!
//! `project` implements [`Projects`]; consumers (`project_home`,
//! `worktrees`, `agents`, `usage`, `review`) depend only on this module.
//! Everything here is data without behaviour, apart from the `FileList`
//! lookups.

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use lectern_kernel::{Event, Service};

mod files;
mod packages;
mod snapshot;

#[cfg(test)]
mod tests;

pub use files::{FileKind, FileList, FileListBuilder};
pub use packages::{Ecosystem, MAX_MANIFEST_BYTES, Manifest, ManifestError, ManifestKind, Package};
pub use snapshot::{
    ActiveTree, Head, Location, PackageMatch, ProjectKey, ProjectSnapshot, RelPath, Resolved, Scan,
    ScanError, Worktree, WorktreeKey,
};

/// The open projects. A single-provider service, provided by the `project`
/// plugin.
pub trait Projects: Send + Sync {
    /// Opens a path. Returns at once and may be called on any thread; the
    /// result arrives as `ProjectEvent::Opened`. When the path's repository
    /// is already open, the result is the same `ProjectKey` and the active
    /// worktree switches to the one containing the path.
    fn open(&self, path: &Path) -> OpenRequest;

    /// Closes a project: cancels its background work, stops watching, drops
    /// the snapshot and emits `ProjectEvent::Closed`. Returns at once.
    fn close(&self, project: &ProjectKey);

    /// Switches the active worktree. Returns at once; `Changed` follows when
    /// the new worktree's files and sub-projects are ready. Ignored when the
    /// worktree is not in the project or is prunable.
    fn set_active_worktree(&self, project: &ProjectKey, worktree: &WorktreeKey);

    /// The current snapshot. Only copies an `Arc` under a lock: no blocking,
    /// no IO, fine on the UI thread.
    fn snapshot(&self, project: &ProjectKey) -> Option<Arc<ProjectSnapshot>>;

    /// The open projects, in the order they were opened. Does not block.
    fn open_projects(&self) -> Vec<ProjectKey>;

    /// Places a path among the open projects by prefix comparison only (no
    /// IO, any thread). When several open projects contain the path (a
    /// repository and a submodule or nested repository opened on its own),
    /// the one with the deepest matching worktree or folder root wins.
    /// `path` must already be canonical (from `resolve` or
    /// `std::fs::canonicalize`); None when no open project contains it.
    fn locate(&self, path: &Path) -> Option<Location>;

    /// Which project any path belongs to, open or not: canonicalizes the
    /// path and discovers the repository upwards. Blocks (a few stats and
    /// reading `.git` files); call it on the executor only. Callers
    /// (`agents`, `usage`) deduplicate equal paths.
    fn resolve(&self, path: &Path) -> Result<Resolved, ResolveError>;

    /// The deepest sub-project containing `rel` in the given git worktree,
    /// for when `locate` gives `PackageMatch::NotLoaded` for a worktree that
    /// is not active. Blocks; call it on the executor only. Gives `Found` or
    /// `None`, except for the active worktree: then it answers from the
    /// snapshot without IO, and `NotLoaded` while its sub-projects are
    /// still `Pending` (call `locate` again after the next `Changed`).
    /// `Err(ScanError::RootMissing)` when the project is not open or the
    /// worktree is not in it.
    fn resolve_package(
        &self,
        project: &ProjectKey,
        worktree: &WorktreeKey,
        rel: &str,
    ) -> Result<PackageMatch, ScanError>;
}

impl Service for dyn Projects {
    const ID: &'static str = "projects";
    const MULTIPLE: bool = false;
}

/// Identifies one `Projects::open` call in its `ProjectEvent::Opened`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct OpenRequest(pub u64);

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum OpenError {
    NotFound,
    NotADirectory,
    PermissionDenied,
    /// There is a `.git` that cannot be read (a gitdir pointing at a missing
    /// directory, a damaged common directory). With `git`'s reason, for logs.
    RepoUnreadable(Arc<str>),
    Io(Arc<str>),
}

pub type ResolveError = OpenError;

/// Emitted on an executor thread after the snapshot is replaced. Listeners
/// only update state or schedule a redraw; they do no IO.
///
/// A successful `Opened` is followed by at least one `Changed` (the file
/// list is ready or unavailable). Views call `Projects::snapshot` on
/// `Changed`; `generation` only serves to drop stale notifications.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ProjectEvent {
    Opened {
        request: OpenRequest,
        result: Result<ProjectKey, OpenError>,
    },
    Changed {
        project: ProjectKey,
        generation: u64,
        parts: ChangedParts,
    },
    Closed {
        project: ProjectKey,
    },
}

impl Event for ProjectEvent {}

/// What a `ProjectEvent::Changed` changed.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct ChangedParts {
    /// Worktrees added or removed, or a HEAD, locked or prunable changed.
    pub worktrees: bool,
    /// The active worktree changed.
    pub active: bool,
    pub files: bool,
    pub packages: bool,
}
