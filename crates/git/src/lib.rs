//! Repository reading with gix (docs/design/git.md, ADR 0024): repositories,
//! worktrees, references, history, the index, change sets between trees,
//! hunks and display rows. Read-only; runs no external program; reads
//! worktree files only through `lectern_platform::SafeRoot`.
//!
//! Every function blocks (object database, index, worktree) and belongs on a
//! background task, never the UI thread; `rows`, `inline_changes` and
//! `merge3` are pure. `Repo`, `Worktree` and `ChangeSet` are `Send + Sync` and hold no
//! gix handle: each top-level call opens one on the calling thread and
//! drops it before returning. Functions taking `cancel` return
//! `GitError::Cancelled` once it is set.

mod attrs;
mod changes;
mod compare;
mod content;
mod conversion;
mod error;
mod format;
mod gitdir;
mod handle;
mod hunks;
mod merge3;
mod oid;
mod parallel;
mod path;
mod reflog;
mod refs;
mod rename;
mod repo;
mod rows;
mod tracked;
mod tree;
mod tree_diff;
mod untracked;
mod watch;
mod worktree;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

pub use changes::{
    Change, ChangeSet, Detail, FileTexts, Fingerprint, Limits, Side, SideText, Staged, Status,
    Target, Text, Totals,
};
pub use compare::{Base, Refresh};
pub use content::{Content, RawIdentity, Unavailable};
pub use error::{GitError, UnsupportedRepo};
pub use hunks::{FileDiff, Hunk};
pub use merge3::{Markers, Merge3, merge3};
pub use oid::{BlobHasher, HashKind, Oid};
pub use path::{Mode, RefName, RepoPath};
pub use reflog::{ReflogAction, ReflogEntry, ReflogParseError, parse_reflog_line};
pub use refs::{BranchInfo, CommitInfo, GitTime, RemoteBranch, RemoteInfo, Signature, StashInfo};
pub use repo::{Discovered, HeadState, Location, Repo, WorktreeId, WorktreeInfo};
pub use rows::{INLINE_MAX_LINE_BYTES, InlineChanges, Row, inline_changes, rows};
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
        assert::<crate::ChangeSet>();
        assert::<crate::FileTexts>();
        assert::<crate::GitError>();
    }
}
