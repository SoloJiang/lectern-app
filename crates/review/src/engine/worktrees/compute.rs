//! One worktree's counts: the uncommitted changes, the commits since the
//! merge base with the default branch, their union and the hashes of its
//! paths (docs/design/review.md「概览」, 「并行 worktree 总览」 step 2).
//! Blocking git work: executor tasks only.

use std::hash::{Hash, Hasher};

use lectern_git::{Base, ChangeSet, GitError, HeadState, Repo, Status};

use super::state::BaseRef;
use super::*;
use crate::cutoff;
use crate::engine::overview::{Counted, count_limits};
use crate::engine::round::Git;

/// Where the uncommitted count of the current worktree comes from.
pub(super) enum Source {
    /// Compared here with a handle that is dropped afterwards.
    Own,
    /// The overview's count set.
    Set(Arc<ChangeSet>),
    /// The overview's count was capped.
    Capped,
    /// The overview's first round has not published.
    Pending,
}

impl Source {
    /// The overview's count as the source for `key`, when it is of it;
    /// `current` says `key` is the project's current worktree.
    pub(super) fn of(counted: Counted, key: &WorktreeKey, current: bool) -> Source {
        match counted {
            Counted::Set(k, set) if k == *key => Source::Set(set),
            Counted::Capped(k) if k == *key => Source::Capped,
            Counted::Pending if current => Source::Pending,
            _ => Source::Own,
        }
    }

    /// The count needs no comparison here (no throttle).
    pub(super) fn is_free(&self) -> bool {
        !matches!(self, Source::Own)
    }
}

/// What to compute for one worktree.
pub(super) struct Job {
    pub project: ProjectKey,
    pub key: WorktreeKey,
    pub need: Need,
    pub source: Source,
    /// The previous counts' HEAD and merge base; None for a new worktree.
    pub prev: Option<(Option<Oid>, Option<Oid>)>,
    /// The default branch is resolved again; else this one stands.
    pub resolve_base: bool,
    pub base: Option<BaseRef>,
    pub cancel: Arc<AtomicBool>,
}

/// A worktree's counts and the paths they are of.
pub(super) struct Computed {
    pub head: Option<Oid>,
    pub merge_base: Option<Oid>,
    pub uncommitted: ChangeCount,
    pub committed: ChangeCount,
    pub vs_base: ChangeCount,
    /// The paths changed against the base, sorted and each once; None when
    /// unknown (capped, unavailable, not yet counted).
    pub paths: Option<Vec<RepoPath>>,
}

pub(super) enum Outcome {
    Cancelled,
    /// Nothing changed (the default branch moved, this worktree's merge
    /// base and HEAD did not).
    Unchanged {
        base: Option<Option<BaseRef>>,
    },
    Done {
        computed: Computed,
        base: Option<Option<BaseRef>>,
    },
}

/// The hash of a path for the overlaps: the same in every worktree and
/// run (SipHash with its fixed keys).
pub(in crate::engine) fn hash_path(path: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    hasher.finish()
}

fn unavailable(head: Option<Oid>) -> Computed {
    Computed {
        head,
        merge_base: None,
        uncommitted: ChangeCount::Unavailable,
        committed: ChangeCount::Unavailable,
        vs_base: ChangeCount::Unavailable,
        paths: None,
    }
}

/// The paths of a count set (a rename's old path too), sorted, each once.
fn set_paths(set: &ChangeSet) -> Vec<RepoPath> {
    let mut paths: Vec<RepoPath> = Vec::with_capacity(set.files().len());
    for change in set.files() {
        paths.push(change.path.clone());
        if let Status::Renamed { from, .. } = &change.status {
            paths.push(from.clone());
        }
    }
    paths.sort_unstable();
    paths.dedup();
    paths
}

fn counted(set: &ChangeSet) -> (ChangeCount, Option<Vec<RepoPath>>) {
    if set.files().len() > MAX_PATHS {
        return (ChangeCount::Capped, None);
    }
    (
        ChangeCount::Files(set.files().len() as u32),
        Some(set_paths(set)),
    )
}

/// The union of two sorted path lists.
fn union(a: &[RepoPath], b: &[RepoPath]) -> Vec<RepoPath> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => {
                out.push(a[i].clone());
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                out.push(b[j].clone());
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                out.push(a[i].clone());
                i += 1;
                j += 1;
            }
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
    out
}

/// The default branch and the commit it names. None when the repository
/// has none or it does not resolve (then there is no base).
fn resolve_base(repo: &Repo) -> Result<Option<BaseRef>, GitError> {
    let Some(name) = repo.default_branch()? else {
        return Ok(None);
    };
    Ok(repo.resolve(&name)?.map(|oid| BaseRef {
        name: Arc::from(name.as_str()),
        oid,
    }))
}

impl Engine {
    /// The counts of one worktree. `Cancelled` when the cancel flag went.
    pub(super) fn worktrees_compute(&self, job: &Job) -> Outcome {
        let cancel = &*job.cancel;
        // The handle: a review's or the overview's when there is one, else
        // opened for this and dropped on return (no stat cache is kept for
        // a worktree nobody reviews).
        let cached = lock(&self.gits).get(&job.key).cloned();
        let git: Arc<Git> = match cached {
            Some(git) => git,
            None => match self.open_git(&job.project, &job.key) {
                Ok(git) => git,
                Err(e) => {
                    warn!("worktree handle unavailable";
                        error = diag::review_error(&e));
                    return Outcome::Done {
                        computed: unavailable(None),
                        base: None,
                    };
                }
            },
        };
        let base = if job.resolve_base {
            match resolve_base(&git.repo) {
                Ok(base) => Some(base),
                Err(GitError::Cancelled) => return Outcome::Cancelled,
                Err(e) => {
                    let (error, io) = diag::git_error(&e);
                    warn!("default branch unreadable"; error = error, io = io);
                    Some(None)
                }
            }
        } else {
            None
        };
        let current_base: Option<&BaseRef> = match &base {
            Some(resolved) => resolved.as_ref(),
            None => job.base.as_ref(),
        };
        let head = match git.worktree.head() {
            Ok(HeadState::Branch { commit, .. }) => commit,
            Ok(HeadState::Detached { commit }) => Some(commit),
            Err(e) => {
                let (error, io) = diag::git_error(&e);
                warn!("worktree head unreadable"; error = error, io = io);
                return Outcome::Done {
                    computed: unavailable(None),
                    base,
                };
            }
        };

        // Only the commits side is stale and neither HEAD nor the merge
        // base moved: nothing to do.
        if job.need == Need::COMMITTED
            && let Some((prev_head, prev_mb)) = job.prev
            && prev_head == head
        {
            let mb = match self.worktrees_merge_base(&git, current_base, head, cancel) {
                Ok(mb) => mb,
                Err(GitError::Cancelled) => return Outcome::Cancelled,
                Err(_) => None,
            };
            if mb == prev_mb {
                return Outcome::Unchanged { base };
            }
        }

        // The uncommitted changes.
        let (uncommitted, unc_paths) = match &job.source {
            Source::Set(set) => counted(set),
            Source::Capped => (ChangeCount::Capped, None),
            Source::Pending => (ChangeCount::Pending, None),
            Source::Own => match git.worktree.changes(Base::Head, &count_limits(), cancel) {
                Ok(set) => counted(&set),
                Err(GitError::Cancelled) => return Outcome::Cancelled,
                Err(e) => {
                    let (error, io) = diag::git_error(&e);
                    warn!("worktree comparison failed"; error = error, io = io);
                    (ChangeCount::Unavailable, None)
                }
            },
        };

        // The commits since the merge base with the default branch.
        let mut merge_base = None;
        let (committed, com_paths) = match (current_base, head) {
            (Some(_), Some(head)) => {
                match self.worktrees_merge_base(&git, current_base, Some(head), cancel) {
                    Err(GitError::Cancelled) => return Outcome::Cancelled,
                    Err(e) => {
                        let (error, io) = diag::git_error(&e);
                        warn!("merge base unreadable"; error = error, io = io);
                        (ChangeCount::Unavailable, None)
                    }
                    Ok(None) => (ChangeCount::Unavailable, None),
                    Ok(Some(mb)) if mb == head => {
                        merge_base = Some(mb);
                        (ChangeCount::Files(0), Some(Vec::new()))
                    }
                    Ok(Some(mb)) => {
                        merge_base = Some(mb);
                        let partition = self.cfg.cache.repo(git.common_dir());
                        match cutoff::tree_changes(&git.repo, &partition, Some(mb), head, cancel) {
                            None if cancel.load(Ordering::Acquire) => return Outcome::Cancelled,
                            None => (ChangeCount::Unavailable, None),
                            Some(lectern_git::TreeChanges::All) => (ChangeCount::Capped, None),
                            Some(lectern_git::TreeChanges::Paths(paths))
                                if paths.len() > MAX_PATHS =>
                            {
                                (ChangeCount::Capped, None)
                            }
                            Some(lectern_git::TreeChanges::Paths(paths)) => {
                                (ChangeCount::Files(paths.len() as u32), Some(paths.to_vec()))
                            }
                        }
                    }
                }
            }
            _ => (ChangeCount::Unavailable, None),
        };

        // Against the base: both sets' union. Without a base, or without
        // the commits side, the uncommitted paths stand for the overlaps.
        let (vs_base, paths) = if matches!(committed, ChangeCount::Unavailable) {
            (ChangeCount::Unavailable, unc_paths)
        } else {
            match (uncommitted, committed) {
                (ChangeCount::Files(_), ChangeCount::Files(_)) => {
                    let all = union(
                        unc_paths.as_deref().unwrap_or(&[]),
                        com_paths.as_deref().unwrap_or(&[]),
                    );
                    if all.len() > MAX_PATHS {
                        (ChangeCount::Capped, None)
                    } else {
                        (ChangeCount::Files(all.len() as u32), Some(all))
                    }
                }
                (ChangeCount::Capped, _) | (_, ChangeCount::Capped) => (ChangeCount::Capped, None),
                (ChangeCount::Pending, _) | (_, ChangeCount::Pending) => {
                    (ChangeCount::Pending, None)
                }
                _ => (ChangeCount::Unavailable, None),
            }
        };
        Outcome::Done {
            computed: Computed {
                head,
                merge_base,
                uncommitted,
                committed,
                vs_base,
                paths,
            },
            base,
        }
    }

    /// The merge base of the default branch and `head`, from memory or
    /// computed and remembered. None without a base or a HEAD, or when the
    /// two share no history.
    fn worktrees_merge_base(
        &self,
        git: &Git,
        base: Option<&BaseRef>,
        head: Option<Oid>,
        cancel: &AtomicBool,
    ) -> Result<Option<Oid>, GitError> {
        let (Some(base), Some(head)) = (base, head) else {
            return Ok(None);
        };
        let key = (base.oid, head);
        let cached = lock(&self.worktrees)
            .projects
            .get(&git.project)
            .and_then(|s| s.merge_bases.get(&key).copied());
        if let Some(found) = cached {
            return Ok(found);
        }
        let found = git.repo.merge_base(base.oid, head, cancel)?;
        let mut states = lock(&self.worktrees);
        if let Some(state) = states.projects.get_mut(&git.project) {
            if state.merge_bases.len() >= MAX_MERGE_BASES {
                state.merge_bases.clear();
            }
            state.merge_bases.insert(key, found);
        }
        Ok(found)
    }
}
