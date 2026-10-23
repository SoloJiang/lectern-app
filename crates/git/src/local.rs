//! The change source of a local worktree (docs/design/review.md「改动来源」,
//! prerequisite C): [`LocalChanges`] implements
//! `services::changes::ChangeSource` over [`Worktree::changes`] and
//! [`Worktree::refresh`], converting each [`ChangeSet`] field by field as the
//! mapping table there says. The list's handle holds the `ChangeSet`, so the
//! worktree contents it kept live as long as the `ChangeList`.

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use lectern_services::changes::{
    BaseSpec, Capabilities, ChangeDetail, ChangeError, ChangeList, ChangePath, ChangeSource,
    ChangeStatus, ChangeTotals, DiffHunk, FileChange, FileMode, FileSide, ListHandle,
    RawIdentity as SvcRaw, RefreshHint, SideIdentity, SideText as SvcText, SideTexts, SourceInfo,
    SourceKind, UnavailableKind,
};
use lectern_services::project::{BlobId, ProjectKey, WorktreeKey};

use crate::changes::RawText;
use crate::{
    Base, Change, ChangeSet, Content, Detail, GitError, HashKind, Limits, Mode, Oid, RawIdentity,
    RefName, Refresh, Repo, RepoPath, Side, Staged, Status, Totals, Unavailable, Worktree,
};

/// Tells the lists of one `LocalChanges` from those of another: `refresh`
/// and `texts` only use a handle their own source put there.
static NEXT_SOURCE: AtomicU64 = AtomicU64::new(1);

/// A local worktree's changes against a base, as a `ChangeSource`. Both
/// capabilities (`refresh`, `fingerprint`) are there. `review` builds one
/// per review; it isn't a kernel service (ADR 0025, #47).
pub struct LocalChanges {
    repo: Repo,
    worktree: Worktree,
    base: BaseSpec,
    limits: Limits,
    info: SourceInfo,
    source: u64,
    generation: AtomicU64,
}

/// What `ChangeList::handle` holds for a list of this source.
struct Held {
    source: u64,
    set: Arc<ChangeSet>,
}

impl LocalChanges {
    /// No IO. `project` and `key` only fill `SourceInfo`; `base` is resolved
    /// on every `fetch`, as `Worktree::changes` does.
    pub fn new(
        repo: Repo,
        worktree: Worktree,
        project: ProjectKey,
        key: WorktreeKey,
        base: BaseSpec,
        limits: Limits,
    ) -> LocalChanges {
        let info = SourceInfo {
            kind: SourceKind::Worktree {
                project,
                worktree: key,
                base: base.clone(),
            },
            caps: Capabilities {
                refresh: true,
                fingerprint: true,
            },
        };
        LocalChanges {
            repo,
            worktree,
            base,
            limits,
            info,
            source: NEXT_SOURCE.fetch_add(1, Ordering::Relaxed),
            generation: AtomicU64::new(0),
        }
    }

    /// The `ChangeSet` behind `list` when this source made it.
    fn held<'a>(&self, list: &'a ChangeList) -> Option<&'a Arc<ChangeSet>> {
        let any: &(dyn Any + Send + Sync) = list.handle.0.as_deref()?;
        let held = any.downcast_ref::<Held>()?;
        (held.source == self.source).then_some(&held.set)
    }

    fn git_base(&self) -> Result<Base, ChangeError> {
        Ok(match &self.base {
            BaseSpec::Head => Base::Head,
            BaseSpec::Commit(id) => Base::Commit(
                oid(id).ok_or_else(|| ChangeError::Io(format!("invalid object ID {id}").into()))?,
            ),
            BaseSpec::MergeBase(name) => Base::MergeBase(
                RefName::new(name)
                    .ok_or_else(|| ChangeError::Io(format!("invalid reference {name}").into()))?,
            ),
        })
    }

    /// Converts `set`. A file equal to one of `prev` (the list `set` was
    /// refreshed from) takes its `Detail` from there, sharing the hunks'
    /// `Arc`; its paths are always `set`'s own.
    fn list(&self, set: Arc<ChangeSet>, prev: Option<(&ChangeList, &ChangeSet)>) -> ChangeList {
        let files: Arc<[FileChange]> = set
            .files()
            .iter()
            .map(|change| {
                let detail = prev.and_then(|(list, prev)| unchanged_detail(change, list, prev));
                file_change(change, detail)
            })
            .collect();
        ChangeList {
            generation: self.generation.fetch_add(1, Ordering::Relaxed) + 1,
            base: blob_id(set.base_tree()),
            files,
            totals: totals(set.totals()),
            handle: ListHandle(Some(Arc::new(Held {
                source: self.source,
                set,
            }))),
        }
    }
}

impl ChangeSource for LocalChanges {
    fn info(&self) -> &SourceInfo {
        &self.info
    }

    fn fetch(&self, cancel: &AtomicBool) -> Result<Arc<ChangeList>, ChangeError> {
        let set = self
            .worktree
            .changes(self.git_base()?, &self.limits, cancel)
            .map_err(change_error)?;
        Ok(Arc::new(self.list(set, None)))
    }

    /// A list this source didn't make (no handle, or another source's) is
    /// compared again in full, as `fetch`.
    fn refresh(
        &self,
        prev: &Arc<ChangeList>,
        hint: &RefreshHint,
        cancel: &AtomicBool,
    ) -> Result<Arc<ChangeList>, ChangeError> {
        let Some(prev_set) = self.held(prev) else {
            return self.fetch(cancel);
        };
        // A path that breaks the `RepoPath` invariants names nothing in the
        // worktree; compare everything rather than drop it.
        let paths: Option<Vec<RepoPath>> = match hint {
            RefreshHint::Paths(paths) => paths.iter().map(|p| RepoPath::new(&p.0)).collect(),
            RefreshHint::Full => None,
        };
        let what = match &paths {
            Some(paths) => Refresh::Paths(paths),
            None => Refresh::Full,
        };
        let set = self
            .worktree
            .refresh(prev_set, what, cancel)
            .map_err(change_error)?;
        Ok(Arc::new(self.list(set, Some((prev, prev_set)))))
    }

    /// The worktree side is the content the comparison read, the same `Arc`
    /// as `Text::shared()`; the base side is read by OID up to `max_bytes`.
    fn texts(
        &self,
        list: &ChangeList,
        file: u32,
        max_bytes: u64,
    ) -> Result<SideTexts, ChangeError> {
        let set = self.held(list).ok_or_else(|| {
            ChangeError::Io("the change list is not from this change source".into())
        })?;
        let (old, new) = set
            .raw_texts(&self.repo, file as usize, max_bytes, max_bytes)
            .map_err(change_error)?;
        Ok(SideTexts {
            old: side_text(old),
            new: side_text(new),
        })
    }

    /// `ChangeSet::fingerprint` (crates/git/AGENTS.md「边界」); how Analyze
    /// uses it is #66's. None for a list this source didn't make.
    fn fingerprint(&self, list: &ChangeList) -> Option<[u8; 32]> {
        self.held(list).map(|set| *set.fingerprint().as_bytes())
    }
}

/// The previous list's `detail` for `change` when the git changes are
/// equal. `refresh` shares an unchanged `Change` with `prev`, so the
/// comparison of the hunks is usually a pointer comparison.
fn unchanged_detail(change: &Change, list: &ChangeList, prev: &ChangeSet) -> Option<ChangeDetail> {
    let key = (change.path.as_bytes(), change.untracked);
    let i = prev
        .files()
        .binary_search_by(|c| (c.path.as_bytes(), c.untracked).cmp(&key))
        .ok()?;
    (prev.files()[i] == *change)
        .then(|| list.files.get(i).map(|f| f.detail.clone()))
        .flatten()
}

/// `change` as the `services` type, its detail from the change itself (a
/// comparison with zero `total_patch_bytes` lists every file). For a
/// caller that keys files by status, path, sides and mode without building
/// a `ChangeList` (review's overview, review design「概览」). No IO.
pub fn file_change_of(change: &Change) -> FileChange {
    file_change(change, None)
}

/// `detail`, when given, is the previous list's for an equal change.
fn file_change(change: &Change, detail: Option<ChangeDetail>) -> FileChange {
    FileChange {
        status: match &change.status {
            Status::Added => ChangeStatus::Added,
            Status::Deleted => ChangeStatus::Deleted,
            Status::Modified => ChangeStatus::Modified,
            Status::TypeChanged => ChangeStatus::TypeChanged,
            Status::Renamed { from, similarity } => ChangeStatus::Renamed {
                from: path(from),
                similarity: *similarity,
            },
            Status::Conflicted => ChangeStatus::Conflicted,
        },
        path: path(&change.path),
        old: change.old.as_ref().map(side),
        new: change.new.as_ref().map(side),
        detail: detail.unwrap_or_else(|| self::detail(&change.detail)),
        generated: change.generated,
        untracked: change.untracked,
        staged: change.staged.map(|Staged { oid, mode }| FileSide {
            mode: file_mode(mode),
            content: SideIdentity::Blob(blob_id(oid)),
        }),
    }
}

fn detail(detail: &Detail) -> ChangeDetail {
    match detail {
        Detail::Text(diff) => ChangeDetail::Text {
            hunks: diff
                .hunks
                .iter()
                .map(|h| DiffHunk {
                    old: h.old.clone(),
                    new: h.new.clone(),
                    old_byte: h.old_byte,
                    new_byte: h.new_byte,
                })
                .collect(),
            added: diff.added,
            removed: diff.removed,
        },
        Detail::Truncated { added, removed } => ChangeDetail::Truncated {
            added: *added,
            removed: *removed,
        },
        Detail::Binary { old_size, new_size } => ChangeDetail::Binary {
            old_size: *old_size,
            new_size: *new_size,
        },
        Detail::TooLarge => ChangeDetail::TooLarge,
        Detail::Listed => ChangeDetail::Listed,
        Detail::ModeOnly => ChangeDetail::ModeOnly,
        Detail::StagedOnly => ChangeDetail::StagedOnly,
        Detail::NoDiff => ChangeDetail::NoDiff,
    }
}

/// The same `Arc`, not a copy.
fn path(path: &RepoPath) -> ChangePath {
    ChangePath(Arc::clone(path.shared()))
}

fn side(side: &Side) -> FileSide {
    FileSide {
        mode: file_mode(side.mode),
        content: match &side.content {
            Content::Blob(oid) => SideIdentity::Blob(blob_id(*oid)),
            Content::Unhashed {
                size,
                mtime_ns,
                inode,
            } => SideIdentity::Unhashed {
                size: *size,
                mtime_ns: narrow(*mtime_ns),
                inode: *inode,
            },
            Content::Unavailable(unavailable) => match unavailable {
                Unavailable::Filtered { raw } => SideIdentity::Filtered(match *raw {
                    RawIdentity::Sha256(digest) => SvcRaw::Sha256(digest),
                    RawIdentity::Stat {
                        size,
                        mtime_ns,
                        inode,
                    } => SvcRaw::Stat {
                        size,
                        mtime_ns: narrow(mtime_ns),
                        inode,
                    },
                }),
                Unavailable::NotRetained { oid } => SideIdentity::NotRetained(blob_id(*oid)),
                Unavailable::UnsupportedType => {
                    SideIdentity::Unavailable(UnavailableKind::UnsupportedType)
                }
                Unavailable::PathThroughSymlink => {
                    SideIdentity::Unavailable(UnavailableKind::PathThroughSymlink)
                }
                Unavailable::NestedRepository => {
                    SideIdentity::Unavailable(UnavailableKind::NestedRepository)
                }
                Unavailable::Missing => SideIdentity::Unavailable(UnavailableKind::Missing),
                Unavailable::Unreadable => SideIdentity::Unavailable(UnavailableKind::Unreadable),
            },
        },
    }
}

fn file_mode(mode: Mode) -> FileMode {
    match mode {
        Mode::File => FileMode::File,
        Mode::Executable => FileMode::Executable,
        Mode::Symlink => FileMode::Symlink,
        Mode::Gitlink => FileMode::Gitlink,
        Mode::Special => FileMode::Special,
    }
}

/// Nanoseconds since the epoch fit `i64` until 2262; saturates beyond.
fn narrow(ns: i128) -> i64 {
    i64::try_from(ns).unwrap_or(if ns < 0 { i64::MIN } else { i64::MAX })
}

fn blob_id(oid: Oid) -> BlobId {
    let bytes = oid.as_bytes();
    match oid.kind() {
        HashKind::Sha1 => {
            let mut b = [0u8; 20];
            b.copy_from_slice(bytes);
            BlobId::Sha1(b)
        }
        HashKind::Sha256 => {
            let mut b = [0u8; 32];
            b.copy_from_slice(bytes);
            BlobId::Sha256(b)
        }
    }
}

fn oid(id: &BlobId) -> Option<Oid> {
    match id {
        BlobId::Sha1(b) => Oid::from_bytes(HashKind::Sha1, b),
        BlobId::Sha256(b) => Oid::from_bytes(HashKind::Sha256, b),
    }
}

fn totals(t: &Totals) -> ChangeTotals {
    let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    ChangeTotals {
        files: count(t.files),
        added: t.added,
        removed: t.removed,
        truncated: count(t.truncated),
        too_large: count(t.too_large),
        listed: count(t.listed),
        not_retained: count(t.not_retained),
        untracked_capped: t.untracked_capped,
        invalid_paths: Arc::clone(&t.invalid_paths),
    }
}

fn side_text(side: RawText) -> SvcText {
    match side {
        RawText::Absent => SvcText::Absent,
        RawText::Bytes(bytes) => SvcText::Text(bytes),
        RawText::TooLarge { size } => SvcText::TooLarge { size },
        RawText::NotRetained => SvcText::NotRetained,
        RawText::Unavailable => SvcText::Unavailable,
    }
}

/// `Unsupported`, `NoMergeBase`, `TooManyEntries` and `Cancelled` keep their
/// names; the rest are `Io` with the reason.
fn change_error(err: GitError) -> ChangeError {
    match err {
        GitError::Unsupported(why) => ChangeError::Unsupported(why.to_string().into()),
        GitError::NoMergeBase => ChangeError::NoMergeBase,
        GitError::TooManyEntries => ChangeError::TooManyEntries,
        GitError::Cancelled => ChangeError::Cancelled,
        other @ (GitError::ObjectMissing(_)
        | GitError::Corrupt(_)
        | GitError::Io(_)
        | GitError::TooLarge { .. }) => ChangeError::Io(other.to_string().into()),
    }
}

#[cfg(test)]
mod tests;
