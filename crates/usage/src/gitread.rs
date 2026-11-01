//! What the branch observations read from a repository with `lectern-git`
//! (docs/design/usage.md「分支观察」): the worktrees and their HEADs when a
//! watch begins, a branch's reflog for the segments of a branch seen for the
//! first time, the default branch, and the paths a commit changed. Blocking;
//! executor only. Tests replace it with a table.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, PoisonError};

use lectern_git::{HashKind, HeadState, Oid, RefName, Repo, TreeChanges};

/// The most paths a commit's change list holds before it counts as "all".
const COMMIT_PATHS: usize = 2_000;

/// The most bytes of a branch's reflog read (docs/design/usage.md「分支观察」).
const LOG_BYTES: u64 = 4 << 20;

/// Repositories kept open between reads.
const OPEN_REPOS: usize = 16;

/// A worktree and where its HEAD points.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeHead {
    /// The worktree's key: its administrative directory (the common
    /// directory for the main worktree).
    pub(crate) key: PathBuf,
    /// The full reference name of the branch; None when detached.
    pub(crate) branch: Option<String>,
}

/// What a branch's reflog says about its segments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BranchLog {
    /// The reflog file exists (the branch does).
    pub(crate) exists: bool,
    /// When the branch was created (Unix milliseconds), when the log starts
    /// with its creation.
    pub(crate) created: Option<i64>,
    /// Renames: when, from, to (full reference names), oldest first.
    pub(crate) renames: Vec<(i64, String, String)>,
}

/// The reads. `common_dir` is a repository's common directory.
pub trait GitRead: Send + Sync {
    /// The worktrees with their HEADs; None when the repository cannot be
    /// read.
    fn heads(&self, common_dir: &Path) -> Option<Vec<WorktreeHead>>;
    /// The full reference name of the default branch.
    fn default_branch(&self, common_dir: &Path) -> Option<String>;
    /// The reflog of a branch (`refs/heads/...`); None when it cannot be read.
    fn branch_log(&self, common_dir: &Path, branch: &str) -> Option<BranchLog>;
    /// The paths (relative, slash separated) changed from `old` to `new`;
    /// None when they cannot be told (a missing object, too many).
    fn commit_paths(
        &self,
        common_dir: &Path,
        old: Option<&[u8]>,
        new: &[u8],
    ) -> Option<Vec<String>>;
}

/// The repositories on disk.
#[derive(Default)]
pub(crate) struct DiskGit {
    repos: Mutex<HashMap<PathBuf, Repo>>,
    cancel: Arc<AtomicBool>,
}

impl DiskGit {
    pub(crate) fn new() -> DiskGit {
        DiskGit::default()
    }

    fn repo(&self, common_dir: &Path) -> Option<Repo> {
        let mut repos = self.repos.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(repo) = repos.get(common_dir) {
            return Some(repo.clone());
        }
        let found = Repo::discover(common_dir, &[]).ok()??;
        if repos.len() >= OPEN_REPOS {
            repos.clear();
        }
        repos.insert(common_dir.to_path_buf(), found.repo.clone());
        Some(found.repo)
    }
}

impl GitRead for DiskGit {
    fn heads(&self, common_dir: &Path) -> Option<Vec<WorktreeHead>> {
        let repo = self.repo(common_dir)?;
        let worktrees = repo.worktrees().ok()?;
        Some(
            worktrees
                .into_iter()
                .map(|w| WorktreeHead {
                    key: w.git_dir,
                    branch: match w.head {
                        HeadState::Branch { name, .. } => Some(name.as_str().to_owned()),
                        HeadState::Detached { .. } => None,
                    },
                })
                .collect(),
        )
    }

    fn default_branch(&self, common_dir: &Path) -> Option<String> {
        let repo = self.repo(common_dir)?;
        let name = repo.default_branch().ok()??;
        // A remote-tracking default (`refs/remotes/origin/main`) names the
        // local branch of the same name.
        let full = name.as_str();
        match full.strip_prefix("refs/remotes/") {
            Some(rest) => rest
                .split_once('/')
                .map(|(_, branch)| format!("refs/heads/{branch}")),
            None => Some(full.to_owned()),
        }
    }

    fn branch_log(&self, common_dir: &Path, branch: &str) -> Option<BranchLog> {
        let repo = self.repo(common_dir)?;
        let name = RefName::new(branch)?;
        let log = repo.ref_reflog(&name, LOG_BYTES).ok()?;
        let mut out = BranchLog {
            exists: log.exists,
            ..BranchLog::default()
        };
        if let Some(first) = log.entries.first()
            && log.complete
            && first.old.is_none()
        {
            out.created = Some(first.time.seconds.saturating_mul(1000));
        }
        for entry in &log.entries {
            if let lectern_git::ReflogAction::BranchRenamed { from, to } = &entry.action {
                out.renames.push((
                    entry.time.seconds.saturating_mul(1000),
                    from.as_str().to_owned(),
                    to.as_str().to_owned(),
                ));
            }
        }
        Some(out)
    }

    fn commit_paths(
        &self,
        common_dir: &Path,
        old: Option<&[u8]>,
        new: &[u8],
    ) -> Option<Vec<String>> {
        let repo = self.repo(common_dir)?;
        let hash: HashKind = repo.location().hash;
        let oid = |bytes: &[u8]| {
            let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            Oid::from_hex(hash, &hex)
        };
        let old = match old {
            Some(b) => Some(oid(b)?),
            None => None,
        };
        let new = oid(new)?;
        match repo.tree_changes(old, new, COMMIT_PATHS, &self.cancel) {
            Ok(TreeChanges::Paths(paths)) => Some(
                paths
                    .iter()
                    .map(|p| String::from_utf8_lossy(p.as_bytes()).into_owned())
                    .collect(),
            ),
            _ => None,
        }
    }
}
