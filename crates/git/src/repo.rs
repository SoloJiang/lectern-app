//! Discovering and opening repositories, listing worktrees
//! (docs/design/git.md「仓库与 worktree」).

use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::error::gix_err;
use crate::handle::{ConfigScope, Handle};
use crate::{GitError, HashKind, Oid, RefName, Worktree, format, gitdir};

/// Where a repository lives and how it is stored.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Location {
    /// The git common directory, absolute and canonical. `cache` tells
    /// repositories apart by it; all worktrees of a repository share it.
    pub common_dir: PathBuf,
    pub hash: HashKind,
    /// A bare repository: no main worktree (it may still have linked ones).
    pub bare: bool,
}

/// The result of [`Repo::discover`].
#[derive(Clone, Debug)]
pub struct Discovered {
    pub repo: Repo,
    /// The worktree `path` is in; None when `path` is in a bare repository or
    /// in a git directory.
    pub worktree: Option<WorktreeInfo>,
}

/// One repository (a git common directory). All its worktrees share one
/// `Repo`. Cloning is an `Arc` clone; no gix handle is held.
#[derive(Clone, Debug)]
pub struct Repo(Arc<RepoInner>);

#[derive(Debug)]
struct RepoInner {
    location: Location,
    scope: ConfigScope,
}

/// A tagged ID: the main worktree and a linked worktree that happens to be
/// called `main` (`<common>/worktrees/main`) never compare equal. `Main`
/// sorts first, linked worktrees by name bytes.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum WorktreeId {
    Main,
    /// The directory name under `<common>/worktrees/`, as is.
    Linked(Arc<str>),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WorktreeInfo {
    pub id: WorktreeId,
    /// The same as `id == WorktreeId::Main`, for callers' convenience.
    pub is_main: bool,
    /// The worktree root. Canonical when it exists; when prunable, the path
    /// recorded in the administrative `gitdir` file without its trailing
    /// `/.git`, normalized lexically without touching the file system.
    pub root: PathBuf,
    /// The common directory for the main worktree,
    /// `<common>/worktrees/<name>` for a linked one.
    pub git_dir: PathBuf,
    pub head: HeadState,
    /// The reason in the `locked` file; `Some("")` for an empty file.
    pub locked: Option<String>,
    /// The worktree root no longer exists.
    pub prunable: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum HeadState {
    /// `commit` is None when the branch has no commit yet (unborn).
    Branch {
        name: RefName,
        commit: Option<Oid>,
    },
    Detached {
        commit: Oid,
    },
}

impl Repo {
    /// Looks for a repository at `path` and upwards, never above a directory
    /// in `ceilings` (the semantics of `GIT_CEILING_DIRECTORIES`). Returns
    /// `Ok(None)` when there is none: a plain folder is a project too
    /// (ADR 0020). Reads no environment variable; `project` / `app` read
    /// `GIT_CEILING_DIRECTORIES` once and pass the parsed list.
    pub fn discover(path: &Path, ceilings: &[PathBuf]) -> Result<Option<Discovered>, GitError> {
        Repo::discover_scoped(path, ceilings, ConfigScope::User)
    }

    pub(crate) fn discover_scoped(
        path: &Path,
        ceilings: &[PathBuf],
        scope: ConfigScope,
    ) -> Result<Option<Discovered>, GitError> {
        let start = std::fs::canonicalize(path)?;
        let start_dir = if start.is_dir() {
            start.clone()
        } else {
            start
                .parent()
                .map_or_else(|| start.clone(), Path::to_path_buf)
        };
        // gix compares ceilings with the physical path it walks.
        let ceilings: Vec<PathBuf> = ceilings
            .iter()
            .map(|c| std::fs::canonicalize(c).unwrap_or_else(|_| c.clone()))
            .collect();
        let options = gix::discover::upwards::Options {
            ceiling_dirs: ceilings.clone(),
            match_ceiling_dir_or_error: false,
            ..Default::default()
        };
        let found = match gix::discover::upwards_opts(&start_dir, options) {
            Ok((found, _trust)) => found,
            Err(err) if err.is_not_found() => return Ok(None),
            Err(err) => return Err(GitError::from_gix(err.into_error())),
        };
        let (git_dir, found_worktree) = found.into_repository_and_work_tree_directories();
        // gix still probes the ceiling directory itself; git never moves up
        // into it (it only checks a ceiling it starts in).
        let found_at = std::fs::canonicalize(found_worktree.as_deref().unwrap_or(&git_dir))?;
        if ceilings
            .iter()
            .any(|c| *c == found_at && start_dir != *c && start_dir.starts_with(c))
        {
            return Ok(None);
        }
        let git_dir = std::fs::canonicalize(&git_dir)?;
        let common_dir = match read_plain_file(&git_dir.join("commondir"))? {
            Some(rel) => std::fs::canonicalize(git_dir.join(rel))?,
            None => git_dir.clone(),
        };
        format::check(&common_dir)?;
        let handle = Handle::open(&common_dir, scope)?;
        let repo = Repo(Arc::new(RepoInner {
            location: Location {
                common_dir: common_dir.clone(),
                hash: HashKind::from_gix(handle.object_hash()),
                bare: handle.workdir().is_none(),
            },
            scope,
        }));
        let id = if git_dir == common_dir {
            Some(WorktreeId::Main)
        } else if git_dir.parent() == Some(common_dir.join("worktrees").as_path()) {
            git_dir
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| WorktreeId::Linked(n.into()))
        } else {
            None
        };
        let in_git_dir = start.starts_with(&git_dir) || start.starts_with(&common_dir);
        let worktree = match id {
            Some(id) if !in_git_dir => repo
                .info(&handle, &id)?
                .filter(|info| !info.prunable && start.starts_with(&info.root)),
            _ => None,
        };
        drop(handle);
        Ok(Some(Discovered { repo, worktree }))
    }

    pub fn location(&self) -> &Location {
        &self.0.location
    }

    pub(crate) fn scope(&self) -> ConfigScope {
        self.0.scope
    }

    pub(crate) fn hash(&self) -> HashKind {
        self.0.location.hash
    }

    /// A handle on the common directory, for repository-level calls.
    pub(crate) fn open_common(&self) -> Result<Handle, GitError> {
        Handle::open(&self.0.location.common_dir, self.0.scope)
    }

    /// The main worktree and every linked worktree, sorted by id. Reads the
    /// administrative files and each HEAD, not the worktrees themselves.
    pub fn worktrees(&self) -> Result<Vec<WorktreeInfo>, GitError> {
        let handle = self.open_common()?;
        let mut out = Vec::new();
        if let Some(main) = self.info(&handle, &WorktreeId::Main)? {
            out.push(main);
        }
        let dir = self.0.location.common_dir.join("worktrees");
        let Some(entries) = gitdir::read_dir(&dir)? else {
            return Ok(out);
        };
        for entry in entries {
            let entry = entry?;
            // git names worktrees after directory basenames; a name that
            // isn't UTF-8 can't be a `WorktreeId` and is left out.
            let Some(name) = entry.file_name().to_str().map(Arc::<str>::from) else {
                continue;
            };
            if !entry.file_type()?.is_dir() {
                continue;
            }
            if let Some(info) = self.info(&handle, &WorktreeId::Linked(name))? {
                out.push(info);
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    /// Opens a worktree: reads its HEAD and configuration; the index is read
    /// only when needed. A prunable worktree (root gone) can't be opened.
    pub fn worktree(&self, id: &WorktreeId) -> Result<Worktree, GitError> {
        let info = {
            let handle = self.open_common()?;
            self.info(&handle, id)?
        };
        let Some(info) = info else {
            return Err(GitError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no worktree {id:?}"),
            )));
        };
        if info.prunable {
            return Err(GitError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                format!("worktree root {} no longer exists", info.root.display()),
            )));
        }
        // Validates the worktree's own git directory and configuration.
        drop(Handle::open(&info.git_dir, self.0.scope)?);
        Worktree::new(self.clone(), info)
    }

    /// The administrative data of one worktree; None when it doesn't exist
    /// (no main worktree in a bare repository; a linked worktree without a
    /// `gitdir` or `HEAD` file, which git skips too).
    fn info(&self, handle: &Handle, id: &WorktreeId) -> Result<Option<WorktreeInfo>, GitError> {
        let common = &self.0.location.common_dir;
        match id {
            WorktreeId::Main => {
                let Some(workdir) = handle.workdir() else {
                    return Ok(None);
                };
                let literal = normalize(workdir);
                let (root, prunable) = canonical_or_literal(literal);
                let head = read_head(self, handle, common, id)?
                    .ok_or_else(|| GitError::Corrupt(format!("{}: no HEAD", common.display())))?;
                Ok(Some(WorktreeInfo {
                    id: id.clone(),
                    is_main: true,
                    root,
                    git_dir: common.clone(),
                    head,
                    locked: None,
                    prunable,
                }))
            }
            WorktreeId::Linked(name) => {
                if name.is_empty() || name.contains('/') || &**name == "." || &**name == ".." {
                    return Ok(None);
                }
                let admin = common.join("worktrees").join(&**name);
                let Some(recorded) = read_plain_file(&admin.join("gitdir"))? else {
                    return Ok(None);
                };
                // `extensions.relativeWorktrees` records it relative to the
                // administrative directory.
                let dot_git = normalize(&admin.join(recorded));
                let literal = if dot_git.file_name().is_some_and(|n| n == ".git") {
                    dot_git
                        .parent()
                        .map_or_else(|| dot_git.clone(), Path::to_path_buf)
                } else {
                    dot_git
                };
                let (root, prunable) = canonical_or_literal(literal);
                let Some(head) = read_head(self, handle, &admin, id)? else {
                    return Ok(None);
                };
                let locked = gitdir::read(&admin.join("locked"))?.map(|bytes| {
                    let text = String::from_utf8_lossy(&bytes);
                    text.trim_end_matches(['\n', '\r']).to_owned()
                });
                Ok(Some(WorktreeInfo {
                    id: id.clone(),
                    is_main: false,
                    root,
                    git_dir: admin,
                    head,
                    locked,
                    prunable,
                }))
            }
        }
    }
}

/// Reads `<git_dir>/HEAD` and resolves a symbolic target without reading
/// objects. None when the file doesn't exist.
pub(crate) fn read_head(
    repo: &Repo,
    common_handle: &Handle,
    git_dir: &Path,
    id: &WorktreeId,
) -> Result<Option<HeadState>, GitError> {
    let path = git_dir.join("HEAD");
    let Some(bytes) = gitdir::read(&path)? else {
        return Ok(None);
    };
    let text = bytes.trim_ascii();
    let corrupt = || {
        GitError::Corrupt(format!(
            "{}: {:?}",
            path.display(),
            String::from_utf8_lossy(text)
        ))
    };
    if let Some(target) = text.strip_prefix(b"ref:") {
        let target = std::str::from_utf8(target.trim_ascii()).map_err(|_| corrupt())?;
        let name = RefName::new(target).ok_or_else(corrupt)?;
        // Per-worktree refs (refs/worktree/, refs/bisect/, refs/rewritten/)
        // of a linked worktree live in its own git directory.
        let per_worktree = ["refs/worktree/", "refs/bisect/", "refs/rewritten/"]
            .iter()
            .any(|p| target.starts_with(p));
        let commit = match id {
            WorktreeId::Linked(_) if per_worktree => {
                let own = Handle::open(git_dir, repo.scope())?;
                resolve_direct(&own, &name)?
            }
            _ => resolve_direct(common_handle, &name)?,
        };
        Ok(Some(HeadState::Branch { name, commit }))
    } else {
        let commit = Oid::from_hex_bytes(repo.hash(), text).ok_or_else(corrupt)?;
        Ok(Some(HeadState::Detached { commit }))
    }
}

/// Follows symbolic references to the first object without reading it
/// (tags stay unpeeled). None when the reference doesn't exist.
pub(crate) fn resolve_direct(handle: &Handle, name: &RefName) -> Result<Option<Oid>, GitError> {
    let Some(mut reference) = handle.try_find_reference(name.as_str()).map_err(gix_err)? else {
        return Ok(None);
    };
    match reference.follow_to_object() {
        Ok(id) => Ok(Some(Oid::from_gix(&id))),
        // A symbolic chain that ends in a missing reference.
        Err(err) if err.is_not_found() => Ok(None),
        Err(err) => Err(GitError::from_gix(err)),
    }
}

/// The trimmed content of a one-line administrative file (`commondir`,
/// `gitdir`); None when it doesn't exist.
fn read_plain_file(path: &Path) -> Result<Option<PathBuf>, GitError> {
    let Some(bytes) = gitdir::read(path)? else {
        return Ok(None);
    };
    let line = bytes.trim_ascii_end();
    if line.is_empty() {
        return Err(GitError::Corrupt(format!("{} is empty", path.display())));
    }
    Ok(Some(path_from_bytes(line)))
}

#[cfg(unix)]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

/// The canonical path and false when it exists; the literal path and true
/// (prunable) when it doesn't.
fn canonical_or_literal(literal: PathBuf) -> (PathBuf, bool) {
    match std::fs::canonicalize(&literal) {
        Ok(canonical) => (canonical, false),
        Err(err) if err.kind() == io::ErrorKind::NotFound => (literal, true),
        Err(_) => (literal, false),
    }
}

/// Lexical normalization: drops `.`, resolves `..` against the preceding
/// component. Never touches the file system.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if out.file_name().is_some() {
                    out.pop();
                } else if !out.has_root() {
                    // A relative path that climbs above its start keeps `..`;
                    // `..` of the root is the root.
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests;
