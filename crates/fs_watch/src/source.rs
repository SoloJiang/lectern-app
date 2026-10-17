//! The filter and config sources `git` implements, and the consumer callback.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use lectern_kernel::BusyGuard;

use crate::path::WatchPath;

/// Implemented by `git` (`git::Worktree::watch_source`).
pub trait WorktreeSource: Send + Sync + 'static {
    /// Reads the index and the ignore rules (including untracked
    /// `.gitignore` files in directories that are not ignored) and builds a
    /// filter. `gitignores` are the `.gitignore` paths in the events that
    /// triggered this load: read even when the walk hits its entry limit
    /// before finding them; those under an ignored directory are still not
    /// read. Blocks; fs_watch calls it in an executor `Background` task and
    /// sets `cancel` to give up.
    fn filter(
        &self,
        gitignores: &[WatchPath],
        cancel: &AtomicBool,
    ) -> Result<Arc<dyn PathFilter>, SourceError>;

    /// Every file the worktree's effective config, ignore rules and
    /// attribute rules read (or would read once the file appears), as
    /// absolute paths that may not exist. Blocks; called like `filter`.
    fn config_files(&self, cancel: &AtomicBool) -> Result<Vec<PathBuf>, SourceError>;
}

/// Answers from memory only, no IO, on any thread. Paths are relative to the
/// worktree root and satisfy the `WatchPath` invariants. Case and Unicode
/// normalization follow the repository's `core.ignoreCase` and
/// `core.precomposeUnicode`; that is up to the implementation.
pub trait PathFilter: Send + Sync + 'static {
    fn tracked(&self, path: &[u8]) -> Tracked;
    /// Whether `path` itself is excluded by the ignore rules, not looking at
    /// its parents; fs_watch checks those one by one.
    fn ignored(&self, path: &[u8], is_dir: bool) -> bool;
    /// The worktree's `.gitignore` files read when this filter was built,
    /// in byte order. fs_watch uses them to tell whether a directory event
    /// touches the rules.
    fn rule_files(&self) -> &[WatchPath];
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tracked {
    No,
    /// The index has this path (skip-worktree entries included).
    File,
    /// The index has paths under this directory.
    Dir,
    /// The index has this path as a submodule (gitlink).
    Gitlink,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SourceError(pub String);

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SourceError {}

/// A consumer of batches.
pub trait Sink<B>: Send + Sync + 'static {
    /// Delivers a coalesced batch. Called on the fs_watch thread inside
    /// `contain`; only updates state or submits background work: no
    /// blocking, no IO, no `flush_revision`. `busy` is the guard taken with
    /// `busy()` when the batch's first event arrived; the consumer moves it
    /// into the task handling the batch and drops it when that task ends
    /// (docs/design/kernel.md「静止」). After a panic this subscription
    /// delivers nothing more; the others carry on.
    fn deliver(&self, batch: B, busy: Option<BusyGuard>);

    /// Takes a guard; plugins return `PluginHandle::busy()`. Called on the
    /// fs_watch thread, the thread calling `watch_*` or a backend callback
    /// thread (`EventSink::send`); must not block or take a lock contended
    /// by those threads. With None (the plugin is deactivated) batches are
    /// coalesced and delivered as usual, without a guard.
    fn busy(&self) -> Option<BusyGuard> {
        None
    }
}
