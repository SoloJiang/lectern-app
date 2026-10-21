//! The only way this crate reaches git: the repository's hash kind,
//! object database blobs and fs_watch's filter source. Production forwards
//! to `lectern-git` and keeps one `git::Repo` per opened repository (it holds
//! no gix handle); tests use a stand-in that counts its calls.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lectern_fs_watch::WorktreeSource;
use lectern_git::{GitError, HashKind, Oid, Repo, WorktreeId};
use lectern_services::project::BlobId;

use crate::util::lock;

/// A worktree, as the calls name it.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Tree {
    pub common_dir: Arc<Path>,
    pub git_dir: Arc<Path>,
    pub root: Arc<Path>,
}

pub(crate) trait GitAccess: Send + Sync {
    /// The repository's hash kind. Blocking.
    fn hash(&self, common_dir: &Path) -> Result<HashKind, GitError>;
    /// One blob, whole; `GitError::TooLarge` past `max`. Blocking.
    fn blob(&self, common_dir: &Path, oid: BlobId, max: u64) -> Result<Arc<[u8]>, GitError>;
    /// fs_watch's filter source for the worktree. Blocking (opens the
    /// worktree).
    fn watch_source(&self, tree: &Tree) -> Result<Arc<dyn WorktreeSource>, GitError>;
    /// Drops what is kept for the repository (the project closed).
    fn forget(&self, common_dir: &Path);
}

/// How [`LecternGit`] discovers a repository.
pub(crate) type DiscoverFn =
    fn(&Path, &[PathBuf]) -> Result<Option<lectern_git::Discovered>, GitError>;

pub(crate) struct LecternGit {
    discover: DiscoverFn,
    repos: Mutex<HashMap<PathBuf, Repo>>,
}

impl LecternGit {
    pub(crate) fn new() -> LecternGit {
        LecternGit::with_discover(Repo::discover)
    }

    pub(crate) fn with_discover(discover: DiscoverFn) -> LecternGit {
        LecternGit {
            discover,
            repos: Mutex::new(HashMap::new()),
        }
    }

    fn repo(&self, common_dir: &Path) -> Result<Repo, GitError> {
        if let Some(repo) = lock(&self.repos).get(common_dir) {
            return Ok(repo.clone());
        }
        let repo = match (self.discover)(common_dir, &[])? {
            Some(found) => found.repo,
            None => {
                return Err(GitError::Corrupt(format!(
                    "{}: no repository",
                    common_dir.display()
                )));
            }
        };
        lock(&self.repos).insert(common_dir.to_path_buf(), repo.clone());
        Ok(repo)
    }
}

impl GitAccess for LecternGit {
    fn hash(&self, common_dir: &Path) -> Result<HashKind, GitError> {
        Ok(self.repo(common_dir)?.location().hash)
    }

    fn blob(&self, common_dir: &Path, oid: BlobId, max: u64) -> Result<Arc<[u8]>, GitError> {
        self.repo(common_dir)?.blob(git_oid(oid), max)
    }

    fn watch_source(&self, tree: &Tree) -> Result<Arc<dyn WorktreeSource>, GitError> {
        let repo = self.repo(&tree.common_dir)?;
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
        Ok(repo.worktree(&id)?.watch_source())
    }

    fn forget(&self, common_dir: &Path) {
        let repo = lock(&self.repos).remove(common_dir);
        drop(repo);
    }
}

/// The hash kind of an OID.
pub(crate) fn hash_kind(oid: &BlobId) -> HashKind {
    match oid {
        BlobId::Sha1(_) => HashKind::Sha1,
        BlobId::Sha256(_) => HashKind::Sha256,
    }
}

/// `lectern-git`'s `Oid` as a `BlobId`: the same bytes.
pub(crate) fn blob_id(oid: &Oid) -> BlobId {
    BlobId::from_bytes(oid.as_bytes()).unwrap_or(BlobId::Sha1([0; 20]))
}

/// A `BlobId` as `lectern-git`'s `Oid`.
pub(crate) fn git_oid(oid: BlobId) -> Oid {
    let kind = hash_kind(&oid);
    Oid::from_hex(kind, &oid.to_string()).unwrap_or_else(|| Oid::for_blob(kind, b""))
}
