//! What the runtime log keeps of this crate's values
//! (docs/design/fs_watch.md「日志与诊断」, docs/design/logging.md): the
//! watched root through `Value::path`, the kind of watch and durations.
//! Never the paths in a batch, reflog lines (they carry ref names, commit
//! messages and command text), a sink's panic message or file contents.

use crate::types::{DirBatch, RepoBatch, WorktreeBatch};

/// The kind of watch a batch belongs to, as the `watch` field.
pub(crate) trait Watched {
    const KIND: &'static str;
}

impl Watched for RepoBatch {
    const KIND: &'static str = "repo";
}

impl Watched for WorktreeBatch {
    const KIND: &'static str = "worktree";
}

impl Watched for DirBatch {
    const KIND: &'static str = "dir";
}
