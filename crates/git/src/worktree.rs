//! A worktree handle: HEAD, the index and safe reads of its files
//! (docs/design/git.md「index」).

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, PoisonError};

use lectern_platform::{Read, SafeError, SafeRoot};

use crate::handle::Handle;
use crate::repo::read_head;
use crate::tracked::{self, EntryLimits, TrackedPaths};
use crate::{GitError, HeadState, Oid, Repo, RepoPath, WorktreeId, WorktreeInfo};

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
