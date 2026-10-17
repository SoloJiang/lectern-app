//! Batches, health, revisions and coalescing parameters.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::path::WatchPath;
use crate::reflog::{ReflogRecord, ReflogReset};

/// One-to-one with `git::WorktreeId`; the conversion is in `git`.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum WorktreeName {
    Main,
    Linked(Arc<str>),
}

/// Two equal reads mean no counted change happened to the worktree between
/// them. `epoch` tells worktree states apart: watching again after every
/// handle was dropped gives a new epoch, so revisions read from different
/// states never compare equal.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Revision {
    pub epoch: u64,
    pub n: u64,
}

/// Coalescing parameters (docs/design/fs_watch.md「合并」): a batch is
/// delivered at `min(last + quiet, first + max_delay)`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Coalesce {
    pub quiet: Duration,
    pub max_delay: Duration,
}

impl Coalesce {
    pub const REPO: Coalesce = Coalesce {
        quiet: Duration::from_millis(20),
        max_delay: Duration::from_millis(200),
    };
    pub const WORKTREE: Coalesce = Coalesce {
        quiet: Duration::from_millis(50),
        max_delay: Duration::from_millis(500),
    };
    pub const DIR: Coalesce = Coalesce {
        quiet: Duration::from_millis(250),
        max_delay: Duration::from_secs(1),
    };
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WorktreeBatch {
    /// This worktree's revision at delivery.
    pub revision: Revision,
    /// Byte order, deduplicated. A directory path (a directory deleted or
    /// renamed, or a backend reporting only the directory) stands for every
    /// path below it. Empty when `rescan` is true.
    pub paths: Vec<WatchPath>,
    pub meta: MetaChanges,
    /// Events were lost or there were too many paths: treat everything as
    /// possibly changed (`git::Refresh::Full`).
    pub rescan: bool,
    pub health: Option<Health>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct MetaChanges {
    /// This worktree's HEAD.
    pub head: bool,
    /// This worktree's index.
    pub index: bool,
    /// `refs/`, `packed-refs`, `reftable/`.
    pub refs: bool,
    /// `config`, `config.worktree`, `info/exclude`, `info/attributes`, or any
    /// file on the config list.
    pub config: bool,
    /// `paths` holds a path named `.gitignore` or `.gitattributes`.
    pub rules: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RepoBatch {
    /// Worktrees whose HEAD or index changed, sorted by name.
    pub worktrees: Vec<(WorktreeName, WorktreeMeta)>,
    /// Directories were added to or removed from `<common dir>/worktrees/`,
    /// or a worktree's `gitdir`, `locked` or `commondir` changed.
    pub worktree_list: bool,
    pub refs: bool,
    /// `config`, `info/exclude`, `info/attributes` in the common directory.
    /// Config files outside the repository only count in worktree batches.
    pub config: bool,
    /// Both sorted by `seq`, which they share: merged by `seq` they give the
    /// order of records and resets (see `ReflogReset`).
    pub reflog: Vec<ReflogRecord>,
    pub reflog_resets: Vec<ReflogReset>,
    pub rescan: bool,
    pub health: Option<Health>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct WorktreeMeta {
    pub head: bool,
    pub index: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DirBatch {
    /// Absolute paths (`DirSpec::root` joined with the relative path, not
    /// normalized), byte order, deduplicated.
    pub paths: Vec<PathBuf>,
    pub rescan: bool,
    /// Some when the root appeared (`Appeared`, with `rescan` true) or
    /// vanished (`Vanished`).
    pub root: Option<RootState>,
    pub health: Option<Health>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RootState {
    Appeared,
    Vanished,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Health {
    Live,
    /// Still watching, with one degradation (docs/design/fs_watch.md
    /// 「状态与异常」).
    Degraded(Degraded),
    Failed(Failure),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Degraded {
    /// The filter failed to load: every path except under `.git` is
    /// delivered and counted.
    Filter(String),
    /// The config list failed to load, or one of its targets could not be
    /// armed: only the config files in the repository are watched.
    Config(String),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Failure {
    Backend(String),
    RootGone,
    Unmounted,
    Stopped,
}

/// Why `flush_revision` gave no revision.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FlushError {
    Timeout,
    /// The worktree state failed (backend error, root gone); its revision is
    /// no longer trustworthy. Callers treat it as changed.
    NotWatching(Failure),
    /// Called on the fs_watch thread (inside a `Sink` callback).
    WouldDeadlock,
}

impl std::fmt::Display for FlushError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlushError::Timeout => f.write_str("timed out waiting for the fs_watch thread"),
            FlushError::NotWatching(failure) => write!(f, "not watching: {failure:?}"),
            FlushError::WouldDeadlock => f.write_str("called on the fs_watch thread"),
        }
    }
}

impl std::error::Error for FlushError {}
