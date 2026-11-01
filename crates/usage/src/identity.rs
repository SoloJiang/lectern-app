//! Project identification (docs/design/usage.md「项目标识」): what an
//! opened repository shows (`RepoFacts`), and the judgment whether it is a
//! project the database already knows (`identify`). Pure: the store reads
//! the known projects and applies the result, `facts` reads the repository.
//!
//! A repository is compared by its root commits, the address of its
//! primary remote (or the whole set of addresses when several remotes and
//! no default-branch upstream leave no primary), and the code platform's
//! repository ID when there is one. The ID wins over everything else; the
//! remote set only serves to recognize a moved repository, because a fork
//! often adds the original as `upstream` and an intersection would merge
//! the two.
//!
//! Remote addresses carry user names and sometimes tokens: they are
//! reduced to host and path before they are kept, and never logged.

use std::collections::BTreeSet;

use lectern_services::usage::{ForgeRepo, TemporaryReason, UsageProjectId};

/// What a repository shows when it is opened.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepoFacts {
    /// The root commits (hex object IDs) reachable from the default
    /// branch, or from HEAD without one: at most four. Empty for a
    /// repository without a commit.
    pub roots: BTreeSet<String>,
    /// The normalized fetch addresses of all remotes.
    pub remotes: BTreeSet<String>,
    /// The primary remote's normalized address; with several remotes and
    /// none chosen, the sorted addresses joined by newlines; None without
    /// remotes.
    pub primary: Option<String>,
    /// The name of the remote chosen as primary, kept so that the choice
    /// stays when remotes are added. None when no single remote was
    /// chosen.
    pub primary_name: Option<String>,
    /// The code platform's repository ID, when the platform is known and
    /// answered.
    pub forge: Option<ForgeRepo>,
    /// The repository has no commit yet.
    pub unborn: bool,
}

/// A project the database knows, as `identify` compares it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownProject {
    pub id: UsageProjectId,
    /// Recorded at the opened location: the latest identified project
    /// there.
    pub here: bool,
    /// Its path no longer exists, or another repository holds it now: it
    /// may be the repository that was moved.
    pub path_missing: bool,
    /// What it showed when it was last identified.
    pub facts: RepoFacts,
}

/// The outcome of [`identify`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Identification {
    /// The project recorded here, unchanged.
    Same(UsageProjectId),
    /// The repository was moved or renamed: the project at the old path
    /// continues here.
    Moved { from: UsageProjectId },
    /// History was rewritten, the repository is the recorded project.
    Rewritten { project: UsageProjectId },
    /// A project of its own.
    New,
    /// A new project for now; the user may say it is `candidate`.
    Temporary {
        reason: TemporaryReason,
        candidate: Option<UsageProjectId>,
    },
}

/// Decides what `seen`, a repository just opened, is. `known` holds the
/// identified projects: the one recorded at the opened location (`here`)
/// and those elsewhere whose path may have gone (`path_missing`).
pub fn identify(known: &[KnownProject], seen: &RepoFacts) -> Identification {
    let here = known.iter().find(|k| k.here);
    if seen.unborn {
        return match here {
            // Still without a commit: nothing new.
            Some(h) if h.facts.unborn => Identification::Same(h.id),
            other => Identification::Temporary {
                reason: TemporaryReason::Unborn,
                candidate: other.map(|h| h.id),
            },
        };
    }
    match here {
        Some(h) if !h.facts.unborn => at_known_location(h, seen),
        // No record here, or only the placeholder of a repository that had
        // no commit: judged like a repository seen for the first time.
        _ => elsewhere(known, seen),
    }
}

/// The location has a recorded project `known`.
fn at_known_location(known: &KnownProject, seen: &RepoFacts) -> Identification {
    let old = &known.facts;
    let same_root = !old.roots.is_disjoint(&seen.roots);
    match (&old.forge, &seen.forge) {
        // The project had an ID and this time there is none (offline, not
        // asked yet): not the same until it is known, whatever the roots.
        (Some(_), None) => {
            return Identification::Temporary {
                reason: TemporaryReason::ForgeIdPending,
                candidate: Some(known.id),
            };
        }
        // The IDs decide.
        (Some(old_id), Some(new_id)) => {
            return match (old_id == new_id, same_root) {
                (true, true) => Identification::Same(known.id),
                (true, false) => Identification::Rewritten { project: known.id },
                (false, _) => Identification::New,
            };
        }
        // No recorded ID: the remotes and roots decide.
        (None, _) => {}
    }
    if same_root {
        return match (&old.primary, &seen.primary) {
            (Some(a), Some(b)) if a != b => Identification::Temporary {
                reason: TemporaryReason::PrimaryRemoteChanged,
                candidate: Some(known.id),
            },
            // Equal, or a remote added or removed: the same roots are
            // evidence enough that nothing else is at the path.
            _ => Identification::Same(known.id),
        };
    }
    match (&old.primary, &seen.primary) {
        (Some(a), Some(b)) if a == b => Identification::Rewritten { project: known.id },
        _ if old.remotes.is_empty() && seen.remotes.is_empty() => Identification::Temporary {
            reason: TemporaryReason::LocalRewrite,
            candidate: Some(known.id),
        },
        _ => Identification::New,
    }
}

/// The location has no recorded project: a moved repository, or a new one.
fn elsewhere(known: &[KnownProject], seen: &RepoFacts) -> Identification {
    let gone: Vec<&KnownProject> = known.iter().filter(|k| !k.here && k.path_missing).collect();
    if let Some(id) = &seen.forge {
        let same_id: Vec<&&KnownProject> = gone
            .iter()
            .filter(|k| k.facts.forge.as_ref() == Some(id))
            .collect();
        match same_id.as_slice() {
            [one] => return Identification::Moved { from: one.id },
            [] => {}
            // More than one: the user decides.
            _ => return Identification::New,
        }
    }
    let same_repo: Vec<&&KnownProject> = gone
        .iter()
        .filter(|k| {
            // The IDs, when both sides have one, were compared above.
            (k.facts.forge.is_none() || seen.forge.is_none())
                && !seen.remotes.is_empty()
                && k.facts.remotes == seen.remotes
                && !k.facts.roots.is_disjoint(&seen.roots)
        })
        .collect();
    match same_repo.as_slice() {
        [one] => Identification::Moved { from: one.id },
        _ => Identification::New,
    }
}

/// A remote's name and normalized fetch address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    pub address: String,
}

/// The primary remote: the one chosen before when it still exists; else
/// the remote the default branch's upstream is on; else the only remote.
/// With several remotes and no choice the primary is their whole set.
/// Returns (the chosen remote's name, the primary address or set).
pub fn choose_primary(
    remotes: &[Remote],
    recorded: Option<&str>,
    default_upstream: Option<&str>,
) -> (Option<String>, Option<String>) {
    let find = |name: &str| remotes.iter().find(|r| r.name == name);
    let chosen = recorded
        .and_then(find)
        .or_else(|| default_upstream.and_then(find))
        .or(match remotes {
            [only] => Some(only),
            _ => None,
        });
    if let Some(remote) = chosen {
        return (Some(remote.name.clone()), Some(remote.address.clone()));
    }
    if remotes.is_empty() {
        return (None, None);
    }
    let set: BTreeSet<&str> = remotes.iter().map(|r| r.address.as_str()).collect();
    (None, Some(set.into_iter().collect::<Vec<_>>().join("\n")))
}

/// A remote address reduced to host and path: without the scheme, the user
/// name (and any password or token in it) and the port, with the host in
/// lower case and without a trailing `.git` or `/`. `https://u:t@Host:8443/a/b.git/`,
/// `ssh://git@host/a/b` and `git@host:a/b.git` are all `host/a/b`. A local
/// path keeps its leading `/`.
pub fn normalize_address(url: &str) -> String {
    let url = url.trim();
    let (authority, path) = if let Some((_, rest)) = url.split_once("://") {
        match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        }
    } else if let Some((left, path)) = url.split_once(':')
        && !left.contains('/')
        && !left.is_empty()
    {
        // The scp form `user@host:path`.
        (left, path)
    } else {
        ("", url)
    };
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = match host.rsplit_once(':') {
        Some((name, port)) if port.bytes().all(|b| b.is_ascii_digit()) && !name.ends_with(']') => {
            name
        }
        _ => host,
    };
    let host = host.to_ascii_lowercase();
    let mut path = path.trim_end_matches('/');
    path = path.strip_suffix(".git").unwrap_or(path);
    path = path.trim_end_matches('/');
    if host.is_empty() {
        return path.to_owned();
    }
    format!("{host}/{}", path.trim_start_matches('/'))
}
