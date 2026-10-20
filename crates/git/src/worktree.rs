//! A worktree handle: HEAD, the index and safe reads of its files
//! (docs/design/git.md「index」).

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, PoisonError};

use lectern_platform::{Read, SafeError, SafeRoot};

use crate::compare::{self, Base, CompareState, Refresh, Subject};
use crate::handle::Handle;
use crate::repo::read_head;
use crate::tracked::{self, EntryLimits, TrackedPaths};
use crate::{
    ChangeSet, GitError, HeadState, Limits, Oid, Repo, RepoPath, WorktreeId, WorktreeInfo,
};

/// One worktree: its own HEAD, index and files. `Send + Sync`; cloning is
/// an `Arc` clone. Holds the worktree root's `SafeRoot` and the last
/// `TrackedPaths`, never a gix handle. Callers drop it when they leave the
/// worktree.
#[derive(Clone, Debug)]
pub struct Worktree(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    repo: Repo,
    info: WorktreeInfo,
    root: SafeRoot,
    /// Held only within one call, never across calls.
    tracked: Mutex<Option<Arc<TrackedPaths>>>,
    /// The stat cache and conversion baseline of `changes` and `refresh`,
    /// and the index's `.gitattributes` entries for `canonical_oid`. The
    /// index those calls parse is held only within one call.
    compare: Mutex<CompareState>,
}

/// What [`Worktree::read_file`] found. Raw bytes, no conversion.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum WorktreeFile {
    File {
        bytes: Vec<u8>,
    },
    /// The link target, not followed.
    Symlink {
        target: Vec<u8>,
    },
    /// Larger than the caller's limit; nothing was read.
    TooLarge {
        size: u64,
    },
    /// Nothing at that path, or a path component isn't a directory.
    NotFound,
    /// `UnsupportedType` (directory, FIFO, socket, device),
    /// `PathThroughSymlink` or `Unreadable`.
    Unavailable(crate::Unavailable),
}

impl Worktree {
    pub(crate) fn new(repo: Repo, info: WorktreeInfo) -> Result<Worktree, GitError> {
        let root = SafeRoot::open(&info.root)?;
        Ok(Worktree(Arc::new(Inner {
            repo,
            info,
            root,
            tracked: Mutex::new(None),
            compare: Mutex::new(CompareState::default()),
        })))
    }

    pub fn id(&self) -> &WorktreeId {
        &self.0.info.id
    }

    pub fn root(&self) -> &Path {
        &self.0.info.root
    }

    /// The worktree's git directory (`WorktreeInfo::git_dir`).
    pub fn git_dir(&self) -> &Path {
        &self.0.info.git_dir
    }

    /// Reads HEAD again (the HEAD file plus one reference lookup).
    /// `agents` / `usage` call it after a tool call to see the branch
    /// (ADR 0020).
    pub fn head(&self) -> Result<HeadState, GitError> {
        let handle = Handle::open(self.git_dir(), self.0.repo.scope())?;
        read_head(&self.0.repo, &handle, self.git_dir(), self.id())?
            .ok_or_else(|| GitError::Corrupt(format!("{}: no HEAD", self.git_dir().display())))
    }

    /// Every path in the index, in index order (path bytes). Returns the
    /// same `Arc` while the index file is unchanged. Empty without an index
    /// file. Sparse directory entries are expanded in memory within
    /// `limits` (`GitError::TooManyEntries` above it), checking `cancel`.
    pub fn tracked(
        &self,
        limits: &EntryLimits,
        cancel: &AtomicBool,
    ) -> Result<Arc<TrackedPaths>, GitError> {
        let path = self.git_dir().join("index");
        let hash = self.0.repo.hash();
        let current = tracked::checksum(&path, hash)?;
        {
            let cached = self
                .0
                .tracked
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(cached) = cached.as_ref()
                && cached.checksum() == current
            {
                if cached.len() > limits.entries {
                    return Err(GitError::TooManyEntries);
                }
                return Ok(Arc::clone(cached));
            }
        }
        let paths = match current {
            None => TrackedPaths::empty(),
            Some(_) => {
                let open = || Handle::open(self.git_dir(), self.0.repo.scope());
                tracked::read(&path, hash, &open, limits, cancel)?
            }
        };
        let paths = Arc::new(paths);
        *self
            .0
            .tracked
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(&paths));
        Ok(paths)
    }

    /// Reads one file's raw bytes (no conversion), at most `max`, through
    /// `SafeRoot` from the worktree root: no symbolic link is followed at
    /// any level and nothing outside the worktree is read. For `project`'s
    /// manifests and other reads without git semantics.
    pub fn read_file(&self, path: &RepoPath, max: u64) -> Result<WorktreeFile, GitError> {
        use crate::Unavailable;

        let Some(rel) = path.as_rel() else {
            return Ok(WorktreeFile::Unavailable(Unavailable::Unreadable));
        };
        Ok(match self.0.root.read(rel, max) {
            Ok(Read::File { bytes, .. }) => WorktreeFile::File { bytes },
            Ok(Read::Symlink { target, .. }) => WorktreeFile::Symlink { target },
            Ok(Read::TooLarge { meta }) => WorktreeFile::TooLarge { size: meta.size },
            Ok(Read::Unsupported { .. }) => WorktreeFile::Unavailable(Unavailable::UnsupportedType),
            Err(SafeError::NotFound) => WorktreeFile::NotFound,
            Err(SafeError::PathThroughSymlink) => {
                WorktreeFile::Unavailable(Unavailable::PathThroughSymlink)
            }
            Err(SafeError::Io(_)) => WorktreeFile::Unavailable(Unavailable::Unreadable),
        })
    }

    /// The worktree against `base`, untracked files included
    /// (docs/design/git.md「工作区比较」). Writes neither the index nor the
    /// object database, runs no external program and reads worktree files
    /// only through `SafeRoot`; files whose index stat matches (and isn't
    /// racy) aren't read.
    pub fn changes(
        &self,
        base: Base,
        limits: &Limits,
        cancel: &AtomicBool,
    ) -> Result<Arc<ChangeSet>, GitError> {
        compare::run(&self.subject(), &base, limits, cancel, None, None)
    }

    /// A new change set for what `prev` (a `changes` of this worktree)
    /// compared, with the same base and limits. `Refresh::Paths` compares
    /// only those paths again and takes everything else from `prev`; when
    /// HEAD (for `Base::Head`) or the index changed since `prev`, or a path
    /// is a `.gitignore` / `.gitattributes`, it compares everything, as
    /// `Refresh::Full` does. Unchanged worktree contents are shared with
    /// `prev`, not read again.
    pub fn refresh(
        &self,
        prev: &Arc<ChangeSet>,
        what: Refresh<'_>,
        cancel: &AtomicBool,
    ) -> Result<Arc<ChangeSet>, GitError> {
        compare::refresh(&self.subject(), prev, what, cancel)
    }

    /// A `fs_watch` source for this worktree (docs/design/fs_watch.md
    /// 「对其他 crate 的要求」): the filter is the index paths (the same
    /// `Arc` as [`Worktree::tracked`] while the index is unchanged) plus the
    /// ignore rules read once through `SafeRoot`, answered from memory; the
    /// config list comes from the origins of the configuration gix loaded
    /// and every `include` value. Holds this handle; runs no git CLI.
    pub fn watch_source(&self) -> Arc<dyn lectern_fs_watch::WorktreeSource> {
        Arc::new(crate::watch::Source::new(self.clone()))
    }

    /// The blob OID `git add` would store for `bytes` as the regular file at
    /// `path`: git's built-in to-git conversions (`text` / `eol` / `crlf`
    /// and `core.autocrlf`, `ident`, `working-tree-encoding`) under the
    /// attributes and configuration the worktree comparison uses, without
    /// writing the object database. None when a configured clean or process
    /// filter applies to `path` (e.g. Git LFS): no external program is ever
    /// run, and without it the content can't be made canonical. Reads the
    /// index and, through `SafeRoot`, the `.gitattributes` in `path`'s
    /// directories; `bytes` themselves are not read from the worktree. A
    /// conversion that fails (bytes invalid in the declared encoding)
    /// leaves the content raw, as the comparison does. Each call parses
    /// the index once unless this handle already did for the same index:
    /// for many paths use [`canonical_oids`](Self::canonical_oids).
    pub fn canonical_oid(&self, path: &RepoPath, bytes: &[u8]) -> Result<Option<Oid>, GitError> {
        Ok(self.canonical_oids(&[(path, bytes)])?.pop().flatten())
    }

    /// [`canonical_oid`](Self::canonical_oid) for each `(path, bytes)`, in
    /// order, with one gix handle and at most one parse of the index for
    /// the whole batch; each `.gitattributes` is read at most once. A new
    /// handle parses the index (and drops it on return); a handle that
    /// already did, for the same index, answers from the `.gitattributes`
    /// entries it kept and [`tracked`](Self::tracked). An error for one
    /// path fails the batch.
    pub fn canonical_oids(
        &self,
        items: &[(&RepoPath, &[u8])],
    ) -> Result<Vec<Option<Oid>>, GitError> {
        let tracked = || self.tracked(&EntryLimits::default(), &AtomicBool::new(false));
        compare::canonical_oids(&self.subject(), &tracked, items)
    }

    pub(crate) fn safe_root(&self) -> &SafeRoot {
        &self.0.root
    }

    pub(crate) fn repo(&self) -> &Repo {
        &self.0.repo
    }

    pub(crate) fn subject(&self) -> Subject<'_> {
        Subject {
            root: &self.0.root,
            root_path: &self.0.info.root,
            git_dir: &self.0.info.git_dir,
            id: &self.0.info.id,
            repo: &self.0.repo,
            state: &self.0.compare,
        }
    }

    /// Reads only the checksum at the end of the index file: no entries
    /// parsed, no sparse directory expanded. Equal to
    /// `TrackedPaths::checksum`. For checking whether a cache is stale
    /// without rebuilding the file list. None without an index file.
    pub fn index_checksum(&self) -> Result<Option<Oid>, GitError> {
        tracked::checksum(&self.git_dir().join("index"), self.0.repo.hash())
    }
}

#[cfg(test)]
mod tests;
