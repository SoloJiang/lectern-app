//! `discover::build` and the root of a session (docs/design/welcome.md
//! 「测试」纯函数). The sessions are values built here; no directory is read.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lectern_services::project::{ProjectKey, Resolved};
use lectern_services::session::{Agent, Place, SessionList, SessionSummary, Timestamp};

use super::support::*;
use crate::discover::{Built, MAX_AGENTS, MAX_FOLDERS, build, root_of};

fn list(sessions: Vec<SessionSummary>) -> Arc<SessionList> {
    Arc::new(SessionList {
        generation: 1,
        sessions: sessions.into(),
        ..SessionList::default()
    })
}

fn run(sessions: Vec<SessionSummary>, open: &[&ProjectKey], recent: &[&ProjectKey]) -> Built {
    let open: Vec<_> = open.iter().map(|k| (*k).clone()).collect();
    let recent: Vec<_> = recent.iter().map(|k| (*k).clone()).collect();
    build(&[list(sessions)], &open, &recent, Timestamp(NOW))
}

#[test]
fn groups_by_project_and_takes_the_newest_root() {
    let app = git_key("/repos/app/.git");
    let notes = folder_key("/notes");
    let sessions = vec![
        // The newest session of `app` ran in a linked worktree.
        Sess::new(
            "a1",
            "/repos/app-feature/crates/x",
            resolved(&app, "crates/x"),
        )
        .at(NOW - 5 * MINUTE)
        .build(),
        Sess::new("a2", "/repos/app", resolved(&app, ""))
            .at(NOW - 50 * MINUTE)
            .agent("session-codex", Agent::Codex)
            .build(),
        Sess::new("n1", "/notes", resolved(&notes, ""))
            .at(NOW - 30 * MINUTE)
            .build(),
    ];
    let built = run(sessions, &[], &[]);
    assert_eq!(built.total, 3);
    assert_eq!(built.skipped, 0);
    let names: Vec<_> = built.folders.iter().map(|f| &*f.name).collect();
    assert_eq!(names, ["app-feature", "notes"], "newest activity first");
    let f = &built.folders[0];
    assert_eq!(&*f.path, Path::new("/repos/app-feature"));
    assert_eq!(f.key, app);
    assert_eq!(f.sessions, 2, "the repository's other worktree counts here");
    assert_eq!(f.newest_activity, Timestamp(NOW - 5 * MINUTE));
    assert_eq!(&*f.agents, [Agent::ClaudeCode, Agent::Codex]);
    insta::assert_debug_snapshot!(built.folders);
}

#[test]
fn root_strips_rel_from_cwd() {
    let git = git_key("/repos/app/.git");
    let resolved_at = |rel: &str| Resolved {
        project: git.clone(),
        worktree: None,
        rel: Arc::from(rel),
    };
    assert_eq!(
        root_of(Path::new("/repos/app"), &resolved_at("")),
        Path::new("/repos/app")
    );
    assert_eq!(
        root_of(
            Path::new("/repos/app/crates/review"),
            &resolved_at("crates/review")
        ),
        Path::new("/repos/app")
    );
    assert_eq!(
        root_of(Path::new("/repos/app/crates"), &resolved_at("crates")),
        Path::new("/repos/app")
    );
    // A rel that is not the end of the cwd leaves the cwd alone.
    assert_eq!(
        root_of(Path::new("/repos/app/src"), &resolved_at("crates/review")),
        Path::new("/repos/app/src")
    );
    // Whole components only: "tes" is not the end of ".../crates".
    assert_eq!(
        root_of(Path::new("/repos/app/crates"), &resolved_at("tes")),
        Path::new("/repos/app/crates")
    );
    // A folder project is its own root, whatever the cwd.
    let notes = Resolved {
        project: folder_key("/notes"),
        worktree: None,
        rel: Arc::from("deep/er"),
    };
    assert_eq!(
        root_of(Path::new("/notes/deep/er"), &notes),
        Path::new("/notes")
    );
}

#[test]
fn skips_open_projects_and_recent_keys() {
    let open = git_key("/repos/open/.git");
    let recent = git_key("/repos/recent/.git");
    let fresh = git_key("/repos/fresh/.git");
    let sessions = vec![
        Sess::new("o", "/repos/open", resolved(&open, "")).build(),
        // Another worktree of the recent repository.
        Sess::new("r", "/repos/recent-wt", resolved(&recent, "")).build(),
        Sess::new("f", "/repos/fresh", resolved(&fresh, "")).build(),
    ];
    let built = run(sessions, &[&open], &[&recent]);
    let paths: Vec<_> = built.folders.iter().map(|f| f.path.to_path_buf()).collect();
    assert_eq!(paths, [PathBuf::from("/repos/fresh")]);
}

#[test]
fn skips_pending_missing_and_unresolvable() {
    let fresh = git_key("/repos/fresh/.git");
    let sessions = vec![
        Sess::new("p", "/x/p", Place::Pending).build(),
        Sess::new("m", "/x/m", Place::Missing).build(),
        Sess::new(
            "u",
            "/x/u",
            Place::Unresolvable(lectern_services::project::OpenError::NotFound),
        )
        .build(),
        // Located is an open project: not a discovery either.
        Sess::new("l", "/repos/fresh", located(&fresh, "")).build(),
    ];
    let built = run(sessions, &[], &[]);
    assert!(built.folders.is_empty());
    assert_eq!(built.total, 4);
    assert_eq!(
        built.skipped, 3,
        "Located is not skipped, it is just not a folder"
    );
}

#[test]
fn running_needs_open_turn_within_10_min() {
    let key = git_key("/repos/a/.git");
    let sessions = vec![
        // Open turn, active 9 minutes ago: running.
        Sess::new("1", "/repos/a", resolved(&key, ""))
            .at(NOW - 9 * MINUTE)
            .running()
            .build(),
        // Open turn, exactly 10 minutes: still running.
        Sess::new("2", "/repos/a", resolved(&key, ""))
            .at(NOW - 10 * MINUTE)
            .running()
            .build(),
        // Open turn, 11 minutes: stopped without closing the turn.
        Sess::new("3", "/repos/a", resolved(&key, ""))
            .at(NOW - 11 * MINUTE)
            .running()
            .build(),
        // Recent but the turn is closed.
        Sess::new("4", "/repos/a", resolved(&key, ""))
            .at(NOW - MINUTE)
            .build(),
    ];
    let built = run(sessions, &[], &[]);
    assert_eq!(
        (built.folders[0].sessions, built.folders[0].running),
        (4, 2)
    );
}

#[test]
fn subagents_count_as_sessions_not_running() {
    let key = git_key("/repos/a/.git");
    let sessions = vec![
        Sess::new("main", "/repos/a", resolved(&key, ""))
            .running()
            .build(),
        Sess::new("main/agent-1", "/repos/a", resolved(&key, ""))
            .running()
            .child_of("main")
            .build(),
    ];
    let built = run(sessions, &[], &[]);
    assert_eq!(
        (built.folders[0].sessions, built.folders[0].running),
        (2, 1)
    );
}

#[test]
fn caps_at_50() {
    let mut sessions = Vec::new();
    for i in 0..60 {
        let cwd = format!("/repos/p{i}");
        let key = folder_key(&cwd);
        sessions.push(
            Sess::new(format!("s{i}"), cwd, resolved(&key, ""))
                .at(NOW - i as i64 * MINUTE)
                .build(),
        );
    }
    let built = run(sessions, &[], &[]);
    assert_eq!(built.folders.len(), MAX_FOLDERS);
    assert_eq!(&*built.folders[0].name, "p0", "the newest is first");
    assert_eq!(&*built.folders[49].name, "p49", "the oldest ten are cut");
}

#[test]
fn at_most_four_agents_in_order_of_first_appearance() {
    let key = git_key("/repos/a/.git");
    let agent = |n: &'static str| Agent::Other(Arc::from(n));
    let sessions = vec![
        Sess::new("1", "/repos/a", resolved(&key, ""))
            .at(NOW - MINUTE)
            .build(),
        Sess::new("2", "/repos/a", resolved(&key, ""))
            .at(NOW - 2 * MINUTE)
            .agent("session-codex", Agent::Codex)
            .build(),
        Sess::new("3", "/repos/a", resolved(&key, ""))
            .at(NOW - 3 * MINUTE)
            .agent("s-x", agent("x"))
            .build(),
        Sess::new("4", "/repos/a", resolved(&key, ""))
            .at(NOW - 4 * MINUTE)
            .agent("s-y", agent("y"))
            .build(),
        Sess::new("5", "/repos/a", resolved(&key, ""))
            .at(NOW - 5 * MINUTE)
            .agent("s-z", agent("z"))
            .build(),
        Sess::new("6", "/repos/a", resolved(&key, ""))
            .at(NOW - 6 * MINUTE)
            .build(),
    ];
    let built = run(sessions, &[], &[]);
    let f = &built.folders[0];
    assert_eq!(f.agents.len(), MAX_AGENTS);
    assert_eq!(
        &*f.agents,
        [Agent::ClaudeCode, Agent::Codex, agent("x"), agent("y")]
    );
    assert_eq!(f.sessions, 6, "the fifth agent's session still counts");
}

#[test]
fn sources_are_merged_newest_first() {
    let key = git_key("/repos/a/.git");
    let from_claude = list(vec![
        Sess::new("c", "/repos/a", resolved(&key, ""))
            .at(NOW - 10 * MINUTE)
            .build(),
    ]);
    let from_codex = list(vec![
        Sess::new("x", "/repos/a", resolved(&key, ""))
            .at(NOW - MINUTE)
            .agent("session-codex", Agent::Codex)
            .build(),
    ]);
    let built = build(&[from_claude, from_codex], &[], &[], Timestamp(NOW));
    assert_eq!(&*built.folders[0].agents, [Agent::Codex, Agent::ClaudeCode]);
    assert_eq!(built.folders[0].newest_activity, Timestamp(NOW - MINUTE));
}

#[test]
fn recent_rows_get_counts_by_key_from_located_and_resolved() {
    let app = git_key("/repos/app/.git");
    let other = git_key("/repos/other/.git");
    let sessions = vec![
        // The recent project, in its main tree and in a worktree not open.
        Sess::new("1", "/repos/app", located(&app, ""))
            .running()
            .build(),
        Sess::new("2", "/repos/app-wt", resolved(&app, "")).build(),
        Sess::new("2/agent", "/repos/app-wt", resolved(&app, ""))
            .running()
            .child_of("2")
            .build(),
        // Another recent project, with no session at all.
        // A project that is neither recent nor open: a folder, no count.
        Sess::new(
            "3",
            "/repos/fresh",
            resolved(&git_key("/repos/fresh/.git"), ""),
        )
        .build(),
    ];
    let built = run(sessions, &[], &[&app, &other]);
    assert_eq!(built.counts.get(&app), Some(&(3, 1)));
    assert_eq!(built.counts.get(&other), None);
    assert_eq!(built.folders.len(), 1, "the recent project is not a folder");
}
