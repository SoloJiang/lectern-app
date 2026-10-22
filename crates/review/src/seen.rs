//! Seen marks and the three seen states (docs/design/review.md「已看状态」).
//!
//! A mark's key holds both sides of the change it was made on; the state of
//! a file is judged from this review target's latest record for the file in
//! the annotation log's index (`log::Index`).

use std::sync::Arc;

use lectern_services::changes::{
    BaseSpec, ChangePath, ChangeStatus, FileChange, FileMode, FileSide, SideIdentity,
};
use lectern_services::review::{FileKey, Progress, SeenState};
use lectern_services::session::Timestamp;

use crate::log::{Index, Record};

#[cfg(test)]
mod tests;

/// The key of a seen mark.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct SeenKey {
    pub target: TargetKey,
    pub old_path: Option<ChangePath>,
    pub new_path: Option<ChangePath>,
    pub old: Option<(FileMode, SideIdentity)>,
    pub new: Option<(FileMode, SideIdentity)>,
    /// `ChangeStatus` without the similarity.
    pub status: StatusKind,
}

/// A review target in the annotation log. A worktree is written as its git
/// directory relative to the common directory ("" for the main worktree,
/// "worktrees/<name>" for the others), so marks survive the repository
/// moving (the log itself is per repository).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) enum TargetKey {
    Session { source: Arc<str>, id: Arc<str> },
    Outside { worktree: Arc<str> },
    Range { worktree: Arc<str>, base: BaseSpec },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum StatusKind {
    Added,
    Deleted,
    Modified,
    TypeChanged,
    Renamed,
    Conflicted,
}

impl SeenKey {
    /// The key of `change` in `target`. A blob that was not retained is the
    /// same content as the blob kept: whether the comparison kept its bytes
    /// depends on the retention budget, not on the content.
    pub(crate) fn new(target: TargetKey, change: &FileChange) -> SeenKey {
        let file = file_key(change);
        let side = |s: &Option<FileSide>| {
            s.as_ref().map(|s| {
                let content = match s.content {
                    SideIdentity::NotRetained(oid) => SideIdentity::Blob(oid),
                    other => other,
                };
                (s.mode, content)
            })
        };
        SeenKey {
            target,
            old_path: file.old,
            new_path: file.new,
            old: side(&change.old),
            new: side(&change.new),
            status: match change.status {
                ChangeStatus::Added => StatusKind::Added,
                ChangeStatus::Deleted => StatusKind::Deleted,
                ChangeStatus::Modified => StatusKind::Modified,
                ChangeStatus::TypeChanged => StatusKind::TypeChanged,
                ChangeStatus::Renamed { .. } => StatusKind::Renamed,
                ChangeStatus::Conflicted => StatusKind::Conflicted,
            },
        }
    }

    pub(crate) fn file(&self) -> FileKey {
        FileKey {
            old: self.old_path.clone(),
            new: self.new_path.clone(),
        }
    }
}

/// The file identity the index keeps one record under: the new path, the
/// old one when there is none.
pub(crate) fn identity(file: &FileKey) -> Option<&ChangePath> {
    file.new.as_ref().or(file.old.as_ref())
}

/// The stable key of a file in a snapshot.
pub(crate) fn file_key(change: &FileChange) -> FileKey {
    let path = Some(change.path.clone());
    match &change.status {
        ChangeStatus::Added => FileKey {
            old: None,
            new: path,
        },
        ChangeStatus::Deleted => FileKey {
            old: path,
            new: None,
        },
        ChangeStatus::Renamed { from, .. } => FileKey {
            old: Some(from.clone()),
            new: path,
        },
        ChangeStatus::Modified | ChangeStatus::TypeChanged | ChangeStatus::Conflicted => FileKey {
            old: path.clone(),
            new: path,
        },
    }
}

/// The state of the change `key` in its review target:
///
/// - `Seen`: the latest record for the file is a `seen` with exactly this
///   key (an `unseen` after it cancels it).
/// - `ChangedSinceSeen`: otherwise, the most recent `seen` record R of the
///   same change C: `C.new_path` is set and equals `R.new_path`, or
///   `C.old_path` is set and equals `R.new_path` (renamed or deleted since
///   it was seen). So a rename matches as an (old, new) pair: an unrelated
///   new file at a renamed or deleted path is not the same change.
/// - `Unseen`: neither.
pub(crate) fn state(index: &Index, key: &SeenKey) -> SeenState {
    let seen = |path: &ChangePath| match index.mark(&key.target, path) {
        Some(Record::Seen { at, key: r }) => Some((*at, r)),
        _ => None,
    };
    let file = key.file();
    if let Some((_, r)) = identity(&file).and_then(seen)
        && r == key
    {
        return SeenState::Seen;
    }
    let same = |path: &Option<ChangePath>| {
        let path = path.as_ref()?;
        seen(path).and_then(|(at, r)| (r.new_path.as_ref() == Some(path)).then_some(at))
    };
    match same(&key.new_path).max(same(&key.old_path)) {
        Some(since) => SeenState::ChangedSinceSeen { since },
        None => SeenState::Unseen,
    }
}

/// The `seen` record that marks `key` as seen at `at`.
pub(crate) fn mark_seen(key: SeenKey, at: Timestamp) -> Record {
    Record::Seen { at, key }
}

/// The `unseen` record that cancels the marks of `file` in `target`.
pub(crate) fn mark_unseen(target: TargetKey, file: FileKey, at: Timestamp) -> Record {
    Record::Unseen { at, target, file }
}

/// Looks good: a `seen` record for each of `keys` that is not `Seen`.
pub(crate) fn looks_good(index: &Index, keys: &[SeenKey], at: Timestamp) -> Vec<Record> {
    keys.iter()
        .filter(|k| state(index, k) != SeenState::Seen)
        .map(|k| mark_seen(k.clone(), at))
        .collect()
}

pub(crate) fn progress(states: &[SeenState]) -> Progress {
    let mut p = Progress {
        total: states.len() as u32,
        ..Progress::default()
    };
    for s in states {
        match s {
            SeenState::Seen => p.seen += 1,
            SeenState::ChangedSinceSeen { .. } => p.changed_since_seen += 1,
            SeenState::Unseen => {}
        }
    }
    p
}
