//! The only way this crate reaches git (docs/design/project.md「依赖 git 的
//! 接口」). The production implementation forwards to `lectern-git`; tests
//! use `testing::FakeGit`. Every call blocks and runs on the executor.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use lectern_fs_watch::{WorktreeName, WorktreeSource};
use lectern_git::{
    EntryLimits, GitError, HashKind, HeadState, Mode, Oid, Repo, RepoPath, WorktreeFile, WorktreeId,
};
use lectern_services::project::{FileKind, FileList, Head};

pub(crate) trait GitAccess: Send + Sync {
    /// Discovers the repository at `path` and upwards, not above
    /// `GIT_CEILING_DIRECTORIES`. `Ok(None)` when there is none.
    fn discover(&self, path: &Path) -> Result<Option<Discovered>, GitError>;

    /// Every worktree of the repository: the main one (none when bare) and
    /// `<common>/worktrees/*`.
    fn worktrees(&self, common_dir: &Path) -> Result<Vec<WorktreeInfo>, GitError>;

    /// Opens one worktree for one scan. Reading the index and the manifests
    /// share this handle; it is dropped when the scan ends, so nothing of a
    /// worktree stays in memory between scans. Fails when the root is gone.
    fn open_worktree(
        &self,
        common_dir: &Path,
        git_dir: &Path,
    ) -> Result<Box<dyn TreeAccess>, GitError>;

    /// The blob OID of `bytes` in the repository's hash algorithm (as
    /// `git hash-object`), the key of the manifest parse cache.
    fn hash_blob(&self, kind: HashKind, bytes: &[u8]) -> Oid;
}

/// One open worktree. The active worktree's handle lives as long as its
/// watch (step 4): its `watch_source` holds the same handle, so the filter
/// and the refreshes share one parsed index.
pub(crate) trait TreeAccess: Send + Sync {
    /// The index's paths as a file list, with the index checksum (None
    /// without an index file). Non-UTF-8 and invalid paths are counted in
    /// `FileList::skipped`.
    fn index_files(&self, cancel: &AtomicBool) -> Result<IndexFiles, GitError>;

    /// Only the index checksum at the end of the file.
    fn index_checksum(&self) -> Result<Option<Oid>, GitError>;

    /// Reads a worktree file without following symbolic links or leaving
    /// the worktree. `Ok(None)` when there is nothing at `rel`;
    /// `Err(GitError::TooLarge)` above `limit`; any other error when it is
    /// not a regular file or cannot be read.
    fn read_file(&self, rel: &str, limit: u64) -> Result<Option<Vec<u8>>, GitError>;

    /// The filter and config source for `fs_watch` (`Worktree::watch_source`
    /// on this same handle). Does no IO; reads only HEAD and config when
    /// fs_watch loads it, not the index.
    fn watch_source(&self) -> Arc<dyn WorktreeSource>;
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Discovered {
    /// The root of the worktree `path` is in; None when `path` is in a git
    /// directory or a bare repository.
    pub worktree_root: Option<PathBuf>,
    /// The git directory of that worktree, or the git directory `path` is
    /// in (the common directory, or `<common>/worktrees/<id>`).
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
    pub bare: bool,
    pub hash: HashKind,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct WorktreeInfo {
    pub root: PathBuf,
    pub git_dir: PathBuf,
    pub is_main: bool,
    pub head: Head,
    pub locked: bool,
    pub prunable: bool,
}

pub(crate) struct IndexFiles {
    pub checksum: Option<Oid>,
    pub files: FileList,
}

/// How `LecternGit` discovers: `Repo::discover` in the app,
/// `lectern_git::testing::discover` (repository configuration only) in
/// tests.
pub(crate) type DiscoverFn =
    fn(&Path, &[PathBuf]) -> Result<Option<lectern_git::Discovered>, GitError>;

/// The production `GitAccess`: `lectern-git`. Holds no repository handle
/// between calls.
pub(crate) struct LecternGit {
    ceilings: Vec<PathBuf>,
    discover: DiscoverFn,
}

impl LecternGit {
    /// Reads `GIT_CEILING_DIRECTORIES` once (git.md「仓库与 worktree」).
    pub(crate) fn from_env() -> LecternGit {
        let ceilings = std::env::var_os("GIT_CEILING_DIRECTORIES")
            .map(|value| {
                std::env::split_paths(&value)
                    .filter(|p| p.is_absolute())
                    .collect()
            })
            .unwrap_or_default();
        LecternGit {
            ceilings,
            discover: Repo::discover,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_discover(discover: DiscoverFn) -> LecternGit {
        LecternGit {
            ceilings: Vec::new(),
            discover,
        }
    }

    fn repo(&self, common_dir: &Path) -> Result<Repo, GitError> {
        match (self.discover)(common_dir, &[])? {
            Some(found) => Ok(found.repo),
            None => Err(not_found(common_dir)),
        }
    }
}

impl GitAccess for LecternGit {
    fn discover(&self, path: &Path) -> Result<Option<Discovered>, GitError> {
        let Some(found) = (self.discover)(path, &self.ceilings)? else {
            return Ok(None);
        };
        let location = found.repo.location();
        let common_dir = location.common_dir.clone();
        let (worktree_root, git_dir) = match found.worktree {
            Some(info) => (Some(info.root), info.git_dir),
            None => {
                // In a git directory: the linked worktree's administrative
                // directory when the path is in one, else the common one.
                let canonical = std::fs::canonicalize(path)?;
                let admin = common_dir.join("worktrees");
                let git_dir = canonical
                    .strip_prefix(&admin)
                    .ok()
                    .and_then(|rest| rest.components().next())
                    .map_or_else(|| common_dir.clone(), |id| admin.join(id));
                (None, git_dir)
            }
        };
        Ok(Some(Discovered {
            worktree_root,
            git_dir,
            common_dir,
            bare: location.bare,
            hash: location.hash,
        }))
    }

    fn worktrees(&self, common_dir: &Path) -> Result<Vec<WorktreeInfo>, GitError> {
        let repo = self.repo(common_dir)?;
        Ok(repo
            .worktrees()?
            .into_iter()
            .map(|info| WorktreeInfo {
                root: info.root,
                git_dir: info.git_dir,
                is_main: info.is_main,
                head: head(&info.head),
                locked: info.locked.is_some(),
                prunable: info.prunable,
            })
            .collect())
    }

    fn open_worktree(
        &self,
        common_dir: &Path,
        git_dir: &Path,
    ) -> Result<Box<dyn TreeAccess>, GitError> {
        let repo = self.repo(common_dir)?;
        let id = if git_dir == repo.location().common_dir {
            WorktreeId::Main
        } else {
            let name = git_dir
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| not_found(git_dir))?;
            WorktreeId::Linked(name.into())
        };
        Ok(Box::new(repo.worktree(&id)?))
    }

    fn hash_blob(&self, kind: HashKind, bytes: &[u8]) -> Oid {
        Oid::for_blob(kind, bytes)
    }
}

impl TreeAccess for lectern_git::Worktree {
    fn index_files(&self, cancel: &AtomicBool) -> Result<IndexFiles, GitError> {
        let tracked = self.tracked(&EntryLimits::default(), cancel)?;
        let mut files = FileList::builder();
        for entry in tracked.iter() {
            let kind = match entry.mode {
                Mode::File => FileKind::File,
                Mode::Executable => FileKind::Executable,
                Mode::Symlink => FileKind::Symlink,
                Mode::Gitlink => FileKind::Submodule,
                Mode::Special => {
                    files.skip();
                    continue;
                }
            };
            match std::str::from_utf8(entry.path.as_bytes()) {
                Ok(path) => files.push(path, kind),
                Err(_) => files.skip(),
            }
        }
        for _ in tracked.invalid_paths() {
            files.skip();
        }
        Ok(IndexFiles {
            checksum: tracked.checksum(),
            files: files.build(),
        })
    }

    fn index_checksum(&self) -> Result<Option<Oid>, GitError> {
        lectern_git::Worktree::index_checksum(self)
    }

    fn read_file(&self, rel: &str, limit: u64) -> Result<Option<Vec<u8>>, GitError> {
        let Some(path) = RepoPath::new(rel.as_bytes()) else {
            return Ok(None);
        };
        match lectern_git::Worktree::read_file(self, &path, limit)? {
            WorktreeFile::File { bytes } => Ok(Some(bytes)),
            WorktreeFile::NotFound => Ok(None),
            WorktreeFile::TooLarge { size } => Err(GitError::TooLarge { size }),
            WorktreeFile::Symlink { .. } => Err(GitError::Corrupt(format!("{rel} is a symlink"))),
            WorktreeFile::Unavailable(why) => Err(GitError::Corrupt(format!("{rel}: {why:?}"))),
        }
    }

    fn watch_source(&self) -> Arc<dyn WorktreeSource> {
        lectern_git::Worktree::watch_source(self)
    }
}

/// The `fs_watch` name of the worktree whose git directory is `git_dir`:
/// `Main` for the common directory, else `Linked(<id>)`, the directory name
/// under `<common>/worktrees/` (as `LecternGit::open_worktree` maps it).
pub(crate) fn worktree_name(common_dir: &Path, git_dir: &Path) -> Option<WorktreeName> {
    if git_dir == common_dir {
        return Some(WorktreeName::Main);
    }
    let name = git_dir.file_name()?.to_str()?;
    Some(WorktreeName::Linked(name.into()))
}

fn head(state: &HeadState) -> Head {
    match state {
        HeadState::Branch {
            name,
            commit: Some(_),
        } => Head::Branch(name.as_str().into()),
        HeadState::Branch { name, commit: None } => Head::Unborn(name.as_str().into()),
        HeadState::Detached { commit } => Head::Detached(commit.to_hex().into()),
    }
}

fn not_found(path: &Path) -> GitError {
    GitError::Io(io::Error::new(
        io::ErrorKind::NotFound,
        format!("no repository at {}", path.display()),
    ))
}
