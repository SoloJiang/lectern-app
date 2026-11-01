//! Reads the facts that identify a repository (docs/design/usage.md「项目标识」)
//! with `lectern-git`: root commits, remotes, the primary remote and
//! whether there is a commit yet. Blocking; executor only. Nothing here is
//! logged: remote addresses can carry credentials.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use lectern_git::{GitError, Oid, RefName, Repo};
use lectern_services::usage::ForgeRepo;

use crate::identity::{Previous, Remote, RepoFacts, choose_primary, normalize_address};

/// The root commits kept: a repository that merged other histories has
/// several, and a handful is enough to recognize it.
const ROOTS: usize = 4;

/// The most new commits the check "the old tip is an ancestor" visits; a
/// tip further from the old one is walked from the start.
const ADVANCE_LIMIT: usize = 10_000;

/// Reads the facts of the repository whose common directory is
/// `common_dir`. `previous` is what the last identification found: the
/// primary remote chosen then stays the primary while that remote exists
/// (docs/design/usage.md: 新增的远端不影响), and the root commits are not
/// looked for again while the tip they were found from is still the tip.
/// `forge` is the code platform's repository ID, when known. `cancel` stops
/// the walk of the history (`GitError::Cancelled`).
pub(crate) fn read(
    common_dir: &Path,
    previous: &Previous,
    forge: Option<ForgeRepo>,
    cancel: &AtomicBool,
) -> Result<RepoFacts, GitError> {
    let found = Repo::discover(common_dir, &[])?;
    let Some(found) = found else {
        return Err(GitError::Corrupt("not a repository".into()));
    };
    from_repo(&found.repo, previous, forge, cancel)
}

/// The same for an open [`Repo`].
pub(crate) fn from_repo(
    repo: &Repo,
    previous: &Previous,
    forge: Option<ForgeRepo>,
    cancel: &AtomicBool,
) -> Result<RepoFacts, GitError> {
    let remotes: Vec<Remote> = repo
        .remotes()?
        .into_iter()
        .filter_map(|r| {
            let address = normalize_address(r.fetch_url.as_deref()?);
            (!address.is_empty()).then(|| Remote {
                name: r.name.to_string(),
                address,
            })
        })
        .collect();
    let default = repo.default_branch()?;
    let tip = match &default {
        Some(name) => repo.resolve(name)?,
        None => None,
    };
    let tip = match tip {
        Some(tip) => Some(tip),
        None => match RefName::new("HEAD") {
            Some(head) => repo.resolve(&head)?,
            None => None,
        },
    };
    let tip = match tip {
        Some(tip) => Some(tip),
        // HEAD on a branch without a commit while other branches have
        // one: any of them shows the repository's roots.
        None => repo.branches()?.first().map(|b| b.commit),
    };
    let upstream = default_upstream(repo, default.as_ref(), &remotes)?;
    let (primary_name, primary) = choose_primary(
        &remotes,
        previous.primary_name.as_deref(),
        upstream.as_deref(),
    );
    let tip_hex = tip.map(|oid| oid.to_string());
    let roots = match tip {
        // The walk reads every commit down to the roots, which is the
        // whole history of a repository with one root: only when the tip
        // moved.
        Some(_) if tip_hex == previous.tip && !previous.roots.is_empty() => previous.roots.clone(),
        Some(oid) => roots_of(repo, oid, previous, cancel)?,
        None => Default::default(),
    };
    Ok(RepoFacts {
        unborn: tip.is_none(),
        tip: tip_hex,
        roots,
        remotes: remotes.into_iter().map(|r| r.address).collect(),
        primary,
        primary_name,
        forge,
    })
}

/// The root commits of `tip`. A tip that moved forward from the tip the
/// stored roots were found from has those roots and the parentless commits
/// among the new commits (a merge can bring a history with a root of its
/// own), found by visiting only the new commits. A tip that is not a
/// descendant (a rewrite, another default branch), further away than
/// [`ADVANCE_LIMIT`] commits, or whose old tip is gone is walked from the
/// start.
fn roots_of(
    repo: &Repo,
    tip: Oid,
    previous: &Previous,
    cancel: &AtomicBool,
) -> Result<BTreeSet<String>, GitError> {
    let old = previous
        .tip
        .as_deref()
        .and_then(|hex| Oid::from_hex(repo.location().hash, hex));
    if let Some(old) = old
        && !previous.roots.is_empty()
    {
        match repo.advance_since(old, tip, ADVANCE_LIMIT, cancel) {
            Ok(Some(advance)) => {
                let mut roots = previous.roots.clone();
                for root in advance.roots {
                    if roots.len() < ROOTS {
                        roots.insert(root.to_string());
                    }
                }
                return Ok(roots);
            }
            Err(GitError::Cancelled) => return Err(GitError::Cancelled),
            Ok(None) | Err(_) => {}
        }
    }
    Ok(repo
        .root_commits(tip, ROOTS, cancel)?
        .into_iter()
        .map(|oid| oid.to_string())
        .collect())
}

/// The name of the remote the default branch comes from: the remote of a
/// remote-tracking default branch, or the upstream of a local one.
fn default_upstream(
    repo: &Repo,
    default: Option<&RefName>,
    remotes: &[Remote],
) -> Result<Option<String>, GitError> {
    let Some(default) = default else {
        return Ok(None);
    };
    if let Some(rest) = default.as_str().strip_prefix("refs/remotes/") {
        // A remote's name may contain a slash: the longest match.
        let best = remotes
            .iter()
            .filter(|r| {
                rest.strip_prefix(r.name.as_str())
                    .is_some_and(|t| t.starts_with('/'))
            })
            .max_by_key(|r| r.name.len());
        return Ok(best.map(|r| r.name.clone()));
    }
    let branch = repo.branches()?.into_iter().find(|b| &b.name == default);
    Ok(branch
        .and_then(|b| b.upstream)
        .map(|u| u.remote.to_string())
        .filter(|name| remotes.iter().any(|r| &r.name == name)))
}
