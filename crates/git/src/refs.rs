//! References and history (docs/design/git.md「引用与历史」).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gix::bstr::ByteSlice;

use crate::error::gix_err;
use crate::handle::Handle;
use crate::{EntryLimits, GitError, Mode, Oid, RefName, RefReflog, Repo, RepoPath, tree};

/// Seconds since the epoch and the UTC offset in seconds (east positive),
/// as git records them.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct GitTime {
    pub seconds: i64,
    pub offset: i32,
}

/// A commit's author or committer: raw bytes, as recorded.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Signature {
    pub name: Arc<[u8]>,
    pub email: Arc<[u8]>,
    pub time: GitTime,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CommitInfo {
    pub id: Oid,
    pub tree: Oid,
    pub parents: Arc<[Oid]>,
    /// The title, as `git log --format=%s` shows it: the first paragraph
    /// of the message on one line. Raw bytes (the commit's `encoding`
    /// header isn't applied).
    pub summary: Arc<[u8]>,
    pub author: Signature,
    /// The committer's time, which `log` sorts by.
    pub committed: GitTime,
}

/// A local branch with where it pulls from and pushes to.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BranchInfo {
    /// `refs/heads/…`.
    pub name: RefName,
    pub commit: Oid,
    /// `branch.<name>.remote` and `branch.<name>.merge`.
    pub upstream: Option<RemoteBranch>,
    /// Where a plain `git push` from this branch goes: the push remote
    /// (`branch.<name>.pushRemote`, `remote.pushDefault`,
    /// `branch.<name>.remote`, `origin`) and the branch named by
    /// `remote.<r>.push` or `push.default`. None when `git push` would
    /// refuse (`nothing`, `simple` or `upstream` without a matching upstream).
    pub push: Option<RemoteBranch>,
}

/// The commits `tip` has beyond an ancestor ([`Repo::advance_since`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Advance {
    /// How many.
    pub commits: usize,
    /// The ones without a parent.
    pub roots: Vec<Oid>,
}

/// A branch on a remote.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RemoteBranch {
    /// The remote's name; `.` for the local repository; the URL itself when
    /// the configuration names a URL instead of a remote.
    pub remote: Arc<str>,
    /// The fetch URL (upstream) or push URL (push target) after
    /// `insteadOf` / `pushInsteadOf`; None for `.`.
    pub url: Option<Arc<str>>,
    /// The branch's name on the remote, e.g. `refs/heads/main`.
    pub branch: RefName,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RemoteInfo {
    pub name: Arc<str>,
    /// After `insteadOf`.
    pub fetch_url: Option<Arc<str>>,
    /// `pushurl` or `url`, after `pushInsteadOf` / `insteadOf`.
    pub push_url: Option<Arc<str>>,
}

/// The three parts of a stash entry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StashInfo {
    /// The stash commit itself: the worktree state.
    pub worktree: Oid,
    /// HEAD when the stash was made (first parent).
    pub base: Oid,
    /// The index state (second parent).
    pub index: Oid,
    /// Untracked files (third parent), only with `-u` / `-a`.
    pub untracked: Option<Oid>,
}

impl Repo {
    /// Follows symbolic references and peels annotated tags to the object
    /// they finally name. None when the reference doesn't exist. `HEAD` is
    /// the main worktree's; use `Worktree::head` for another worktree.
    pub fn resolve(&self, name: &RefName) -> Result<Option<Oid>, GitError> {
        let handle = self.open_common()?;
        let Some(mut reference) = handle.try_find_reference(name.as_str()).map_err(gix_err)? else {
            return Ok(None);
        };
        match reference.peel_to_id() {
            Ok(id) => Ok(Some(Oid::from_gix(&id))),
            Err(err) if err.is_not_found() => Ok(None),
            Err(err) => Err(GitError::from_gix(err)),
        }
    }

    /// Title, author, commit time, parents and tree. An annotated tag is
    /// peeled to its commit.
    pub fn commit(&self, id: Oid) -> Result<CommitInfo, GitError> {
        let handle = self.open_common()?;
        commit_info(&handle, id)
    }

    /// From `tip`, newest commit time first (`git log`'s default order), at
    /// most `limit` commits. For the project home's recent commits.
    pub fn log(
        &self,
        tip: Oid,
        limit: usize,
        cancel: &AtomicBool,
    ) -> Result<Vec<CommitInfo>, GitError> {
        use gix::revision::walk::Sorting;
        use gix::traverse::commit::simple::CommitTimeOrder;

        let handle = self.open_common()?;
        let mut out = Vec::new();
        if limit == 0 {
            return Ok(out);
        }
        let walk = handle
            .rev_walk([tip.to_gix()])
            .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst))
            .all()
            .map_err(|err| missing_or(err, tip))?;
        for info in walk {
            if cancel.load(Ordering::Relaxed) {
                return Err(GitError::Cancelled);
            }
            let info = info.map_err(GitError::from_gix)?;
            out.push(commit_info(&handle, Oid::from_gix(&info.id))?);
            if out.len() == limit {
                break;
            }
        }
        Ok(out)
    }

    /// The best common ancestor of `a` and `b`; None when they share no
    /// history (or a shallow clone cuts it off).
    pub fn merge_base(&self, a: Oid, b: Oid, cancel: &AtomicBool) -> Result<Option<Oid>, GitError> {
        let handle = self.open_common()?;
        merge_base(&handle, a, b, cancel)
    }

    /// Whether `ancestor` is reachable from `of` (a commit is its own
    /// ancestor).
    pub fn is_ancestor(
        &self,
        ancestor: Oid,
        of: Oid,
        cancel: &AtomicBool,
    ) -> Result<bool, GitError> {
        if ancestor == of {
            return Ok(true);
        }
        let handle = self.open_common()?;
        // If `ancestor` is reachable from `of`, it is their only best merge base.
        Ok(merge_base(&handle, ancestor, of, cancel)? == Some(ancestor))
    }

    /// One reference's reflog, `logs/<full name>` in the common directory
    /// (`refs/heads/feat`, `refs/remotes/origin/main`, `refs/stash`), oldest
    /// first, each line parsed by [`parse_reflog_line`](crate::parse_reflog_line).
    /// Reads at most the last `max_bytes` of the file (`usage` gives
    /// 4 MiB); `complete` is false when there is more before them. No file
    /// gives `exists` false. `HEAD` and per-worktree references are the
    /// main worktree's; [`Worktree::head_reflog`](crate::Worktree::head_reflog)
    /// reads another worktree's HEAD. Blocks.
    pub fn ref_reflog(&self, name: &RefName, max_bytes: u64) -> Result<RefReflog, GitError> {
        // `RefName` holds `git check-ref-format` names: no `..`, no
        // component starting with `.`, so the path stays under `logs/`.
        let path = self.location().common_dir.join("logs").join(name.as_str());
        crate::reflog::ref_reflog(&path, self.hash(), max_bytes)
    }

    /// Local branches, sorted by name, with upstream and push target
    /// (ADR 0020 matches them to merge requests).
    pub fn branches(&self) -> Result<Vec<BranchInfo>, GitError> {
        let handle = self.open_common()?;
        let platform = handle.references().map_err(gix_err)?;
        let mut out = Vec::new();
        for reference in platform.local_branches().map_err(gix_err)? {
            let mut reference = reference.map_err(gix_err)?;
            let Some(name) = RefName::from_gix(reference.name()) else {
                continue;
            };
            let commit = match reference.follow_to_object() {
                Ok(id) => Oid::from_gix(&id),
                Err(err) if err.is_not_found() => continue,
                Err(err) => return Err(GitError::from_gix(err)),
            };
            let config = BranchConfig::read(&handle, &name);
            out.push(BranchInfo {
                upstream: config.upstream(&handle),
                push: config.push(&handle),
                name,
                commit,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Configured remotes, sorted by name.
    pub fn remotes(&self) -> Result<Vec<RemoteInfo>, GitError> {
        let handle = self.open_common()?;
        let mut out = Vec::new();
        for name in handle.remote_names() {
            let Ok(name) = std::str::from_utf8(name.as_bstr()) else {
                continue;
            };
            out.push(RemoteInfo {
                name: name.into(),
                fetch_url: remote_url(&handle, name, gix::remote::Direction::Fetch),
                push_url: remote_url(&handle, name, gix::remote::Direction::Push),
            });
        }
        Ok(out)
    }

    /// The branch `refs/remotes/<main remote>/HEAD` points to; otherwise the
    /// local `main` or `master`, whichever exists first; otherwise None. The
    /// main remote is `origin`, or the only remote when there is no `origin`.
    pub fn default_branch(&self) -> Result<Option<RefName>, GitError> {
        let handle = self.open_common()?;
        let names: Vec<String> = handle
            .remote_names()
            .into_iter()
            .filter_map(|n| std::str::from_utf8(n.as_bstr()).ok().map(str::to_owned))
            .collect();
        let main = if names.iter().any(|n| n == "origin") {
            Some("origin".to_owned())
        } else if names.len() == 1 {
            names.into_iter().next()
        } else {
            None
        };
        if let Some(main) = main {
            let head = format!("refs/remotes/{main}/HEAD");
            if let Some(reference) = handle.try_find_reference(head.as_str()).map_err(gix_err)?
                && let gix::refs::TargetRef::Symbolic(target) = reference.target()
                && let Some(target) = RefName::from_gix(target)
            {
                return Ok(Some(target));
            }
        }
        for local in ["refs/heads/main", "refs/heads/master"] {
            if handle.try_find_reference(local).map_err(gix_err)?.is_some() {
                return Ok(RefName::new(local));
            }
        }
        Ok(None)
    }

    /// Root commits reachable from `tip`, at most `limit`. For project
    /// identity (ADR 0020).
    pub fn root_commits(
        &self,
        tip: Oid,
        limit: usize,
        cancel: &AtomicBool,
    ) -> Result<Vec<Oid>, GitError> {
        let handle = self.open_common()?;
        let mut out = Vec::new();
        if limit == 0 {
            return Ok(out);
        }
        let walk = handle
            .rev_walk([tip.to_gix()])
            .all()
            .map_err(|err| missing_or(err, tip))?;
        for info in walk {
            if cancel.load(Ordering::Relaxed) {
                return Err(GitError::Cancelled);
            }
            let info = info.map_err(GitError::from_gix)?;
            if info.parent_ids().next().is_none() {
                out.push(Oid::from_gix(&info.id));
                if out.len() == limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// What `tip` has that `old` has not (`git rev-list old..tip`), when
    /// `old` is an ancestor of `tip`: the number of those commits and the
    /// parentless ones among them. The walk visits the new commits, not the
    /// history below `old`. None when `old` is not an ancestor of `tip`
    /// (the history was rewritten, another branch), when more than
    /// `max_commits` new commits were visited, or when `old` is not in the
    /// repository any more: the caller then walks the whole history. For
    /// project identity: the root commits of an advanced tip are the old
    /// ones and the new parentless commits.
    pub fn advance_since(
        &self,
        old: Oid,
        tip: Oid,
        max_commits: usize,
        cancel: &AtomicBool,
    ) -> Result<Option<Advance>, GitError> {
        if old == tip {
            return Ok(Some(Advance::default()));
        }
        let handle = self.open_common()?;
        let walk = match handle
            .rev_walk([tip.to_gix()])
            .with_hidden([old.to_gix()])
            .all()
        {
            Ok(walk) => walk,
            Err(err) if err.is_not_found() => return Ok(None),
            Err(err) => return Err(missing_or(err, tip)),
        };
        let mut out = Advance::default();
        let mut reaches_old = false;
        for info in walk {
            if cancel.load(Ordering::Relaxed) {
                return Err(GitError::Cancelled);
            }
            let info = match info {
                Ok(info) => info,
                // A commit below the tip is missing (a shallow or damaged
                // repository): no answer from here.
                Err(err) if err.is_not_found() => return Ok(None),
                Err(err) => return Err(GitError::from_gix(err)),
            };
            out.commits += 1;
            if out.commits > max_commits {
                return Ok(None);
            }
            let mut parents = 0;
            for parent in info.parent_ids() {
                parents += 1;
                reaches_old |= Oid::from_gix(&parent.detach()) == old;
            }
            if parents == 0 {
                out.roots.push(Oid::from_gix(&info.id));
            }
        }
        Ok(reaches_old.then_some(out))
    }

    /// A stash commit's parts: worktree, index and (with `-u` / `-a`)
    /// untracked-files commits.
    pub fn stash(&self, id: Oid) -> Result<StashInfo, GitError> {
        let handle = self.open_common()?;
        let info = commit_info(&handle, id)?;
        match info.parents[..] {
            [base, index] => Ok(StashInfo {
                worktree: id,
                base,
                index,
                untracked: None,
            }),
            [base, index, untracked] => Ok(StashInfo {
                worktree: id,
                base,
                index,
                untracked: Some(untracked),
            }),
            _ => Err(GitError::Corrupt(format!(
                "{id} has {} parents, not a stash commit",
                info.parents.len()
            ))),
        }
    }

    /// Every path and mode in a tree, without reading contents (e.g. a
    /// stash's untracked-files commit, for `fs_watch`'s unfiltered list).
    /// Paths that break the `RepoPath` invariants are left out.
    pub fn tree_paths(
        &self,
        tree: Oid,
        limits: &EntryLimits,
        cancel: &AtomicBool,
    ) -> Result<Vec<(RepoPath, Mode)>, GitError> {
        let handle = self.open_common()?;
        let mut out = Vec::new();
        tree::walk(&handle, tree, b"", cancel, &mut |entry| {
            if let Some(path) = RepoPath::new(&entry.path) {
                if out.len() >= limits.entries {
                    return Err(GitError::TooManyEntries);
                }
                out.push((path, entry.mode));
            }
            Ok(())
        })?;
        Ok(out)
    }

    /// A blob's full content; `GitError::TooLarge` without reading it when
    /// it is over `max` bytes.
    pub fn blob(&self, id: Oid, max: u64) -> Result<Arc<[u8]>, GitError> {
        let handle = self.open_common()?;
        let header = handle
            .try_find_header(id.to_gix())
            .map_err(GitError::from_gix)?
            .ok_or(GitError::ObjectMissing(id))?;
        if header.kind() != gix::objs::Kind::Blob {
            return Err(GitError::Corrupt(format!(
                "{id} is a {}, not a blob",
                header.kind()
            )));
        }
        if header.size() > max {
            return Err(GitError::TooLarge {
                size: header.size(),
            });
        }
        let object = handle
            .try_find_object(id.to_gix())
            .map_err(GitError::from_gix)?
            .ok_or(GitError::ObjectMissing(id))?;
        Ok(Arc::from(&object.data[..]))
    }
}

/// A missing start commit is `ObjectMissing`, other failures keep their
/// classification.
pub(crate) fn missing_or(err: gix::Error, id: Oid) -> GitError {
    if err.is_not_found() {
        GitError::ObjectMissing(id)
    } else {
        GitError::from_gix(err)
    }
}

pub(crate) fn commit_info(handle: &Handle, id: Oid) -> Result<CommitInfo, GitError> {
    let object = handle
        .try_find_object(id.to_gix())
        .map_err(GitError::from_gix)?
        .ok_or(GitError::ObjectMissing(id))?;
    let commit = object
        .peel_to_kind(gix::objs::Kind::Commit)
        .map_err(|err| missing_or(err, id))?
        .into_commit();
    let decoded = commit
        .decode()
        .map_err(|err| GitError::Corrupt(format!("commit {id}: {err}")))?;
    let corrupt = |what: &str| GitError::Corrupt(format!("commit {id}: {what}"));
    let author = decoded.author().map_err(|_| corrupt("author"))?;
    let committer = decoded.committer().map_err(|_| corrupt("committer"))?;
    let time = |sig: &gix::actor::SignatureRef<'_>| {
        sig.time()
            .map(|t| GitTime {
                seconds: t.seconds,
                offset: t.offset,
            })
            .map_err(|_| corrupt("time"))
    };
    Ok(CommitInfo {
        id: Oid::from_gix(&commit.id),
        tree: Oid::from_gix(&decoded.tree()),
        parents: decoded.parents().map(|p| Oid::from_gix(&p)).collect(),
        summary: Arc::from(&**decoded.message().summary()),
        author: Signature {
            name: Arc::from(author.name.trim()),
            email: Arc::from(author.email.trim()),
            time: time(&author)?,
        },
        committed: time(&committer)?,
    })
}

pub(crate) fn merge_base(
    handle: &Handle,
    a: Oid,
    b: Oid,
    cancel: &AtomicBool,
) -> Result<Option<Oid>, GitError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(GitError::Cancelled);
    }
    if a == b {
        return Ok(Some(a));
    }
    let cache = handle
        .commit_graph_if_enabled()
        .map_err(GitError::from_gix)?;
    let mut graph = handle.revision_graph(cache.as_ref());
    let bases = gix::revision::plumbing::merge_base(a.to_gix(), &[b.to_gix()], &mut graph)
        .map_err(|err| GitError::from_gix(err.into_error()))?;
    if cancel.load(Ordering::Relaxed) {
        return Err(GitError::Cancelled);
    }
    Ok(bases.map(|bases| Oid::from_gix(bases.first())))
}

fn remote_url(handle: &Handle, name: &str, direction: gix::remote::Direction) -> Option<Arc<str>> {
    let remote = handle.try_find_remote(name)?.ok()?;
    let url = remote.url(direction)?;
    Some(String::from_utf8_lossy(&url.to_bstring()).into())
}

/// The configuration that decides a branch's upstream and push target.
struct BranchConfig {
    short: String,
    full: RefName,
    remote: Option<String>,
    merge: Option<String>,
    push_remote: Option<String>,
}

impl BranchConfig {
    fn read(handle: &Handle, full: &RefName) -> BranchConfig {
        let short = full
            .as_str()
            .strip_prefix("refs/heads/")
            .unwrap_or(full.as_str())
            .to_owned();
        let config = handle.config_snapshot();
        let file = config.plumbing();
        let get = |key: &str| {
            file.string_by("branch", Some(short.as_bytes().as_bstr()), key)
                .map(|v| String::from_utf8_lossy(&v).into_owned())
                .filter(|v| !v.is_empty())
        };
        BranchConfig {
            remote: get("remote"),
            merge: get("merge"),
            push_remote: get("pushRemote").or_else(|| {
                file.string("remote.pushDefault")
                    .map(|v| String::from_utf8_lossy(&v).into_owned())
                    .filter(|v| !v.is_empty())
            }),
            short,
            full: full.clone(),
        }
    }

    /// `branch.<name>.merge` as a full name.
    fn merge_ref(&self) -> Option<RefName> {
        let merge = self.merge.as_deref()?;
        if merge.starts_with("refs/") {
            RefName::new(merge)
        } else {
            RefName::new(&format!("refs/heads/{merge}"))
        }
    }

    fn upstream(&self, handle: &Handle) -> Option<RemoteBranch> {
        let remote = self.remote.as_deref()?;
        let branch = self.merge_ref()?;
        remote_branch(handle, remote, branch, gix::remote::Direction::Fetch)
    }

    fn push(&self, handle: &Handle) -> Option<RemoteBranch> {
        use gix::remote::Direction;

        let has_remote = |name: &str| handle.try_find_remote(name).is_some_and(|r| r.is_ok());
        // `branch.<b>.remote`, else `origin`: the remote `git push` compares
        // with to tell a triangular workflow.
        let fetch_remote = self.remote.clone().unwrap_or_else(|| "origin".to_owned());
        let push_remote = self
            .push_remote
            .clone()
            .or_else(|| self.remote.clone())
            .or_else(|| has_remote("origin").then(|| "origin".to_owned()))?;
        let has_push_specs = handle
            .try_find_remote(push_remote.as_str())
            .and_then(Result::ok)
            .is_some_and(|r| !r.refspecs(Direction::Push).is_empty());
        let branch = if has_push_specs {
            let name = gix::refs::FullName::try_from(self.full.as_str()).ok()?;
            let dst = handle
                .branch_remote_ref_name(name.as_ref(), Direction::Push)?
                .ok()?;
            RefName::from_gix(dst.as_ref())?
        } else {
            let triangular = push_remote != fetch_remote;
            let current = RefName::new(&format!("refs/heads/{}", self.short))?;
            let push_default = handle
                .config_snapshot()
                .string("push.default")
                .map(|v| String::from_utf8_lossy(&v).to_ascii_lowercase())
                .unwrap_or_else(|| "simple".to_owned());
            match push_default.as_str() {
                "nothing" => return None,
                "current" | "matching" => current,
                "upstream" | "tracking" => {
                    if triangular {
                        return None;
                    }
                    self.merge_ref()?
                }
                // `simple` (also the default for unknown values).
                _ => {
                    if triangular {
                        current
                    } else {
                        let upstream = self.merge_ref()?;
                        if upstream != current {
                            return None;
                        }
                        upstream
                    }
                }
            }
        };
        remote_branch(handle, &push_remote, branch, Direction::Push)
    }
}

fn remote_branch(
    handle: &Handle,
    remote: &str,
    branch: RefName,
    direction: gix::remote::Direction,
) -> Option<RemoteBranch> {
    let url = if remote == "." {
        None
    } else if handle.try_find_remote(remote).is_some_and(|r| r.is_ok()) {
        remote_url(handle, remote, direction)
    } else if remote.contains(':') || remote.contains('/') {
        // The configuration names a URL (or path) instead of a remote.
        Some(remote.into())
    } else {
        return None;
    };
    Some(RemoteBranch {
        remote: remote.into(),
        url,
        branch,
    })
}
