//! Repository reading with gix (docs/design/git.md, ADR 0024): repositories,
//! worktrees, references, history and the index. Read-only; runs no
//! external program; reads worktree files only through
//! `lectern_platform::SafeRoot`.
//!
//! Every function blocks (object database, index, worktree) and belongs on a
//! background task, never the UI thread. `Repo` and `Worktree` are
//! `Send + Sync` and hold no gix handle: each top-level call opens one on
//! the calling thread and drops it before returning. Functions taking
//! `cancel` return `GitError::Cancelled` once it is set.

mod content;
mod error;
mod format;
mod gitdir;
mod handle;
mod oid;
mod path;
mod reflog;
mod refs;
mod repo;
mod tracked;
mod tree;
mod worktree;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

pub use content::{RawIdentity, Unavailable};
pub use error::{GitError, UnsupportedRepo};
pub use oid::{HashKind, Oid};
pub use path::{Mode, RefName, RepoPath};
pub use reflog::{ReflogAction, ReflogEntry, ReflogParseError, parse_reflog_line};
pub use refs::{BranchInfo, CommitInfo, GitTime, RemoteBranch, RemoteInfo, Signature, StashInfo};
pub use repo::{Discovered, HeadState, Location, Repo, WorktreeId, WorktreeInfo};
pub use tracked::{EntryLimits, TrackedEntry, TrackedPaths};
pub use worktree::{Worktree, WorktreeFile};

#[cfg(test)]
mod send_sync {
    fn assert<T: Send + Sync>() {}

    #[test]
    fn repo_types_are_send_and_sync() {
        assert::<crate::Repo>();
        assert::<crate::Worktree>();
        assert::<crate::TrackedPaths>();
        assert::<crate::GitError>();
    }
}
