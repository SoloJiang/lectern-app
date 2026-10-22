//! Platform-neutral file watching (docs/design/fs_watch.md). Worktree events
//! are filtered by the git index and ignore rules and coalesced into
//! batches; each worktree has a revision counter that `flush_revision`
//! reads after the backend delivered what already happened; new reflog
//! lines get read stamps and sequence numbers; directories outside git are
//! watched unfiltered.
//!
//! The FSEvents and kqueue backends live in `platform` and are injected by
//! `app`; the filter and the config list come from `git` through
//! [`WorktreeSource`]. No function here does IO on the calling thread except
//! [`WorktreeWatch::flush_revision`], which blocks: backend arms, reflog
//! reads and `lstat`s run on the fs_watch thread, filter and config loads in
//! executor tasks.

mod backend;
mod clock;
mod diag;
mod engine;
mod path;
mod queue;
mod reflog;
mod source;
mod types;
mod util;
mod watcher;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

pub use backend::{
    Armed, Backend, BackendError, EventSink, Flags, RawBatch, RawEvent, Scope, Target,
};
pub use path::WatchPath;
pub use reflog::{Origin, ReadStamp, ReflogFile, ReflogRecord, ReflogReset, ResetReason, RunId};
pub use source::{FilterRequest, PathFilter, Sink, SourceError, Tracked, WorktreeSource};
pub use types::{
    Coalesce, Degraded, DirBatch, Failure, FlushError, Health, MetaChanges, RepoBatch, Revision,
    RootState, WorktreeBatch, WorktreeMeta, WorktreeName,
};
pub use watcher::{
    Config, DirSpec, DirWatch, RepoSpec, RepoWatch, Watcher, WorktreeSpec, WorktreeWatch,
};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod send_sync {
    fn assert<T: Send + Sync>() {}

    #[test]
    fn handles_are_send_and_sync() {
        assert::<crate::Watcher>();
        assert::<crate::RepoWatch>();
        assert::<crate::WorktreeWatch>();
        assert::<crate::DirWatch>();
        assert::<crate::EventSink>();
    }
}
