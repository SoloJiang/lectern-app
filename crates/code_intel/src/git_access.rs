//! The only way this crate reaches git (docs/design/code_intel.md「依赖 git、
//! fs_watch 与 cache 的接口」). The production implementation forwards to
//! `lectern-git`; tests use `testing::FakeGit`. Every call blocks and runs on
//! the executor; each opens its own handle and drops it on return, so no
//! `git::Worktree` or `TrackedPaths` is held between calls.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};

use lectern_fs_watch::{PathFilter, SourceError, WatchPath, WorktreeSource};
use lectern_git::{
    Base, EntryLimits, GitError, HashKind, HeadState, Limits, Mode, Oid, Repo, RepoPath, Status,
    Worktree, WorktreeId,
};
use lectern_services::project::{BlobId, RelPath};

/// One worktree, as every call names it: the common directory (which
/// repository), the worktree's git directory (which worktree) and its root.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Tree {
    pub common_dir: Arc<Path>,
    pub git_dir: Arc<Path>,
    pub root: Arc<Path>,
}

pub(crate) trait GitAccess: Send + Sync {
    /// The paths of the worktree whose content may differ from the index
    /// or from HEAD: `Worktree::changes(Base::Head, limits)` with
    /// `total_files = 0`, `total_patch_bytes = 0`, `retained_bytes = 0`, so
    /// no line diff and no retained content, only each change's paths (and a
    /// rename's source).
    fn changed_paths(&self, tree: &Tree, cancel: &AtomicBool) -> Result<ChangedPaths, GitError>;

    /// Another worktree's index entries (tier 5): the regular and
    /// executable files with UTF-8 paths, in index order.
    fn tracked(&self, tree: &Tree, cancel: &AtomicBool)
    -> Result<Vec<(RelPath, BlobId)>, GitError>;

    /// One blob from the object database (`TextOrigin::Object`).
    fn blob(&self, tree: &Tree, oid: BlobId, max: u64) -> Result<Arc<[u8]>, GitError>;

    /// The worktree's HEAD commit; None when unborn. The HEAD input's
    /// generation follows it.
    fn head(&self, tree: &Tree) -> Result<Option<BlobId>, GitError>;

    /// The checksum at the end of the index file, entries not parsed. The
    /// index input's generation follows it.
    fn index_checksum(&self, tree: &Tree) -> Result<Option<BlobId>, GitError>;

    /// The blob OID `git add` would give `raw` at `path` after the built-in
    /// conversions; None when a clean or process filter applies. Called only
    /// when the raw bytes' OID is not the expected one.
    fn canonical_oid(
        &self,
        tree: &Tree,
        path: &str,
        raw: &[u8],
    ) -> Result<Option<BlobId>, GitError>;

    /// fs_watch's filter and config source for the worktree. No IO: the
    /// handle is opened on the first `filter` call, which happens only when
    /// this crate's subscription created the worktree state.
    fn watch_source(&self, tree: &Tree) -> Arc<dyn WorktreeSource>;
}

/// What [`GitAccess::changed_paths`] found.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(crate) struct ChangedPaths {
    /// Byte order, deduplicated: every change's path, plus a rename's
    /// source.
    pub paths: Vec<RelPath>,
    /// The paths that differ from HEAD with their OID in HEAD (None when
    /// HEAD does not have them): the base side of every change. Includes
    /// changes only staged (the worktree equals the index, not HEAD).
    pub head: Vec<(RelPath, Option<BlobId>)>,
    pub untracked_capped: bool,
}

/// `lectern-git`'s `Oid` as a `BlobId`: the same bytes.
pub(crate) fn blob_id(oid: &Oid) -> BlobId {
    BlobId::from_bytes(oid.as_bytes()).expect("an OID has 20 or 32 bytes")
}

/// A `BlobId` as `lectern-git`'s `Oid`.
pub(crate) fn git_oid(oid: BlobId) -> Oid {
    let (kind, bytes) = match &oid {
        BlobId::Sha1(b) => (HashKind::Sha1, &b[..]),
        BlobId::Sha256(b) => (HashKind::Sha256, &b[..]),
    };
    Oid::from_hex(kind, &hex(bytes)).expect("a BlobId is a valid OID")
}

/// The hash algorithm of an OID.
pub(crate) fn hash_kind(oid: &BlobId) -> HashKind {
    match oid {
        BlobId::Sha1(_) => HashKind::Sha1,
        BlobId::Sha256(_) => HashKind::Sha256,
    }
}

/// The git blob OID of `bytes` (`git hash-object`).
pub(crate) fn hash_blob(kind: HashKind, bytes: &[u8]) -> BlobId {
    blob_id(&Oid::for_blob(kind, bytes))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn rel(path: &RepoPath) -> Option<RelPath> {
    std::str::from_utf8(path.as_bytes()).ok().map(Arc::from)
}

/// How [`LecternGit`] discovers a repository: `Repo::discover` in the app,
/// `lectern_git::testing::discover` (repository configuration only) in
/// tests.
pub(crate) type DiscoverFn =
    fn(&Path, &[PathBuf]) -> Result<Option<lectern_git::Discovered>, GitError>;

/// The production [`GitAccess`]: `lectern-git`. Holds no handle between
/// calls.
pub(crate) struct LecternGit {
    discover: DiscoverFn,
}

impl LecternGit {
    pub(crate) fn new() -> LecternGit {
        LecternGit {
            discover: Repo::discover,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_discover(discover: DiscoverFn) -> LecternGit {
        LecternGit { discover }
    }

    fn worktree(&self, tree: &Tree) -> Result<Worktree, GitError> {
        open_worktree(self.discover, tree)
    }
}

fn open_worktree(discover: DiscoverFn, tree: &Tree) -> Result<Worktree, GitError> {
    let repo = match discover(&tree.common_dir, &[])? {
        Some(found) => found.repo,
        None => {
            return Err(GitError::Corrupt(format!(
                "{}: no repository",
                tree.common_dir.display()
            )));
        }
    };
    let id = if *tree.git_dir == *repo.location().common_dir {
        WorktreeId::Main
    } else {
        let name = tree
            .git_dir
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| GitError::Corrupt(format!("{}", tree.git_dir.display())))?;
        WorktreeId::Linked(name.into())
    };
    repo.worktree(&id)
}

impl GitAccess for LecternGit {
    fn changed_paths(&self, tree: &Tree, cancel: &AtomicBool) -> Result<ChangedPaths, GitError> {
        let worktree = self.worktree(tree)?;
        let limits = Limits {
            total_files: 0,
            total_patch_bytes: 0,
            retained_bytes: 0,
            ..Limits::default()
        };
        let set = worktree.changes(Base::Head, &limits, cancel)?;
        let mut paths = Vec::with_capacity(set.files().len());
        let mut head = Vec::with_capacity(set.files().len());
        for change in set.files() {
            let Some(path) = rel(&change.path) else {
                continue;
            };
            let base = change.old.as_ref().and_then(|s| s.content.oid());
            if let Status::Renamed { from, .. } = &change.status {
                if let Some(from) = rel(from) {
                    paths.push(from.clone());
                    head.push((from, base.as_ref().map(blob_id)));
                }
                head.push((path.clone(), None));
            } else {
                head.push((path.clone(), base.as_ref().map(blob_id)));
            }
            paths.push(path);
        }
        paths.sort();
        paths.dedup();
        head.sort();
        head.dedup_by(|a, b| a.0 == b.0);
        Ok(ChangedPaths {
            paths,
            head,
            untracked_capped: set.totals().untracked_capped,
        })
    }

    fn tracked(
        &self,
        tree: &Tree,
        cancel: &AtomicBool,
    ) -> Result<Vec<(RelPath, BlobId)>, GitError> {
        let worktree = self.worktree(tree)?;
        let tracked = worktree.tracked(&EntryLimits::default(), cancel)?;
        Ok(tracked
            .iter()
            .filter(|e| matches!(e.mode, Mode::File | Mode::Executable))
            .filter_map(|e| Some((rel(&e.path)?, blob_id(&e.oid))))
            .collect())
    }

    fn blob(&self, tree: &Tree, oid: BlobId, max: u64) -> Result<Arc<[u8]>, GitError> {
        let repo = match (self.discover)(&tree.common_dir, &[])? {
            Some(found) => found.repo,
            None => return Err(GitError::ObjectMissing(git_oid(oid))),
        };
        repo.blob(git_oid(oid), max)
    }

    fn head(&self, tree: &Tree) -> Result<Option<BlobId>, GitError> {
        Ok(match self.worktree(tree)?.head()? {
            HeadState::Branch { commit, .. } => commit.as_ref().map(blob_id),
            HeadState::Detached { commit } => Some(blob_id(&commit)),
        })
    }

    fn index_checksum(&self, tree: &Tree) -> Result<Option<BlobId>, GitError> {
        Ok(self.worktree(tree)?.index_checksum()?.as_ref().map(blob_id))
    }

    fn canonical_oid(
        &self,
        tree: &Tree,
        path: &str,
        raw: &[u8],
    ) -> Result<Option<BlobId>, GitError> {
        let Some(path) = RepoPath::new(path.as_bytes()) else {
            return Ok(None);
        };
        Ok(self
            .worktree(tree)?
            .canonical_oid(&path, raw)?
            .as_ref()
            .map(blob_id))
    }

    fn watch_source(&self, tree: &Tree) -> Arc<dyn WorktreeSource> {
        Arc::new(LazySource {
            discover: self.discover,
            tree: tree.clone(),
            source: OnceLock::new(),
        })
    }
}

/// Opens the worktree only when fs_watch first loads the filter: normally
/// `project`'s subscription created the worktree state and this source is
/// never called (fs_watch「监听器与句柄」).
struct LazySource {
    discover: DiscoverFn,
    tree: Tree,
    source: OnceLock<Arc<dyn WorktreeSource>>,
}

impl LazySource {
    fn source(&self) -> Result<&Arc<dyn WorktreeSource>, SourceError> {
        if let Some(source) = self.source.get() {
            return Ok(source);
        }
        let worktree =
            open_worktree(self.discover, &self.tree).map_err(|e| SourceError(e.to_string()))?;
        Ok(self.source.get_or_init(|| worktree.watch_source()))
    }
}

impl WorktreeSource for LazySource {
    fn filter(
        &self,
        gitignores: &[WatchPath],
        cancel: &AtomicBool,
    ) -> Result<Arc<dyn PathFilter>, SourceError> {
        self.source()?.filter(gitignores, cancel)
    }

    fn config_files(&self, cancel: &AtomicBool) -> Result<Vec<PathBuf>, SourceError> {
        self.source()?.config_files(cancel)
    }
}
