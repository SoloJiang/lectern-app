//! Project keys and the immutable project snapshot
//! (docs/design/project.md「项目标识」「快照」).

use std::path::Path;
use std::sync::Arc;

use super::{FileList, Package};

/// A path inside a worktree, `/` separated, no leading or trailing `/`;
/// the root is "".
pub type RelPath = Arc<str>;

/// A project's key for this run; also the repository location in `usage`
/// records and what `cache` splits its databases by.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum ProjectKey {
    /// The canonical absolute path of the git common directory. Every
    /// worktree of one repository gets the same key.
    Git { common_dir: Arc<Path> },
    /// A plain folder: the canonical absolute path of the opened root.
    Folder { root: Arc<Path> },
}

/// The canonical absolute path of a worktree's git directory: the common
/// directory for the main worktree, `<common>/worktrees/<id>` for a linked
/// one. Not the root: a prunable worktree's root no longer exists and cannot
/// be canonicalized, while its administrative directory is still there.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct WorktreeKey(pub Arc<Path>);

/// Immutable. Every change replaces the whole snapshot with a new `Arc`, so
/// a view holding an old one is unaffected.
///
/// Invariants (kept by `project`): worktree keys and roots are unique, at
/// most one worktree `is_main`, and `active.worktree` is one of them and
/// not prunable; `packages` is `Ready` only when `files` is, from the same
/// file list; all parts of one snapshot come from the same scan.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ProjectSnapshot {
    pub key: ProjectKey,
    /// Incremented on every replacement, strictly increasing; equal to the
    /// `generation` of the matching `ProjectEvent::Changed`.
    pub generation: u64,
    /// Display name: the main worktree root's directory name; for a bare
    /// repository the common directory's name without `.git`; for a folder
    /// its directory name.
    pub name: Arc<str>,
    /// All worktrees of a git project, the main one first and the rest by
    /// root path. Empty for a folder project.
    pub worktrees: Arc<[Worktree]>,
    pub active: Option<ActiveTree>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Worktree {
    /// The git directory; see `WorktreeKey`.
    pub key: WorktreeKey,
    /// The root directory, canonical when it exists. When prunable, the
    /// path recorded in the administrative `gitdir` file without its
    /// trailing `/.git`, normalized only lexically.
    pub root: Arc<Path>,
    pub is_main: bool,
    pub head: Head,
    pub locked: bool,
    /// The administrative directory exists but the root does not. Listed,
    /// never scanned, and cannot become the active worktree.
    pub prunable: bool,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Head {
    /// The full reference name, such as "refs/heads/main".
    Branch(Arc<str>),
    /// The commit OID in hex; its length follows the hash algorithm.
    Detached(Arc<str>),
    /// A branch with no commits yet.
    Unborn(Arc<str>),
    /// HEAD could not be read.
    Unknown,
}

/// The active worktree (the root directory of a folder project) with its
/// files and sub-projects. Only the active tree keeps a file list.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ActiveTree {
    pub root: Arc<Path>,
    /// None for a folder project.
    pub worktree: Option<WorktreeKey>,
    /// The opened path relative to `root`; "" when the root was opened.
    pub opened_at: RelPath,
    pub files: Scan<Arc<FileList>>,
    pub packages: Scan<Arc<[Package]>>,
}

/// A part of the snapshot that is computed in the background.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Scan<T> {
    Pending,
    Ready(T),
    Unavailable(ScanError),
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ScanError {
    /// With the reason `git` gave.
    IndexUnreadable(Arc<str>),
    RootMissing,
    Io(Arc<str>),
}

/// Where a path is among the open projects (`Projects::locate`).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Location {
    pub project: ProjectKey,
    pub worktree: Option<WorktreeKey>,
    pub rel: RelPath,
    pub package: PackageMatch,
}

/// The deepest sub-project containing a path. `locate` only uses the
/// sub-projects of the active worktree's snapshot (kept current by
/// watching); it never applies them to other worktrees, whose branches may
/// add, remove or move sub-projects.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum PackageMatch {
    /// The matched sub-project's directory.
    Found(RelPath),
    /// The active worktree's sub-projects are ready and the path is in none.
    None,
    /// Two cases, told apart by `Location::worktree`:
    /// - the path is in a worktree that is not active (always this): call
    ///   `Projects::resolve_package` on the executor when needed;
    /// - the path is in the active worktree or a folder project whose
    ///   sub-projects are still `Pending`: call `locate` again after the next
    ///   `ProjectEvent::Changed`.
    NotLoaded,
}

/// Which project a path belongs to (`Projects::resolve`); the project need
/// not be open.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Resolved {
    pub project: ProjectKey,
    pub worktree: Option<WorktreeKey>,
    pub rel: RelPath,
}
