//! `identity_*` and `other_*` (docs/design/usage.md「项目标识」): the
//! judgment of `identity::identify` row by row, the facts read from a
//! repository, and what recording the result does to the totals, the
//! aliases and the commands of the Usage page.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use lectern_git::testing::TestRepo;
use lectern_services::project::{ChangedParts, ProjectEvent, ProjectKey};
use lectern_services::usage::{
    AliasId, AllUsage, ForgeRepo, ProjectInfo, ProjectRef, ProjectStatus, ProjectUsage,
    TemporaryReason, Tokens, UsageCommand, UsageDegraded, UsageProjectId, UsageQuery,
    UsageSnapshot, UsageState, Window,
};

use super::plugin::Rig;
use super::support::*;
use crate::engine::FactsReader;
use crate::identity::{
    Identification, KnownProject, Previous, Remote, RepoFacts, choose_primary, identify,
    normalize_address,
};
use crate::store::{Commanded, Outcome, Recorded};
use crate::testing::FakeSessions;

// ---- facts and known projects ------------------------------------------------

fn pid(n: u8) -> UsageProjectId {
    UsageProjectId([n; 16])
}

fn forge(id: &str) -> ForgeRepo {
    ForgeRepo {
        host: "github.com".into(),
        id: id.into(),
    }
}

/// A repository with these root commits, one remote (or none) and a
/// code platform ID (or none).
fn facts(roots: &[&str], remote: Option<&str>, id: Option<&str>) -> RepoFacts {
    let remotes: BTreeSet<String> = remote.map(normalize_address).into_iter().collect();
    RepoFacts {
        roots: roots.iter().map(|r| (*r).to_owned()).collect(),
        primary: remotes.iter().next().cloned(),
        primary_name: remote.map(|_| "origin".to_owned()),
        remotes,
        forge: id.map(forge),
        unborn: false,
        tip: None,
    }
}

fn unborn() -> RepoFacts {
    RepoFacts {
        unborn: true,
        ..RepoFacts::default()
    }
}

fn here(n: u8, facts: RepoFacts) -> KnownProject {
    KnownProject {
        id: pid(n),
        here: true,
        path_missing: false,
        facts,
    }
}

/// A project elsewhere whose path is gone.
fn gone(n: u8, facts: RepoFacts) -> KnownProject {
    KnownProject {
        id: pid(n),
        here: false,
        path_missing: true,
        facts,
    }
}

fn temporary(reason: TemporaryReason, candidate: u8) -> Identification {
    Identification::Temporary {
        reason,
        candidate: Some(pid(candidate)),
    }
}

// ---- the decision table, row by row ----------------------------------------------

#[test]
fn identity_moved_by_the_same_forge_id() {
    // Row 1: no record here, one project with a vanished path has the same
    // ID; the roots and remotes may have changed meanwhile.
    let old = facts(&["a"], Some("https://github.com/acme/old"), Some("7"));
    let seen = facts(&["b"], Some("https://github.com/acme/new"), Some("7"));
    assert_eq!(
        identify(&[gone(1, old)], &seen),
        Identification::Moved { from: pid(1) }
    );
}

#[test]
fn identity_moved_by_the_same_roots_and_remotes_without_ids() {
    // Row 1, the case without IDs.
    let old = facts(&["a"], Some("git@github.com:acme/app.git"), None);
    let seen = facts(&["a"], Some("https://github.com/acme/app"), None);
    assert_eq!(
        identify(&[gone(1, old)], &seen),
        Identification::Moved { from: pid(1) }
    );
}

#[test]
fn identity_moved_needs_exactly_one_candidate() {
    let seen = facts(&["a"], Some("https://github.com/acme/app"), Some("7"));
    let a = facts(&["a"], Some("https://github.com/acme/app"), Some("7"));
    let b = facts(&["b"], Some("https://github.com/acme/other"), Some("7"));
    assert_eq!(
        identify(&[gone(1, a), gone(2, b)], &seen),
        Identification::New,
        "two projects with the ID: the user decides"
    );
    let same = facts(&["a"], Some("https://github.com/acme/app"), None);
    let seen = facts(&["a"], Some("https://github.com/acme/app"), None);
    assert_eq!(
        identify(&[gone(1, same.clone()), gone(2, same)], &seen),
        Identification::New
    );
}

#[test]
fn identity_moved_is_not_guessed_from_roots_alone() {
    // No remotes: a copy of a local repository looks the same as a move.
    let old = facts(&["a"], None, None);
    let seen = facts(&["a"], None, None);
    assert_eq!(identify(&[gone(1, old)], &seen), Identification::New);
    // Another ID: not the same repository whatever the roots.
    let old = facts(&["a"], Some("https://github.com/acme/app"), Some("7"));
    let seen = facts(&["a"], Some("https://github.com/acme/app"), Some("8"));
    assert_eq!(identify(&[gone(1, old)], &seen), Identification::New);
    // The old path still holds a repository: nothing moved.
    let mut present = gone(1, facts(&["a"], Some("https://github.com/acme/app"), None));
    present.path_missing = false;
    let seen = facts(&["a"], Some("https://github.com/acme/app"), None);
    assert_eq!(identify(&[present], &seen), Identification::New);
}

#[test]
fn identity_fork_is_not_merged_by_an_upstream_remote() {
    // Row 2: a fork adds the original as `upstream`; the sets of addresses
    // overlap but are not equal, and the primary remotes differ.
    let original = facts(&["a"], Some("https://github.com/acme/app"), None);
    let mut fork = facts(&["a"], Some("https://github.com/me/app"), None);
    fork.remotes
        .insert(normalize_address("https://github.com/acme/app"));
    assert_eq!(identify(&[gone(1, original)], &fork), Identification::New);
}

#[test]
fn identity_location_without_a_record_makes_a_new_project() {
    // Row 2: nothing known at all.
    let seen = facts(&["a"], Some("https://github.com/acme/app"), None);
    assert_eq!(identify(&[], &seen), Identification::New);
}

#[test]
fn identity_reopen_same_repository_is_the_same_project() {
    // Row 3, each of its three conditions.
    let old = facts(&["a"], Some("https://github.com/acme/app"), Some("7"));
    let seen = facts(&["a"], Some("git@github.com:acme/app.git"), Some("7"));
    assert_eq!(
        identify(&[here(1, old)], &seen),
        Identification::Same(pid(1)),
        "the same ID"
    );
    let old = facts(&["a"], Some("https://github.com/acme/app"), None);
    let seen = facts(&["a"], Some("ssh://git@GitHub.com:22/acme/app.git/"), None);
    assert_eq!(
        identify(&[here(1, old)], &seen),
        Identification::Same(pid(1)),
        "no IDs, the same primary remote"
    );
    let old = facts(&["a"], None, None);
    assert_eq!(
        identify(&[here(1, old.clone())], &old),
        Identification::Same(pid(1)),
        "no remotes at all"
    );
}

#[test]
fn identity_reopen_after_a_remote_was_added_or_removed_is_the_same_project() {
    // Not in the table: the same roots and a remote on one side only.
    let local = facts(&["a"], None, None);
    let pushed = facts(&["a"], Some("https://github.com/acme/app"), None);
    assert_eq!(
        identify(&[here(1, local.clone())], &pushed),
        Identification::Same(pid(1))
    );
    assert_eq!(
        identify(&[here(1, pushed)], &local),
        Identification::Same(pid(1))
    );
}

#[test]
fn identity_reopen_without_a_forge_id_is_pending() {
    // Row 4, whatever the roots.
    let old = facts(&["a"], Some("https://github.com/acme/app"), Some("7"));
    for seen_roots in [&["a"], &["b"]] {
        let seen = facts(seen_roots, Some("https://github.com/acme/app"), None);
        assert_eq!(
            identify(&[here(1, old.clone())], &seen),
            temporary(TemporaryReason::ForgeIdPending, 1)
        );
    }
}

#[test]
fn identity_path_reused_by_a_fork_with_another_id_is_a_new_project() {
    // Row 5, with IDs.
    let old = facts(&["a"], Some("https://github.com/acme/app"), Some("7"));
    let fork = facts(&["a"], Some("https://github.com/me/app"), Some("8"));
    assert_eq!(identify(&[here(1, old)], &fork), Identification::New);
}

#[test]
fn identity_primary_remote_changed_without_ids_is_temporary() {
    // Row 5, without IDs: a rename of the remote and a fork look alike.
    let old = facts(&["a"], Some("https://github.com/acme/app"), None);
    let seen = facts(&["a"], Some("https://github.com/me/app"), None);
    assert_eq!(
        identify(&[here(1, old)], &seen),
        temporary(TemporaryReason::PrimaryRemoteChanged, 1)
    );
}

#[test]
fn identity_rewritten_history_with_the_same_forge_id_is_the_same_project() {
    // Row 6.
    let old = facts(&["a"], Some("https://github.com/acme/app"), Some("7"));
    let seen = facts(&["b"], Some("https://github.com/acme/app"), Some("7"));
    assert_eq!(
        identify(&[here(1, old)], &seen),
        Identification::Rewritten { project: pid(1) }
    );
}

#[test]
fn identity_rewritten_history_with_the_same_primary_remote_is_the_same_project() {
    // Row 6, without IDs.
    let old = facts(&["a"], Some("https://github.com/acme/app"), None);
    let seen = facts(&["b"], Some("https://github.com/acme/app"), None);
    assert_eq!(
        identify(&[here(1, old)], &seen),
        Identification::Rewritten { project: pid(1) }
    );
}

#[test]
fn identity_local_rewrite_without_remotes_is_temporary() {
    // Row 7.
    let old = facts(&["a"], None, None);
    let seen = facts(&["b"], None, None);
    assert_eq!(
        identify(&[here(1, old)], &seen),
        temporary(TemporaryReason::LocalRewrite, 1)
    );
}

#[test]
fn identity_path_reused_by_an_unrelated_repository_is_a_new_project() {
    // Row 8: the roots differ and nothing else says they are one.
    let old = facts(&["a"], Some("https://github.com/acme/app"), Some("7"));
    let other = facts(&["b"], Some("https://github.com/acme/app"), Some("8"));
    assert_eq!(identify(&[here(1, old)], &other), Identification::New);
    let old = facts(&["a"], Some("https://github.com/acme/app"), None);
    let other = facts(&["b"], Some("https://github.com/me/other"), None);
    assert_eq!(identify(&[here(1, old)], &other), Identification::New);
    let old = facts(&["a"], Some("https://github.com/acme/app"), None);
    let local = facts(&["b"], None, None);
    assert_eq!(identify(&[here(1, old)], &local), Identification::New);
}

#[test]
fn identity_unborn_repository_is_temporary_until_its_first_commit() {
    // Row 9.
    assert_eq!(
        identify(&[], &unborn()),
        Identification::Temporary {
            reason: TemporaryReason::Unborn,
            candidate: None
        }
    );
    // Still no commit at the next look: nothing changes.
    assert_eq!(
        identify(&[here(1, unborn())], &unborn()),
        Identification::Same(pid(1))
    );
    // The first commit: judged like a repository seen for the first time,
    // against the projects whose path is gone.
    let old = gone(2, facts(&["a"], Some("https://github.com/acme/app"), None));
    let seen = facts(&["a"], Some("https://github.com/acme/app"), None);
    assert_eq!(
        identify(&[here(1, unborn()), old], &seen),
        Identification::Moved { from: pid(2) }
    );
    assert_eq!(identify(&[here(1, unborn())], &seen), Identification::New);
    // A repository made again at the path of a project with commits.
    assert_eq!(
        identify(&[here(1, seen.clone())], &unborn()),
        temporary(TemporaryReason::Unborn, 1)
    );
}

// ---- remote addresses and the primary remote ------------------------------------

#[test]
fn identity_address_is_reduced_to_host_and_path() {
    for (url, expected) in [
        ("https://github.com/acme/app.git", "github.com/acme/app"),
        (
            "https://User:Tok3n@GitHub.com:8443/acme/app.git/",
            "github.com/acme/app",
        ),
        ("ssh://git@github.com/acme/app", "github.com/acme/app"),
        ("git@github.com:acme/app.git", "github.com/acme/app"),
        ("github.com:acme/app", "github.com/acme/app"),
        (
            "https://gitlab.example.com/group/sub/app.git",
            "gitlab.example.com/group/sub/app",
        ),
        ("/srv/git/app.git", "/srv/git/app"),
        ("file:///srv/git/app.git", "/srv/git/app"),
        ("../app", "../app"),
    ] {
        assert_eq!(normalize_address(url), expected, "{url}");
    }
    let secret = normalize_address("https://me:s3cr3t@host.example/x.git");
    assert!(!secret.contains("s3cr3t") && !secret.contains("me:"));
}

fn remote(name: &str, address: &str) -> Remote {
    Remote {
        name: name.into(),
        address: normalize_address(address),
    }
}

#[test]
fn identity_primary_remote_is_chosen_kept_and_chosen_again() {
    let origin = remote("origin", "https://github.com/me/app");
    let upstream = remote("upstream", "https://github.com/acme/app");
    // The default branch's upstream first.
    assert_eq!(
        choose_primary(&[origin.clone(), upstream.clone()], None, Some("origin")),
        (Some("origin".into()), Some("github.com/me/app".into()))
    );
    // The only remote.
    assert_eq!(
        choose_primary(std::slice::from_ref(&upstream), None, None),
        (Some("upstream".into()), Some("github.com/acme/app".into()))
    );
    // Several and no way to choose: the whole set.
    assert_eq!(
        choose_primary(&[origin.clone(), upstream.clone()], None, None),
        (None, Some("github.com/acme/app\ngithub.com/me/app".into()))
    );
    // No remote.
    assert_eq!(choose_primary(&[], Some("origin"), None), (None, None));
    // Once chosen it stays, even when a remote is added or the upstream
    // would now point elsewhere.
    let added = remote("fork", "https://github.com/other/app");
    assert_eq!(
        choose_primary(
            &[origin.clone(), upstream.clone(), added],
            Some("origin"),
            Some("upstream")
        ),
        (Some("origin".into()), Some("github.com/me/app".into()))
    );
    // Removed or renamed: chosen again among what is left.
    assert_eq!(
        choose_primary(
            std::slice::from_ref(&upstream),
            Some("origin"),
            Some("origin")
        ),
        (Some("upstream".into()), Some("github.com/acme/app".into()))
    );
    // Gone and several left: the default upstream, else the set.
    let second = remote("second", "https://github.com/acme/second");
    assert_eq!(
        choose_primary(
            &[upstream.clone(), second.clone()],
            Some("origin"),
            Some("second")
        ),
        (Some("second".into()), Some("github.com/acme/second".into()))
    );
}

// ---- reading a repository ------------------------------------------------------------

/// What the repository shows, the history walked when `previous` does not
/// say it need not be.
fn facts_of(repo: &TestRepo, previous: &Previous) -> RepoFacts {
    crate::facts::from_repo(&repo.repo(), previous, None, &AtomicBool::new(false)).unwrap()
}

/// What the next identification is given after one that found `facts`.
fn previous_of(facts: &RepoFacts) -> Previous {
    Previous {
        primary_name: facts.primary_name.clone(),
        tip: facts.tip.clone(),
        roots: facts.roots.clone(),
    }
}

/// A repository with one commit on `main` and these remotes
/// (name, address); `origin` is the upstream of `main`.
fn repo_with(remotes: &[(&str, &str)]) -> (TestRepo, String) {
    let repo = TestRepo::new();
    let tree = repo.tree(&[("a.txt", b"a")]);
    let root = repo.commit(tree, &[], "root");
    let second = repo.commit(repo.tree(&[("a.txt", b"b")]), &[root], "second");
    repo.set_ref("refs/heads/main", second);
    let mut config = String::new();
    for (name, url) in remotes {
        config.push_str(&format!("[remote \"{name}\"]\n\turl = {url}\n"));
    }
    if remotes.iter().any(|(n, _)| *n == "origin") {
        config.push_str("[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n");
    }
    repo.append_config(&config);
    (repo, root.to_string())
}

#[test]
fn identity_facts_come_from_the_repository() {
    let (repo, root) = repo_with(&[
        ("origin", "https://user:secret@GitHub.com/me/app.git"),
        ("upstream", "git@github.com:acme/app.git"),
    ]);
    let facts = facts_of(&repo, &Previous::default());
    assert_eq!(facts.roots, BTreeSet::from([root]));
    assert_eq!(
        facts.remotes,
        BTreeSet::from([
            "github.com/acme/app".to_owned(),
            "github.com/me/app".to_owned()
        ])
    );
    assert_eq!(facts.primary.as_deref(), Some("github.com/me/app"));
    assert_eq!(facts.primary_name.as_deref(), Some("origin"));
    assert!(!facts.unborn);
    let text = format!("{facts:?}");
    assert!(!text.contains("secret") && !text.contains("user:"));
}

#[test]
fn identity_facts_keep_the_primary_remote_when_one_is_added_or_removed() {
    let (repo, _) = repo_with(&[("origin", "https://github.com/me/app")]);
    let first = facts_of(&repo, &Previous::default());
    assert_eq!(first.primary_name.as_deref(), Some("origin"));
    // A fork adds the original as `upstream`: the choice stays.
    repo.append_config("[remote \"upstream\"]\n\turl = https://github.com/acme/app\n");
    let remembered = Previous {
        primary_name: first.primary_name.clone(),
        ..Previous::default()
    };
    let both = facts_of(&repo, &remembered);
    assert_eq!(both.primary.as_deref(), Some("github.com/me/app"));
    assert_eq!(both.remotes.len(), 2);
    // Without the recorded choice the default branch's upstream decides the
    // same.
    let fresh = facts_of(&repo, &Previous::default());
    assert_eq!(fresh.primary_name.as_deref(), Some("origin"));
}

/// A cancelled flag: a read that walks the history ends at once with
/// `Cancelled`, so a read that succeeds with it set did not walk.
fn cancelled() -> AtomicBool {
    AtomicBool::new(true)
}

#[test]
fn identity_facts_walk_the_history_for_the_roots_once_per_tip() {
    let (repo, root) = repo_with(&[("origin", "https://github.com/me/app")]);
    // Nothing known: the walk is needed, and the cancel flag stops it.
    let none = crate::facts::from_repo(&repo.repo(), &Previous::default(), None, &cancelled());
    assert!(matches!(none, Err(lectern_git::GitError::Cancelled)));
    let first = facts_of(&repo, &Previous::default());
    assert_eq!(first.roots, BTreeSet::from([root.clone()]));
    assert!(first.tip.is_some());
    // The same tip: the roots come from what was found, no walk (the read
    // succeeds although the flag is set).
    let again =
        crate::facts::from_repo(&repo.repo(), &previous_of(&first), None, &cancelled()).unwrap();
    assert_eq!(again, first);
}

#[test]
fn identity_facts_walk_again_when_the_tip_moved() {
    let (repo, root) = repo_with(&[]);
    let first = facts_of(&repo, &Previous::default());
    // A new commit on the default branch.
    let tip = repo
        .repo()
        .resolve(&lectern_git::RefName::new("refs/heads/main").unwrap())
        .unwrap()
        .unwrap();
    let next = repo.commit(repo.tree(&[("a.txt", b"c")]), &[tip], "third");
    repo.set_ref("refs/heads/main", next);
    let stale = crate::facts::from_repo(&repo.repo(), &previous_of(&first), None, &cancelled());
    assert!(
        matches!(stale, Err(lectern_git::GitError::Cancelled)),
        "the tip moved, the history is walked"
    );
    let second = facts_of(&repo, &previous_of(&first));
    assert_ne!(second.tip, first.tip);
    assert_eq!(second.tip, Some(next.to_string()));
    assert_eq!(second.roots, BTreeSet::from([root]));
}

#[test]
fn identity_facts_reuse_what_the_store_recorded() {
    let mut h = Harness::new();
    let (repo, _) = repo_with(&[("origin", "https://github.com/me/app")]);
    let first = facts_of(&repo, &Previous::default());
    assert_eq!(
        h.store().previous(Path::new(APP)).unwrap(),
        Previous::default()
    );
    h.identified(APP, &first, OCT_20 + HOUR, &[]);
    let recorded = h.store().previous(Path::new(APP)).unwrap();
    assert_eq!(recorded, previous_of(&first));
    // The next identification of the unchanged repository does not walk.
    let again = crate::facts::from_repo(&repo.repo(), &recorded, None, &cancelled()).unwrap();
    let (_, second) = h.identified(APP, &again, OCT_20 + 2 * HOUR, &[]);
    assert_eq!(second.outcome, Outcome::Same);
}

#[test]
#[ignore = "a measurement: cargo test -p lectern-usage --lib identity_facts_walk_cost -- --ignored --nocapture"]
fn identity_facts_walk_cost() {
    // Synthetic history of one root and a chain of `n` commits.
    for n in [1_000usize, 5_000, 20_000] {
        let repo = TestRepo::new();
        let mut parent = repo.commit(repo.tree(&[("a.txt", b"0")]), &[], "root");
        let tree = repo.tree(&[("a.txt", b"x")]);
        for i in 1..n {
            parent = repo.commit(tree, &[parent], &format!("c{i}"));
        }
        repo.set_ref("refs/heads/main", parent);
        let started = std::time::Instant::now();
        let first = facts_of(&repo, &Previous::default());
        let walked = started.elapsed();
        let started = std::time::Instant::now();
        let _ = crate::facts::from_repo(&repo.repo(), &previous_of(&first), None, &cancelled())
            .unwrap();
        println!(
            "{n} commits: walk {walked:?}, unchanged tip {:?}",
            started.elapsed()
        );
    }
}

#[test]
fn identity_facts_of_a_repository_without_a_commit_are_unborn() {
    let repo = TestRepo::new();
    let facts = facts_of(&repo, &Previous::default());
    assert!(facts.unborn);
    assert!(facts.roots.is_empty());
}

#[test]
fn identity_facts_of_a_local_repository_have_no_remote() {
    let (repo, root) = repo_with(&[]);
    let facts = facts_of(&repo, &Previous::default());
    assert_eq!(facts.roots, BTreeSet::from([root]));
    assert!(facts.remotes.is_empty());
    assert_eq!((facts.primary, facts.primary_name), (None, None));
}

// ---- recording an identification --------------------------------------------------------

const APP: &str = "/work/app/.git";

/// One session of `n` records of 1 input token in `cwd`, starting at `at`.
fn session_in(fake: &FakeSessions, id: &str, cwd: &str, at: i64, n: u64) {
    fake.add(id, Path::new(cwd), at, None);
    let mut records = vec![context(0, at, cwd)];
    for i in 0..n {
        records.push(claude(
            100 * (i + 1),
            at + i as i64,
            None,
            &format!("{id}-{i}"),
            [1, 0, 0, 0],
        ));
    }
    fake.push(id, records);
}

fn claude_fake() -> FakeSessions {
    FakeSessions::new(
        "session-claude-code",
        lectern_services::session::Agent::ClaudeCode,
    )
}

impl Harness {
    /// Declares the repository whose common directory is `common`, with
    /// its main worktree one level up, and opens it as the plugin does:
    /// identified from `facts` at `at`.
    fn identified(
        &mut self,
        common: &str,
        facts: &RepoFacts,
        at: i64,
        gone: &[&str],
    ) -> (ProjectKey, Recorded) {
        let common_dir = Path::new(common);
        let root = common_dir.parent().unwrap();
        let key = self.projects.repo(root, common_dir);
        self.projects.set_open(&key, true);
        let rows = self.store().projects().unwrap();
        let gone: HashSet<i64> = rows
            .iter()
            .filter(|p| gone.iter().any(|g| p.path == Path::new(g)))
            .map(|p| p.location)
            .collect();
        let name = root.file_name().unwrap().to_string_lossy().into_owned();
        let recorded = self
            .store()
            .identify_open(common_dir, &name, facts, at, &gone)
            .unwrap();
        if !self.open.contains(&key) {
            self.open.push(key.clone());
        }
        (key, recorded)
    }

    /// The paths that no longer exist, for the snapshots.
    fn vanish(&self, paths: &[&str]) {
        *self.missing.borrow_mut() = paths.iter().map(Path::new).map(Path::to_path_buf).collect();
    }

    fn command(&self, command: UsageCommand) -> Commanded {
        self.store().command(&command, self.now).unwrap()
    }

    fn project_info(&self, pid: UsageProjectId) -> ProjectUsage {
        match self.snapshot(&UsageQuery::Project {
            project: ProjectRef::Id(pid),
            window: Window::D30,
        }) {
            Some(UsageSnapshot::Project(p)) => p,
            other => panic!("{other:?}"),
        }
    }

    fn listed(&self) -> AllUsage {
        self.all(Window::D30)
    }
}

fn find(all: &AllUsage, name: &str) -> (ProjectInfo, Tokens) {
    all.projects
        .iter()
        .find(|(p, _)| &*p.name == name)
        .cloned()
        .unwrap_or_else(|| panic!("no project {name} in {:?}", all.projects))
}

fn app_facts() -> RepoFacts {
    facts(&["a"], Some("https://github.com/acme/app"), None)
}

#[test]
fn identity_worktrees_are_one_project() {
    let mut h = Harness::new();
    h.projects.repo(Path::new("/work/app"), Path::new(APP));
    h.projects.repo(Path::new("/work/app-wt"), Path::new(APP));
    let fake = claude_fake();
    session_in(&fake, "main", "/work/app", OCT_20 + 9 * HOUR, 2);
    session_in(&fake, "wt", "/work/app-wt", OCT_20 + 10 * HOUR, 3);
    h.read_every(&fake);
    let (key, _) = h.identified(APP, &app_facts(), OCT_20 + 11 * HOUR, &[]);
    assert_eq!(
        h.store().projects().unwrap().len(),
        1,
        "one project, not two"
    );
    assert_eq!(h.project(&key, Window::D30).total.input, 5);
}

#[test]
fn identity_location_is_the_common_directory_and_its_project_is_written_into_the_facts() {
    let h = Harness::new();
    h.projects.repo(Path::new("/work/app"), Path::new(APP));
    let fake = claude_fake();
    session_in(&fake, "s", "/work/app", OCT_20 + 9 * HOUR, 2);
    h.read_every(&fake);
    let rows = h.store().projects().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].path, Path::new(APP));
    assert!(!rows[0].opened, "never opened until a ProjectEvent says so");
    let conn = rusqlite::Connection::open(h.db_path()).unwrap();
    let projects: Vec<i64> = conn
        .prepare("SELECT DISTINCT coalesce(project, 0) FROM fact")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(projects, [rows[0].id], "every fact carries the project ID");
}

#[test]
fn identity_reopen_records_the_facts_once() {
    let mut h = Harness::new();
    let (_, first) = h.identified(APP, &app_facts(), OCT_20 + HOUR, &[]);
    assert_eq!(first.outcome, Outcome::New);
    let (_, second) = h.identified(APP, &app_facts(), OCT_20 + 2 * HOUR, &[]);
    assert_eq!(second.outcome, Outcome::Same);
    assert_eq!(first.project.pid, second.project.pid);
    let conn = rusqlite::Connection::open(h.db_path()).unwrap();
    let n: i64 = conn
        .query_row("SELECT count(*) FROM project_fact", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
    // A changed fact is recorded as it is.
    let pushed = facts(&["a"], Some("https://github.com/acme/app"), Some("7"));
    let (_, third) = h.identified(APP, &pushed, OCT_20 + 3 * HOUR, &[]);
    assert_eq!(third.outcome, Outcome::Same);
    let n: i64 = conn
        .query_row("SELECT count(*) FROM project_fact", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 2);
}

#[test]
fn identity_path_reused_keeps_the_old_projects_usage() {
    // The discriminating case: an unrelated repository takes over a path.
    let mut h = Harness::new();
    let fake = claude_fake();
    session_in(&fake, "before", "/work/app", OCT_20 + 9 * HOUR, 5);
    h.projects.repo(Path::new("/work/app"), Path::new(APP));
    h.read_every(&fake);
    let (old_key, old) = h.identified(APP, &app_facts(), OCT_20 + 10 * HOUR, &[]);
    assert_eq!(old.outcome, Outcome::New);
    let before = h.listed();
    assert_eq!(find(&before, "app").1.input, 5);

    // The path is now another repository.
    let other = facts(&["z"], Some("https://github.com/someone/else"), None);
    let (new_key, new) = h.identified(APP, &other, OCT_20 + 40 * HOUR, &[]);
    assert_eq!(new.outcome, Outcome::New);
    assert_ne!(new.project.pid, old.project.pid, "a project of its own");
    assert_eq!(old_key, new_key, "the key is the path");
    let after = h.listed();
    assert_eq!(after.projects.len(), 2);
    let old_row = after
        .projects
        .iter()
        .find(|(p, _)| p.id == old.project.pid)
        .unwrap();
    let new_row = after
        .projects
        .iter()
        .find(|(p, _)| p.id == new.project.pid)
        .unwrap();
    assert_eq!(
        old_row.1.input, 5,
        "the old usage stays with the old project"
    );
    assert_eq!(new_row.1.input, 0);
    assert_eq!(
        old_row.0.status,
        ProjectStatus::PathMissing,
        "its path is taken"
    );
    assert_eq!(new_row.0.status, ProjectStatus::Active);
    assert_eq!(h.project(&new_key, Window::D30).total.input, 0);
    assert_eq!(h.project_info(old.project.pid).total.input, 5);
    assert_eq!(h.total(), before.total);

    // Usage after the takeover is the new project's.
    let later = claude_fake();
    session_in(&later, "after", "/work/app", OCT_20 + 50 * HOUR, 3);
    h.read_every(&later);
    let all = h.listed();
    assert_eq!(
        all.projects
            .iter()
            .find(|(p, _)| p.id == old.project.pid)
            .unwrap()
            .1
            .input,
        5
    );
    assert_eq!(
        all.projects
            .iter()
            .find(|(p, _)| p.id == new.project.pid)
            .unwrap()
            .1
            .input,
        3
    );
    assert_eq!(all.other, Tokens::default());
    assert_eq!(h.project(&new_key, Window::D30).total.input, 3);
    assert_buckets(&h);
}

// ---- moved repositories -----------------------------------------------------------------

const OLD: &str = "/old/proj/.git";
const NEW: &str = "/new/renamed/.git";

/// The usage a moved repository leaves behind and collects: 5 tokens at the
/// old path, then 3 at the new one before anything knows it moved.
fn moved_world() -> (Harness, ProjectKey, ProjectInfo) {
    let mut h = Harness::new();
    h.projects.repo(Path::new("/old/proj"), Path::new(OLD));
    h.projects.repo(Path::new("/new/renamed"), Path::new(NEW));
    let fake = claude_fake();
    session_in(&fake, "old", "/old/proj", OCT_20 + 9 * HOUR, 5);
    session_in(&fake, "new", "/new/renamed", OCT_20 + 30 * HOUR, 3);
    h.read_all(&fake, "old");
    let (key, recorded) = h.identified(OLD, &app_facts(), OCT_20 + 10 * HOUR, &[]);
    assert_eq!(recorded.outcome, Outcome::New);
    h.read_all(&fake, "new");
    let (info, _) = find(&h.listed(), "proj");
    (h, key, info)
}

#[test]
fn other_usage_at_a_moved_path_waits_in_other_until_the_move_is_seen() {
    let (h, _, _) = moved_world();
    h.vanish(&[OLD]);
    let all = h.listed();
    assert_eq!(all.projects.len(), 1);
    assert_eq!(all.projects[0].1.input, 5, "the old project has its own");
    assert_eq!(all.projects[0].0.status, ProjectStatus::PathMissing);
    assert_eq!(
        all.other.input, 3,
        "what happened at the new path is in 「其他」"
    );
    assert_eq!(all.total.input, 8);
}

#[test]
fn identity_moved_repository_continues_the_old_project() {
    let (mut h, old_key, old) = moved_world();
    h.vanish(&[OLD]);
    h.open.retain(|k| *k != old_key);
    let (new_key, recorded) = h.identified(NEW, &app_facts(), OCT_20 + 31 * HOUR, &[OLD]);
    assert_eq!(recorded.outcome, Outcome::Moved);
    let all = h.listed();
    assert_eq!(all.projects.len(), 1, "one project, not two");
    let (info, tokens) = &all.projects[0];
    assert_eq!(info.id, old.id, "the old project's ID stays");
    assert_eq!(&*info.name, "renamed", "named after where it is now");
    assert_eq!(info.status, ProjectStatus::Active);
    assert_eq!(info.key, Some(new_key.clone()));
    assert_eq!((tokens.input, all.other.input), (8, 0));
    assert!(all.merged.is_empty(), "an automatic alias is not listed");
    let p = h.project(&new_key, Window::D30);
    assert_eq!((p.project.id, p.total.input), (old.id, 8));
    assert_eq!(h.project_info(old.id).total.input, 8);
    // The move is an alias of its own; only the user's can be undone.
    let conn = rusqlite::Connection::open(h.db_path()).unwrap();
    let (id, kind): (i64, i64) = conn
        .query_row("SELECT id, kind FROM alias", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(kind, 0);
    assert_eq!(
        h.command(UsageCommand::Undo {
            alias: AliasId(id as u64)
        }),
        Commanded::Rejected("not_undoable")
    );
    assert_buckets(&h);
}

#[test]
fn identity_moved_repository_is_found_again_when_reopened() {
    let (mut h, _, old) = moved_world();
    h.vanish(&[OLD]);
    h.identified(NEW, &app_facts(), OCT_20 + 31 * HOUR, &[OLD]);
    let (_, again) = h.identified(NEW, &app_facts(), OCT_20 + 32 * HOUR, &[OLD]);
    assert_eq!(again.outcome, Outcome::Same);
    let all = h.listed();
    assert_eq!(all.projects.len(), 1);
    assert_eq!(all.projects[0].0.id, old.id);
    let conn = rusqlite::Connection::open(h.db_path()).unwrap();
    let aliases: i64 = conn
        .query_row("SELECT count(*) FROM alias", [], |r| r.get(0))
        .unwrap();
    assert_eq!(aliases, 1, "recorded once");
}

// ---- rewritten history and temporary projects --------------------------------------------

/// A repository at `APP` with 5 tokens before `then` is looked at and 3
/// after; `first` is what it showed the first time.
fn rewrite_world(first: &RepoFacts, then: &RepoFacts) -> (Harness, ProjectKey, Recorded, Recorded) {
    let mut h = Harness::new();
    h.projects.repo(Path::new("/work/app"), Path::new(APP));
    let fake = claude_fake();
    session_in(&fake, "before", "/work/app", OCT_20 + 9 * HOUR, 5);
    h.read_all(&fake, "before");
    let (key, one) = h.identified(APP, first, OCT_20 + 10 * HOUR, &[]);
    let (_, two) = h.identified(APP, then, OCT_20 + 40 * HOUR, &[]);
    let later = claude_fake();
    session_in(&later, "after", "/work/app", OCT_20 + 50 * HOUR, 3);
    h.read_all(&later, "after");
    (h, key, one, two)
}

#[test]
fn identity_rewritten_history_stays_one_project() {
    let first = facts(&["a"], Some("https://github.com/acme/app"), None);
    let then = facts(&["b"], Some("https://github.com/acme/app"), None);
    let (mut h, key, one, two) = rewrite_world(&first, &then);
    assert_eq!(two.outcome, Outcome::Rewritten);
    assert_ne!(
        one.project.pid, two.project.pid,
        "a row of its own at the path"
    );
    let all = h.listed();
    assert_eq!(all.projects.len(), 1);
    assert_eq!(
        all.projects[0].0.id, one.project.pid,
        "the recorded project"
    );
    assert_eq!(all.projects[0].1.input, 8, "before and after");
    assert!(all.merged.is_empty());
    assert_eq!(h.project(&key, Window::D30).total.input, 8);
    // The next look at the rewritten repository is the same project.
    let (_, third) = h.identified(APP, &then, OCT_20 + 41 * HOUR, &[]);
    assert_eq!(third.outcome, Outcome::Same);
    assert_eq!(h.listed().projects.len(), 1);
}

#[test]
fn identity_local_rewrite_is_a_temporary_project_the_user_can_confirm() {
    let first = facts(&["a"], None, None);
    let then = facts(&["b"], None, None);
    let (h, _, one, two) = rewrite_world(&first, &then);
    assert_eq!(
        two.outcome,
        Outcome::Temporary(TemporaryReason::LocalRewrite)
    );
    let all = h.listed();
    assert_eq!(
        all.projects.len(),
        2,
        "unconfirmed it is a project of its own"
    );
    let (old, old_tokens) = all
        .projects
        .iter()
        .find(|(p, _)| p.id == one.project.pid)
        .unwrap();
    let (temp, temp_tokens) = all
        .projects
        .iter()
        .find(|(p, _)| p.id == two.project.pid)
        .unwrap();
    assert_eq!((old_tokens.input, temp_tokens.input), (5, 3));
    // The path is held by the temporary project until it is confirmed.
    assert_eq!(old.status, ProjectStatus::PathMissing);
    assert_eq!(
        temp.status,
        ProjectStatus::Temporary {
            reason: TemporaryReason::LocalRewrite,
            candidate: Some(one.project.pid)
        }
    );

    // "It is the same project": joined, and the join can be undone.
    let applied = h.command(UsageCommand::ConfirmSame {
        temporary: temp.id,
        into: old.id,
    });
    assert_eq!(
        applied,
        Commanded::Applied(crate::store::AliasKind::ConfirmSame)
    );
    let joined = h.listed();
    assert_eq!(joined.projects.len(), 1);
    assert_eq!(
        (joined.projects[0].0.id, joined.projects[0].1.input),
        (old.id, 8)
    );
    assert_eq!(joined.merged.len(), 1);
    let ProjectStatus::Merged { into, alias } = joined.merged[0].status else {
        panic!("{:?}", joined.merged[0]);
    };
    assert_eq!((joined.merged[0].id, into), (temp.id, old.id));
    assert_eq!(joined.total.input, 8);
    h.command(UsageCommand::Undo { alias });
    let apart = h.listed();
    assert_eq!(apart.projects.len(), 2);
    assert!(apart.merged.is_empty());
    assert!(
        apart
            .projects
            .iter()
            .any(|(p, _)| matches!(p.status, ProjectStatus::Temporary { .. }))
    );

    // "It is a new project": it is Active with the alias to undo.
    h.command(UsageCommand::ConfirmNew { temporary: temp.id });
    let confirmed = h.listed();
    let (c, _) = confirmed
        .projects
        .iter()
        .find(|(p, _)| p.id == temp.id)
        .unwrap();
    assert_eq!(c.status, ProjectStatus::Active);
    let alias = c.confirmed.expect("the alias that makes it a new project");
    assert_eq!(
        h.command(UsageCommand::ConfirmSame {
            temporary: temp.id,
            into: old.id
        }),
        Commanded::Rejected("not_temporary"),
        "no longer temporary"
    );
    h.command(UsageCommand::Undo { alias });
    let undone = h.listed();
    let (u, _) = undone
        .projects
        .iter()
        .find(|(p, _)| p.id == temp.id)
        .unwrap();
    assert!(matches!(u.status, ProjectStatus::Temporary { .. }));
    assert_eq!(u.confirmed, None);
}

#[test]
fn identity_primary_remote_changed_is_a_temporary_project() {
    let first = facts(&["a"], Some("https://github.com/acme/app"), None);
    let then = facts(&["a"], Some("https://github.com/me/app"), None);
    let (h, _, one, two) = rewrite_world(&first, &then);
    assert_eq!(
        two.outcome,
        Outcome::Temporary(TemporaryReason::PrimaryRemoteChanged)
    );
    let all = h.listed();
    let (temp, _) = all
        .projects
        .iter()
        .find(|(p, _)| p.id == two.project.pid)
        .unwrap();
    assert_eq!(
        temp.status,
        ProjectStatus::Temporary {
            reason: TemporaryReason::PrimaryRemoteChanged,
            candidate: Some(one.project.pid)
        }
    );
}

// ---- unborn repositories ----------------------------------------------------------------

#[test]
fn identity_unborn_repository_is_promoted_by_its_first_commit() {
    let mut h = Harness::new();
    h.projects.repo(Path::new("/work/app"), Path::new(APP));
    let fake = claude_fake();
    session_in(&fake, "s", "/work/app", OCT_20 + 9 * HOUR, 4);
    h.read_all(&fake, "s");
    let (_, first) = h.identified(APP, &unborn(), OCT_20 + 10 * HOUR, &[]);
    assert_eq!(first.outcome, Outcome::Temporary(TemporaryReason::Unborn));
    assert!(h.store().is_unborn(Path::new(APP)).unwrap());
    let all = h.listed();
    assert_eq!(all.projects.len(), 1);
    assert_eq!(
        all.projects[0].0.status,
        ProjectStatus::Temporary {
            reason: TemporaryReason::Unborn,
            candidate: None
        }
    );
    // Looked at again before any commit: nothing changes.
    let (_, still) = h.identified(APP, &unborn(), OCT_20 + 11 * HOUR, &[]);
    assert_eq!(still.outcome, Outcome::Same);
    // The first commit.
    let (_, second) = h.identified(APP, &app_facts(), OCT_20 + 12 * HOUR, &[]);
    assert_eq!(second.outcome, Outcome::New);
    assert_eq!(
        second.project.pid, first.project.pid,
        "the same project, now settled"
    );
    assert!(!h.store().is_unborn(Path::new(APP)).unwrap());
    let all = h.listed();
    assert_eq!(all.projects.len(), 1);
    assert_eq!(all.projects[0].0.status, ProjectStatus::Active);
    assert_eq!(all.projects[0].1.input, 4, "its usage stays");
}

#[test]
fn identity_unborn_repository_with_a_first_commit_can_be_a_moved_one() {
    let (mut h, old_key, old) = moved_world();
    h.vanish(&[OLD]);
    h.open.retain(|k| *k != old_key);
    let (_, first) = h.identified(NEW, &unborn(), OCT_20 + 31 * HOUR, &[OLD]);
    assert_eq!(first.outcome, Outcome::Temporary(TemporaryReason::Unborn));
    let (_, second) = h.identified(NEW, &app_facts(), OCT_20 + 32 * HOUR, &[OLD]);
    assert_eq!(second.outcome, Outcome::Moved);
    let all = h.listed();
    assert_eq!(all.projects.len(), 1);
    assert_eq!(all.projects[0].0.id, old.id);
    assert_eq!(all.projects[0].0.status, ProjectStatus::Active);
    assert_eq!(all.projects[0].1.input, 8);
}

// ---- merging by hand and undoing it ------------------------------------------------------

/// Two repositories with 5 and 2 tokens.
fn two_projects() -> (Harness, ProjectInfo, ProjectInfo) {
    let mut h = Harness::new();
    h.projects
        .repo(Path::new("/work/alpha"), Path::new("/work/alpha/.git"));
    h.projects
        .repo(Path::new("/work/beta"), Path::new("/work/beta/.git"));
    let fake = claude_fake();
    session_in(&fake, "a", "/work/alpha", OCT_20 + 9 * HOUR, 5);
    session_in(&fake, "b", "/work/beta", OCT_20 + 9 * HOUR, 2);
    h.read_every(&fake);
    h.identified(
        "/work/alpha/.git",
        &facts(&["a"], Some("https://github.com/acme/alpha"), None),
        OCT_20 + 10 * HOUR,
        &[],
    );
    h.identified(
        "/work/beta/.git",
        &facts(&["b"], Some("https://github.com/acme/beta"), None),
        OCT_20 + 10 * HOUR,
        &[],
    );
    let all = h.listed();
    let (alpha, _) = find(&all, "alpha");
    let (beta, _) = find(&all, "beta");
    (h, alpha, beta)
}

#[test]
fn identity_merge_undo_moves_the_usage_there_and_back() {
    let (h, alpha, beta) = two_projects();
    h.vanish(&["/work/alpha/.git"]);
    assert_eq!(
        find(&h.listed(), "alpha").0.status,
        ProjectStatus::PathMissing
    );
    let merged = h.command(UsageCommand::Merge {
        from: alpha.id,
        into: beta.id,
    });
    assert_eq!(merged, Commanded::Applied(crate::store::AliasKind::Merge));
    let all = h.listed();
    assert_eq!(all.projects.len(), 1);
    assert_eq!(
        (all.projects[0].0.id, all.projects[0].1.input),
        (beta.id, 7)
    );
    assert_eq!(all.other, Tokens::default());
    assert_eq!(all.total.input, 7);
    assert_eq!(all.merged.len(), 1);
    assert_eq!(all.merged[0].id, alpha.id);
    let ProjectStatus::Merged { into, alias } = all.merged[0].status else {
        panic!("{:?}", all.merged[0]);
    };
    assert_eq!(into, beta.id);
    assert_eq!(h.project_info(beta.id).total.input, 7);
    assert_eq!(
        h.project_info(alpha.id).project.id,
        beta.id,
        "asked for by the merged ID it is the project it joined"
    );

    assert_eq!(
        h.command(UsageCommand::Undo { alias }),
        Commanded::Applied(crate::store::AliasKind::Undo)
    );
    let apart = h.listed();
    assert_eq!(apart.projects.len(), 2);
    assert!(apart.merged.is_empty());
    assert_eq!(find(&apart, "alpha").1.input, 5);
    assert_eq!(find(&apart, "beta").1.input, 2);
    // Undone once; again it is not in force.
    assert_eq!(
        h.command(UsageCommand::Undo { alias }),
        Commanded::Rejected("unknown_alias")
    );
    // And the merge can be made again.
    h.command(UsageCommand::Merge {
        from: alpha.id,
        into: beta.id,
    });
    assert_eq!(h.listed().projects.len(), 1);
}

#[test]
fn identity_merge_is_refused_when_it_makes_no_sense() {
    let (h, alpha, beta) = two_projects();
    let unknown = UsageProjectId([9; 16]);
    for (command, reason) in [
        (
            UsageCommand::Merge {
                from: alpha.id,
                into: alpha.id,
            },
            "loop",
        ),
        (
            UsageCommand::Merge {
                from: unknown,
                into: beta.id,
            },
            "unknown_project",
        ),
        (
            UsageCommand::ConfirmSame {
                temporary: alpha.id,
                into: beta.id,
            },
            "not_temporary",
        ),
        (
            UsageCommand::ConfirmNew {
                temporary: alpha.id,
            },
            "not_temporary",
        ),
        (UsageCommand::Undo { alias: AliasId(99) }, "unknown_alias"),
    ] {
        assert_eq!(h.command(command), Commanded::Rejected(reason), "{reason}");
    }
    h.command(UsageCommand::Merge {
        from: alpha.id,
        into: beta.id,
    });
    assert_eq!(
        h.command(UsageCommand::Merge {
            from: beta.id,
            into: alpha.id
        }),
        Commanded::Rejected("loop"),
        "joined projects do not join back"
    );
    assert_eq!(
        h.command(UsageCommand::Merge {
            from: alpha.id,
            into: beta.id
        }),
        Commanded::Rejected("already_merged")
    );
    assert_eq!(h.listed().projects.len(), 1, "refusals changed nothing");
}

#[test]
fn identity_merged_chain_follows_to_the_last_project() {
    let (mut h, alpha, beta) = two_projects();
    h.projects
        .repo(Path::new("/work/gamma"), Path::new("/work/gamma/.git"));
    let more = claude_fake();
    session_in(&more, "c", "/work/gamma", OCT_20 + 9 * HOUR, 11);
    h.read_every(&more);
    h.identified(
        "/work/gamma/.git",
        &facts(&["c"], Some("https://github.com/acme/gamma"), None),
        OCT_20 + 10 * HOUR,
        &[],
    );
    let gamma = find(&h.listed(), "gamma").0;
    h.command(UsageCommand::Merge {
        from: alpha.id,
        into: beta.id,
    });
    h.command(UsageCommand::Merge {
        from: beta.id,
        into: gamma.id,
    });
    let all = h.listed();
    assert_eq!(all.projects.len(), 1);
    assert_eq!(
        (all.projects[0].0.id, all.projects[0].1.input),
        (gamma.id, 18)
    );
    assert_eq!(all.merged.len(), 2);
    // Both point at the project they end in.
    for m in all.merged.iter() {
        let ProjectStatus::Merged { into, .. } = m.status else {
            panic!("{m:?}")
        };
        assert_eq!(into, gamma.id);
    }
}

// ---- folders ----------------------------------------------------------------------------

#[test]
fn identity_folder_deepest_root_claims_each_directory() {
    let mut h = Harness::new();
    let outer = h.projects.folder(Path::new("/work"));
    let inner = h.projects.folder(Path::new("/work/notes"));
    let fake = claude_fake();
    session_in(&fake, "inner", "/work/notes/deep", OCT_20 + 9 * HOUR, 3);
    session_in(&fake, "outer", "/work/other", OCT_20 + 9 * HOUR, 5);
    session_in(&fake, "away", "/elsewhere", OCT_20 + 9 * HOUR, 7);
    h.read_every(&fake);
    h.open_project(&outer);
    let first = h.listed();
    assert_eq!(
        find(&first, "work").1.input,
        8,
        "the outer root has both until the inner one opens"
    );
    h.open_project(&inner);
    let all = h.listed();
    assert_eq!(find(&all, "work").1.input, 5);
    assert_eq!(find(&all, "notes").1.input, 3);
    assert_eq!(all.other.input, 7);
}

#[test]
fn identity_folder_projects_can_be_merged_too() {
    let mut h = Harness::new();
    let old = h.projects.folder(Path::new("/old/notes"));
    let new = h.projects.folder(Path::new("/new/notes"));
    let fake = claude_fake();
    session_in(&fake, "old", "/old/notes", OCT_20 + 9 * HOUR, 3);
    session_in(&fake, "new", "/new/notes", OCT_20 + 9 * HOUR, 4);
    h.read_every(&fake);
    h.open_project(&old);
    h.open_project(&new);
    h.vanish(&["/old/notes"]);
    let all = h.listed();
    let pick = |key: &ProjectKey| {
        all.projects
            .iter()
            .find(|(p, _)| p.key.as_ref() == Some(key))
            .unwrap()
            .0
            .clone()
    };
    let (old_info, new_info) = (pick(&old), pick(&new));
    assert_eq!(old_info.status, ProjectStatus::PathMissing);
    h.command(UsageCommand::Merge {
        from: old_info.id,
        into: new_info.id,
    });
    let merged = h.listed();
    assert_eq!(merged.projects.len(), 1);
    assert_eq!(merged.projects[0].1.input, 7);
}

// ---- other ------------------------------------------------------------------------------

#[test]
fn other_usage_of_a_repository_never_opened_moves_to_its_project_when_it_is() {
    let mut h = Harness::new();
    h.projects.repo(Path::new("/work/app"), Path::new(APP));
    let fake = claude_fake();
    session_in(&fake, "s", "/work/app", OCT_20 + 9 * HOUR, 4);
    session_in(&fake, "t", "/somewhere/else", OCT_20 + 9 * HOUR, 6);
    h.read_every(&fake);
    let before = h.listed();
    assert!(before.projects.is_empty());
    assert_eq!(before.other.input, 10);
    h.identified(APP, &app_facts(), OCT_20 + 10 * HOUR, &[]);
    let after = h.listed();
    assert_eq!(after.projects.len(), 1);
    assert_eq!(after.projects[0].1.input, 4);
    assert_eq!(after.other.input, 6, "the stray directory stays");
    assert_eq!(after.total.input, 10);
}

// ---- salvage ----------------------------------------------------------------------------

#[test]
fn identity_salvage_keeps_projects_facts_and_aliases() {
    let (mut h, old_key, _) = moved_world();
    h.vanish(&[OLD]);
    h.open.retain(|k| *k != old_key);
    h.identified(NEW, &app_facts(), OCT_20 + 31 * HOUR, &[OLD]);
    let before = h.listed();
    let counts = |h: &Harness| -> Vec<(i64, i64)> {
        let conn = rusqlite::Connection::open(h.db_path()).unwrap();
        ["project", "project_fact", "alias"]
            .iter()
            .map(|table| {
                conn.query_row(
                    &format!("SELECT count(*), coalesce(max(id), 0) FROM {table}"),
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap()
            })
            .collect()
    };
    let rows = counts(&h);
    assert_eq!(rows.iter().map(|c| c.0).collect::<Vec<_>>(), [2, 2, 1]);
    let recovery = h.store().salvage_now(h.now).unwrap();
    assert_eq!(recovery.lost, 0);
    assert_eq!(counts(&h), rows);
    let after = h.listed();
    assert_eq!(after.projects.len(), 1);
    assert_eq!(after.projects[0].0.id, before.projects[0].0.id);
    assert_eq!(after.total, before.total);
}

#[test]
fn identity_salvage_copies_the_project_table_of_version_3() {
    let mut h = Harness::new();
    h.projects.repo(Path::new("/work/app"), Path::new(APP));
    let key = h.open_repo("/work/app");
    let fake = claude_fake();
    session_in(&fake, "s", "/work/app", OCT_20 + 9 * HOUR, 4);
    h.read_every(&fake);
    let id = h.project(&key, Window::D30).project.id;
    // The tables as version 3 had them; the salvage copies them into the
    // current ones and sums again.
    let conn = rusqlite::Connection::open(h.db_path()).unwrap();
    conn.execute_batch(super::store::BACK_TO_V3).unwrap();
    drop(conn);
    let recovery = h.store().salvage_now(h.now).unwrap();
    assert_eq!(recovery.lost, 0, "nothing lost");
    let rows = h.store().projects().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].pid, rows[0].opened, rows[0].since, rows[0].temp),
        (id, true, 0, None)
    );
    assert_eq!(h.project(&key, Window::D30).total.input, 4);
    assert_buckets(&h);
}

// ---- the plugin --------------------------------------------------------------------------

/// What each repository shows, by common directory; a test changes it
/// between events.
type Scripted = Arc<Mutex<HashMap<PathBuf, RepoFacts>>>;

fn scripted(map: &Scripted) -> FactsReader {
    let map = map.clone();
    Arc::new(move |dir, _previous, _cancel| {
        map.lock()
            .unwrap()
            .get(dir)
            .cloned()
            .ok_or_else(|| lectern_git::GitError::Corrupt("not scripted".into()))
    })
}

fn plugin_world(map: &Scripted, repos: &[(&str, u64)]) -> Rig {
    let fake = claude_fake();
    for (root, n) in repos {
        // Active a minute ago: read without waiting for the backfill's pace.
        session_in(&fake, root.trim_start_matches('/'), root, NOW - 60_000, *n);
    }
    let repos: Vec<String> = repos.iter().map(|(r, _)| (*r).to_owned()).collect();
    Rig::with_facts(
        fake,
        move |p| {
            for root in &repos {
                let key = p.repo(Path::new(root), &Path::new(root).join(".git"));
                p.set_open(&key, true);
            }
        },
        scripted(map),
    )
}

const NOW: i64 = OCT_20 + 2 * DAY + 12 * HOUR;

#[test]
fn identity_plugin_identifies_open_repositories_and_applies_commands() {
    let map = Scripted::default();
    {
        let mut facts_by_dir = map.lock().unwrap();
        facts_by_dir.insert(
            "/work/alpha/.git".into(),
            facts(&["a"], Some("https://github.com/acme/alpha"), None),
        );
        facts_by_dir.insert(
            "/work/beta/.git".into(),
            facts(&["b"], Some("https://github.com/acme/beta"), None),
        );
    }
    let rig = plugin_world(&map, &[("/work/alpha", 5), ("/work/beta", 2)]);
    rig.settle();
    let all = rig.all();
    assert_eq!(all.projects.len(), 2);
    assert!(
        all.projects
            .iter()
            .all(|(p, _)| p.status == ProjectStatus::Active && p.key.is_some())
    );
    let id_of = |all: &AllUsage, name: &str| find(all, name).0.id;
    let (alpha, beta) = (id_of(&all, "alpha"), id_of(&all, "beta"));

    // The command returns at once; the snapshot follows in the background.
    rig.usage().command(UsageCommand::Merge {
        from: alpha,
        into: beta,
    });
    rig.settle();
    let merged = rig.all();
    assert_eq!(merged.projects.len(), 1);
    assert_eq!(
        (merged.projects[0].0.id, merged.projects[0].1.input),
        (beta, 7)
    );
    let ProjectStatus::Merged { alias, .. } = merged.merged[0].status else {
        panic!("{:?}", merged.merged);
    };
    rig.usage().command(UsageCommand::Undo { alias });
    rig.settle();
    assert_eq!(rig.all().projects.len(), 2);

    // Opened again after a restart: the same projects, not new ones.
    let rig = rig.restart(|_| {});
    rig.settle();
    let again = rig.all();
    assert_eq!(again.projects.len(), 2);
    assert_eq!(id_of(&again, "alpha"), alpha);
}

#[test]
fn identity_plugin_identifies_a_repository_again_once_it_has_a_commit() {
    let map = Scripted::default();
    map.lock()
        .unwrap()
        .insert("/work/app/.git".into(), unborn());
    let rig = plugin_world(&map, &[("/work/app", 3)]);
    rig.settle();
    let first = rig.all();
    assert_eq!(
        first.projects[0].0.status,
        ProjectStatus::Temporary {
            reason: TemporaryReason::Unborn,
            candidate: None
        }
    );
    // The first commit: the project's worktrees change.
    map.lock()
        .unwrap()
        .insert("/work/app/.git".into(), app_facts());
    rig.kernel().emitter().emit(ProjectEvent::Changed {
        project: ProjectKey::Git {
            common_dir: Path::new("/work/app/.git").into(),
        },
        generation: 2,
        parts: ChangedParts {
            worktrees: true,
            ..ChangedParts::default()
        },
    });
    rig.settle();
    let second = rig.all();
    assert_eq!(second.projects.len(), 1);
    assert_eq!(second.projects[0].0.id, first.projects[0].0.id);
    assert_eq!(second.projects[0].0.status, ProjectStatus::Active);
    assert_eq!(second.projects[0].1.input, 3);
}

#[test]
fn identity_plugin_lists_a_repository_whose_facts_cannot_be_read() {
    // Nothing scripted: the reader fails, and the project is the one at its
    // location, as before the facts were read at all.
    let map = Scripted::default();
    let rig = plugin_world(&map, &[("/work/app", 3)]);
    rig.settle();
    let all = rig.all();
    assert_eq!(all.projects.len(), 1);
    assert_eq!(all.projects[0].1.input, 3);
    assert_eq!(all.projects[0].0.status, ProjectStatus::Active);
}

#[test]
fn identity_plugin_answers_from_an_old_file_kept_after_a_failed_backup() {
    // A version-3 file whose copy cannot be written is not migrated and not
    // written; its tables are not the ones the queries read, and the
    // snapshot says why it is empty.
    let map = Scripted::default();
    let rig = plugin_world(&map, &[("/work/app", 3)]);
    rig.settle();
    let (db, dir) = (rig.db(), rig.dir.path().join("usage"));
    let rig = rig.restart(move |_| {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(super::store::BACK_TO_V3).unwrap();
        conn.execute_batch("PRAGMA user_version = 3;").unwrap();
        drop(conn);
        let blocker = dir.join("usage.db.pre-v3");
        std::fs::create_dir(&blocker).unwrap();
        std::fs::write(blocker.join("keep"), b"x").unwrap();
    });
    rig.settle();
    let all = rig.all();
    assert!(
        matches!(
            all.header.state,
            UsageState::Degraded(UsageDegraded::Unwritable(_))
        ),
        "{:?}",
        all.header.state
    );
    assert!(all.projects.is_empty());
}

#[test]
fn identity_plugin_cancelled_read_records_nothing() {
    // The plugin stops while the history is walked: no fact, no project
    // marked opened, not even the record of a repository whose facts
    // cannot be read.
    let fake = claude_fake();
    session_in(&fake, "s", "/work/app", NOW - 60_000, 3);
    let reader: FactsReader = Arc::new(|_, _, _| Err(lectern_git::GitError::Cancelled));
    let rig = Rig::with_facts(
        fake,
        |p| {
            let key = p.repo(Path::new("/work/app"), Path::new(APP));
            p.set_open(&key, true);
        },
        reader,
    );
    rig.settle();
    assert!(rig.all().projects.is_empty());
    let conn = rusqlite::Connection::open(rig.db()).unwrap();
    for table in ["project_fact", "alias"] {
        let n: i64 = conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "{table}");
    }
    let opened: i64 = conn
        .query_row("SELECT count(*) FROM project WHERE opened = 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(opened, 0);
}

#[test]
fn identity_plugin_commands_that_make_no_sense_change_nothing() {
    let map = Scripted::default();
    let rig = plugin_world(&map, &[("/work/app", 3)]);
    rig.settle();
    let id = rig.all().projects[0].0.id;
    for command in [
        UsageCommand::Merge { from: id, into: id },
        UsageCommand::Undo { alias: AliasId(7) },
        UsageCommand::ConfirmNew { temporary: id },
    ] {
        rig.usage().command(command);
    }
    rig.settle();
    let all = rig.all();
    assert_eq!(all.projects.len(), 1);
    assert!(all.merged.is_empty());
}
