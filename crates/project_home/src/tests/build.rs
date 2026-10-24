//! `build_*` (docs/design/project_home.md「测试」): the classification rules of
//! `snapshot::build`, with the whole model rendered as text for the
//! `snapshot_*` insta snapshots.

use std::sync::Arc;

use lectern_services::project::{RelPath, Scan};
use lectern_services::review::{OutsideCount, Overview};
use lectern_services::session::{Agent, SessionList, SessionSummary, SourceId, Timestamp};
use lectern_services::usage::UsageState;

use crate::snapshot::render::render;
use crate::snapshot::{Built, GroupKind, Inputs, OutsideRow, SidebarRow, UsageInputs, build};
use crate::testing::{
    DAY, MINUTE, NOW, ProjectFx, branch, entry, folder_key, list, overview, package, progress,
    project_usage, sess, unavailable, usage_limits, wt_key,
};

use lectern_services::project::{Head, ProjectSnapshot};

/// Composes with the test clock.
fn compose(
    project: &ProjectSnapshot,
    lists: &[(&str, Vec<SessionSummary>)],
    overview: Option<&Overview>,
    today: Option<&[(RelPath, u32)]>,
    usage: Option<&UsageInputs>,
) -> Built {
    let lists: Vec<(SourceId, Arc<SessionList>)> = lists
        .iter()
        .map(|(id, sessions)| (SourceId(Arc::from(*id)), list(3, sessions.clone())))
        .collect();
    build(&Inputs {
        project,
        sessions: &lists,
        overview,
        changed_today: today,
        usage,
        now: Timestamp(NOW),
    })
}

fn ids(row_titles: impl Iterator<Item = Arc<str>>) -> Vec<String> {
    row_titles.map(|id| id.to_string()).collect()
}

#[test]
fn build_groups_sessions_by_worktree() {
    let fx = ProjectFx::git("app")
        .linked("feat", branch("feat/x"))
        .prunable("old", branch("old"));
    let key = fx.key();
    let main = fx.wt("main");
    let feat = fx.wt("feat");
    let other_project = folder_key("elsewhere");
    let parent = sess("a", "s1", &key, Some(&main)).key();
    // The two sources interleave in one group, newest first.
    let a = vec![
        sess("a", "s1", &key, Some(&main)).ago(5 * MINUTE).build(),
        sess("a", "s4", &key, Some(&fx.wt("old")))
            .ago(2 * MINUTE)
            .build(),
        sess("a", "s5", &key, Some(&main))
            .child_of(&parent)
            .ago(MINUTE)
            .build(),
        sess("a", "s6", &key, Some(&main)).ago(31 * DAY).build(),
    ];
    let b = vec![
        sess("b", "s2", &key, Some(&main))
            .resolved()
            .untitled()
            .agent(Agent::Codex)
            .ago(20 * MINUTE)
            .build(),
        sess("b", "s3", &key, Some(&feat))
            .agent(Agent::Other(Arc::from("other-agent")))
            .ago(60 * MINUTE)
            .build(),
        sess("b", "s7", &other_project, None).ago(MINUTE).build(),
        sess("b", "s8", &key, Some(&wt_key("app", "gone")))
            .ago(3 * MINUTE)
            .build(),
        sess("b", "s9", &key, None).unplaced().ago(MINUTE).build(),
    ];
    let built = compose(&fx.snapshot(), &[("a", a), ("b", b)], None, None, None);
    let sidebar = &built.snapshot.sidebar;

    let kinds: Vec<&GroupKind> = sidebar.groups.iter().map(|g| &g.kind).collect();
    assert!(matches!(
        kinds.as_slice(),
        [
            GroupKind::Worktree {
                is_active: true,
                ..
            },
            GroupKind::Worktree {
                is_active: false,
                ..
            },
            GroupKind::Other
        ]
    ));
    assert_eq!(
        ids(sidebar.groups[0].sessions.iter().map(|s| s.key.id.clone())),
        ["s1", "s2"],
        "main: Located and Resolved, newest first; the sub-agent and the old session are not rows"
    );
    assert_eq!(
        ids(sidebar.groups[1].sessions.iter().map(|s| s.key.id.clone())),
        ["s3"]
    );
    assert_eq!(
        ids(sidebar.groups[2].sessions.iter().map(|s| s.key.id.clone())),
        ["s4", "s8"],
        "the prunable worktree and the one that is not listed"
    );
    assert_eq!(sidebar.session_rows, 5);
    assert_eq!(built.snapshot.branch.as_deref(), Some("main"));
    insta::assert_snapshot!("snapshot_groups", render(&built.snapshot));
}

#[test]
fn build_splits_needs_review_and_done() {
    let fx = ProjectFx::git("app");
    let key = fx.key();
    let main = fx.wt("main");
    let names = ["none", "partial", "all", "changed", "empty"];
    let sessions: Vec<SessionSummary> = names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            sess("a", name, &key, Some(&main))
                .ago((i as i64 + 1) * MINUTE)
                .build()
        })
        .collect();
    let key_of = |name: &str| {
        sessions
            .iter()
            .find(|s| s.key.id.as_ref() == name)
            .unwrap()
            .key
            .clone()
    };
    let ov = overview(
        &key,
        vec![
            entry(&key_of("none"), None, None, 0),
            entry(&key_of("partial"), progress(2, 5, 0), Some(false), 2),
            entry(&key_of("all"), progress(5, 5, 0), None, 0),
            entry(&key_of("changed"), progress(5, 5, 1), Some(true), 0),
            entry(&key_of("empty"), progress(0, 0, 0), None, 0),
        ],
        OutsideCount::Files(3),
    );
    let built = compose(
        &fx.snapshot(),
        &[("a", sessions.clone())],
        Some(&ov),
        None,
        None,
    );
    let main = &built.snapshot.main;

    assert_eq!(
        ids(main.needs_review.iter().map(|c| c.session.key.id.clone())),
        ["none", "partial", "changed"],
        "no progress, partly seen, and seen but changed since; in the overview's order"
    );
    assert_eq!(
        ids(main.done.iter().map(|c| c.session.key.id.clone())),
        ["all"]
    );
    let changed = &main.needs_review[2];
    assert!(changed.signals.drifted);
    assert_eq!(changed.signals.changed_since_seen, 1);
    assert_eq!(main.needs_review[1].signals.critical, 2);
    assert!(main.needs_review[0].progress.is_none());
    assert!(main.overview_ready);
    assert_eq!(main.outside, OutsideRow::Files(3));
    assert_eq!(built.snapshot.sources.overview, Some(7));
    insta::assert_snapshot!("snapshot_review", render(&built.snapshot));
}

#[test]
fn build_without_overview_has_a_pending_outside_row() {
    let fx = ProjectFx::git("app");
    let built = compose(&fx.snapshot(), &[], None, None, None);
    let main = &built.snapshot.main;
    assert!(!main.overview_ready);
    assert_eq!(main.outside, OutsideRow::Pending);
    assert!(main.needs_review.is_empty() && main.done.is_empty());
    let pending = overview(&fx.key(), Vec::new(), OutsideCount::Pending);
    let built = compose(&fx.snapshot(), &[], Some(&pending), None, None);
    assert!(built.snapshot.main.overview_ready);
    assert_eq!(built.snapshot.main.outside, OutsideRow::Pending);
    let capped = overview(&fx.key(), Vec::new(), OutsideCount::Capped(5000));
    let built = compose(&fx.snapshot(), &[], Some(&capped), None, None);
    assert_eq!(built.snapshot.main.outside, OutsideRow::Capped(5000));
    insta::assert_snapshot!("snapshot_empty", render(&built.snapshot));
}

#[test]
fn build_drops_overview_sessions_missing_from_list() {
    let fx = ProjectFx::git("app");
    let key = fx.key();
    let known = sess("a", "known", &key, Some(&fx.wt("main"))).build();
    let gone = sess("a", "gone", &key, Some(&fx.wt("main"))).build();
    let ov = overview(
        &key,
        vec![
            entry(&gone.key, progress(1, 2, 0), None, 0),
            entry(&known.key, progress(1, 2, 0), None, 0),
        ],
        OutsideCount::Files(0),
    );
    let built = compose(
        &fx.snapshot(),
        &[("a", vec![known.clone()])],
        Some(&ov),
        None,
        None,
    );
    assert_eq!(built.missing, [gone.key]);
    assert_eq!(
        ids(built
            .snapshot
            .main
            .needs_review
            .iter()
            .map(|c| c.session.key.id.clone())),
        ["known"]
    );
}

#[test]
fn build_marks_running_within_ten_minutes() {
    let fx = ProjectFx::git("app");
    let key = fx.key();
    let main = fx.wt("main");
    let sessions = vec![
        sess("a", "fresh", &key, Some(&main))
            .open_turn()
            .ago(MINUTE)
            .build(),
        sess("a", "edge", &key, Some(&main))
            .open_turn()
            .ago(10 * MINUTE)
            .build(),
        sess("a", "stale", &key, Some(&main))
            .open_turn()
            .ago(10 * MINUTE + 1)
            .build(),
        sess("a", "days", &key, Some(&main))
            .open_turn()
            .ago(3 * DAY)
            .build(),
        sess("a", "closed", &key, Some(&main))
            .ago(2 * MINUTE)
            .build(),
        sess("a", "future", &key, Some(&main))
            .open_turn()
            .ago(-5 * MINUTE)
            .build(),
    ];
    let built = compose(&fx.snapshot(), &[("a", sessions)], None, None, None);
    let running: Vec<(String, bool)> = built.snapshot.sidebar.groups[0]
        .sessions
        .iter()
        .map(|s| (s.key.id.to_string(), s.running))
        .collect();
    assert_eq!(
        running,
        [
            ("future".to_owned(), true),
            ("fresh".to_owned(), true),
            ("closed".to_owned(), false),
            ("edge".to_owned(), true),
            ("stale".to_owned(), false),
            ("days".to_owned(), false),
        ]
    );
}

fn packages() -> Vec<lectern_services::project::Package> {
    vec![
        package("", "root", None),
        package("web", "web", Some(0)),
        package("web/ui", "ui", Some(1)),
        package("web-extra", "web-extra", Some(0)),
        package("api", "api", Some(0)),
    ]
}

#[test]
fn build_counts_packages_today() {
    let fx = ProjectFx::git("app").packages(Scan::Ready(packages()));
    let today: Vec<(RelPath, u32)> = vec![
        (Arc::from("web/ui"), 4),
        (Arc::from("api"), 1),
        (Arc::from("gone"), 9),
    ];

    let with = compose(&fx.snapshot(), &[], None, Some(&today), None);
    let sidebar = &with.snapshot.sidebar;
    assert!(sidebar.package_counts);
    let rows: Vec<(&str, u8, u32)> = sidebar
        .packages
        .iter()
        .map(|p| (&*p.dir, p.depth, p.changed_today))
        .collect();
    assert_eq!(
        rows,
        [
            ("", 0, 0),
            ("api", 1, 1),
            ("web", 1, 0),
            ("web/ui", 2, 4),
            ("web-extra", 1, 0)
        ],
        "directory order by path segment, so `web/ui` follows `web` and not `web-extra`"
    );
    insta::assert_snapshot!("snapshot_packages", render(&with.snapshot));

    let without = compose(&fx.snapshot(), &[], None, None, None);
    assert!(!without.snapshot.sidebar.package_counts);
    assert!(
        without
            .snapshot
            .sidebar
            .packages
            .iter()
            .all(|p| p.changed_today == 0)
    );

    let capped = overview(&fx.key(), Vec::new(), OutsideCount::Capped(9000));
    let capped = compose(&fx.snapshot(), &[], Some(&capped), Some(&today), None);
    assert!(
        !capped.snapshot.sidebar.package_counts,
        "a capped overview has no change set"
    );
    assert!(
        capped
            .snapshot
            .sidebar
            .packages
            .iter()
            .all(|p| p.changed_today == 0)
    );
}

#[test]
fn build_shows_packages_by_scan_state() {
    let pending = ProjectFx::git("app").packages(Scan::Pending);
    let built = compose(&pending.snapshot(), &[], None, None, None);
    assert_eq!(
        built.snapshot.sidebar.rows.as_ref(),
        [
            SidebarRow::SessionsHeader,
            SidebarRow::SessionsEmpty,
            SidebarRow::PackagesHeader,
            SidebarRow::PackagesSkeleton
        ]
    );
    for fx in [
        ProjectFx::git("app").packages(Scan::Unavailable(unavailable())),
        ProjectFx::git("app").packages(Scan::Ready(Vec::new())),
        ProjectFx::git("app").no_active_tree(),
    ] {
        let built = compose(&fx.snapshot(), &[], None, None, None);
        assert_eq!(
            built.snapshot.sidebar.rows.as_ref(),
            [SidebarRow::SessionsHeader, SidebarRow::SessionsEmpty],
            "no sub-projects, no section"
        );
    }
}

#[test]
fn build_folder_project_hides_outside() {
    let fx = ProjectFx::folder("notes");
    let key = fx.key();
    let sessions = vec![
        sess("a", "f1", &key, None).ago(MINUTE).build(),
        sess("a", "f2", &key, None).ago(2 * MINUTE).build(),
    ];
    let ov = overview(&key, Vec::new(), OutsideCount::Files(3));
    let built = compose(&fx.snapshot(), &[("a", sessions)], Some(&ov), None, None);
    let snapshot = &built.snapshot;
    assert_eq!(snapshot.main.outside, OutsideRow::Hidden);
    assert!(snapshot.branch.is_none());
    assert_eq!(snapshot.sidebar.groups.len(), 1);
    assert_eq!(snapshot.sidebar.groups[0].kind, GroupKind::Folder);
    assert!(
        !snapshot
            .sidebar
            .rows
            .iter()
            .any(|r| matches!(r, SidebarRow::WorktreeHeader(_))),
        "a folder has no worktree heading"
    );
    insta::assert_snapshot!("snapshot_folder", render(snapshot));
}

#[test]
fn build_without_usage() {
    let fx = ProjectFx::git("app");
    let built = compose(&fx.snapshot(), &[], None, None, None);
    assert!(built.snapshot.usage.is_none());
    assert_eq!(built.snapshot.sources.usage, None);
}

#[test]
fn build_with_usage() {
    let fx = ProjectFx::git("app");
    let ready = UsageInputs {
        snapshot: Some(project_usage(UsageState::Ready, 12_345)),
        limits: usage_limits(
            vec![(Agent::ClaudeCode, 20.0), (Agent::Codex, 33.6)],
            Some(1),
        ),
    };
    let built = compose(&fx.snapshot(), &[], None, None, Some(&ready));
    let usage = built.snapshot.usage.as_ref().unwrap();
    assert_eq!(usage.tokens_7d, Some(12_345));
    let tightest = usage.tightest.as_ref().unwrap();
    assert_eq!(
        (tightest.agent.clone(), tightest.used_percent),
        (Agent::Codex, 34)
    );
    assert_eq!(built.snapshot.sources.usage, Some(9));
    insta::assert_snapshot!("snapshot_usage", render(&built.snapshot));

    // Not computed yet, no limit, a percentage out of range, an index out of range.
    let loading = UsageInputs {
        snapshot: Some(project_usage(UsageState::Loading, 0)),
        limits: usage_limits(vec![(Agent::Codex, 140.0)], Some(0)),
    };
    let usage = compose(&fx.snapshot(), &[], None, None, Some(&loading))
        .snapshot
        .usage
        .unwrap();
    assert_eq!(usage.tokens_7d, None, "a loading snapshot has no total yet");
    assert_eq!(usage.tightest.unwrap().used_percent, 100);
    let none = UsageInputs {
        snapshot: None,
        limits: usage_limits(vec![(Agent::Codex, 10.0)], Some(5)),
    };
    let built = compose(&fx.snapshot(), &[], None, None, Some(&none));
    let usage = built.snapshot.usage.unwrap();
    assert_eq!((usage.tokens_7d, usage.tightest), (None, None));
    assert_eq!(
        built.snapshot.sources.usage,
        Some(4),
        "the limits' generation"
    );
}

#[test]
fn build_flattens_sidebar_rows() {
    let mut packages = vec![package("", "root", None)];
    for i in 0..1000 {
        packages.push(package(&format!("pkg{i:04}"), "p", Some(0)));
        packages.push(package(&format!("pkg{i:04}/sub"), "sub", Some(1 + 2 * i)));
    }
    let fx = ProjectFx::git("app")
        .linked("feat", branch("feat"))
        .packages(Scan::Ready(packages));
    let key = fx.key();
    let mut sessions = Vec::new();
    for i in 0..300 {
        let worktree = if i % 3 == 0 {
            fx.wt("feat")
        } else {
            fx.wt("main")
        };
        sessions.push(
            sess("a", &format!("s{i:04}"), &key, Some(&worktree))
                .ago(i * MINUTE)
                .build(),
        );
    }
    let built = compose(&fx.snapshot(), &[("a", sessions)], None, None, None);
    let sidebar = &built.snapshot.sidebar;
    assert_eq!(
        sidebar.packages.len(),
        2001,
        "the root and 2000 sub-projects"
    );
    assert_eq!(sidebar.session_rows, 300);
    // header, 2 groups with a heading each, 300 sessions, header, 2001 packages.
    assert_eq!(sidebar.rows.len(), 1 + 2 + 300 + 1 + 2001);
    assert_eq!(sidebar.rows[0], SidebarRow::SessionsHeader);
    assert_eq!(sidebar.rows[1], SidebarRow::WorktreeHeader(0));
    assert_eq!(sidebar.rows[2], SidebarRow::Session { group: 0, index: 0 });
    let first_package = sidebar
        .rows
        .iter()
        .position(|r| *r == SidebarRow::PackagesHeader)
        .unwrap();
    assert_eq!(first_package, 1 + 2 + 300);
    assert_eq!(sidebar.rows[first_package + 1], SidebarRow::Package(0));
    assert_eq!(sidebar.rows.last(), Some(&SidebarRow::Package(2000)));
    // Every index points at a row that exists.
    for row in sidebar.rows.iter() {
        match *row {
            SidebarRow::WorktreeHeader(g) => assert!((g as usize) < sidebar.groups.len()),
            SidebarRow::Session { group, index } => {
                assert!((index as usize) < sidebar.groups[group as usize].sessions.len());
            }
            SidebarRow::Package(i) => assert!((i as usize) < sidebar.packages.len()),
            _ => {}
        }
    }
    assert_eq!(sidebar.packages[1].dir.as_ref(), "pkg0000");
    assert_eq!(sidebar.packages[2].dir.as_ref(), "pkg0000/sub");
    assert_eq!(sidebar.packages[2].depth, 2);
}

#[test]
fn build_asks_to_complete_each_pending_session_once() {
    let fx = ProjectFx::git("app");
    let key = fx.key();
    let main = fx.wt("main");
    let a = sess("a", "a", &key, Some(&main))
        .pending()
        .ago(MINUTE)
        .build();
    let b = sess("a", "b", &key, Some(&main)).ago(2 * MINUTE).build();
    let c = sess("a", "c", &key, Some(&main))
        .pending()
        .ago(3 * MINUTE)
        .build();
    let ov = overview(
        &key,
        vec![
            entry(&a.key, progress(0, 2, 0), None, 0),
            entry(&b.key, progress(0, 2, 0), None, 0),
        ],
        OutsideCount::Files(0),
    );
    let built = compose(
        &fx.snapshot(),
        &[("a", vec![a.clone(), b, c.clone()])],
        Some(&ov),
        None,
        None,
    );
    assert_eq!(
        built.pending,
        [a.key, c.key],
        "a shows in the sidebar and as a card and is listed once"
    );
}

#[test]
fn build_names_the_branch() {
    let detached = Head::Detached(Arc::from("0123456789abcdef0123456789abcdef01234567"));
    let fx = ProjectFx::git("app").head("main", detached);
    let built = compose(&fx.snapshot(), &[], None, None, None);
    assert_eq!(built.snapshot.branch.as_deref(), Some("01234567"));
    let fx = ProjectFx::git("app").head("main", Head::Unborn(Arc::from("refs/heads/trunk")));
    let built = compose(&fx.snapshot(), &[], None, None, None);
    assert_eq!(built.snapshot.branch.as_deref(), Some("trunk"));
    let fx = ProjectFx::git("app")
        .linked("feat", branch("feat/x"))
        .active_in("feat");
    let built = compose(&fx.snapshot(), &[], None, None, None);
    assert_eq!(built.snapshot.branch.as_deref(), Some("feat/x"));
    assert_eq!(built.snapshot.active_worktree, Some(fx.wt("feat")));
}

#[test]
fn build_settles_the_files_once_they_are_not_pending() {
    for (files, settled) in [
        (Scan::Pending, false),
        (Scan::Ready(()), true),
        (Scan::Unavailable(unavailable()), true),
    ] {
        let fx = ProjectFx::git("app").files(files);
        let built = compose(&fx.snapshot(), &[], None, None, None);
        assert_eq!(built.snapshot.files_settled, settled);
    }
    let fx = ProjectFx::git("app").no_active_tree();
    assert!(
        !compose(&fx.snapshot(), &[], None, None, None)
            .snapshot
            .files_settled
    );
}

#[test]
fn build_has_no_key_clash_between_sources() {
    // The same session ID under two sources is two sessions.
    let fx = ProjectFx::git("app");
    let key = fx.key();
    let main = fx.wt("main");
    let a = sess("a", "same", &key, Some(&main)).build();
    let b = sess("b", "same", &key, Some(&main)).build();
    assert_ne!(a.key, b.key);
    let built = compose(
        &fx.snapshot(),
        &[("a", vec![a]), ("b", vec![b])],
        None,
        None,
        None,
    );
    assert_eq!(built.snapshot.sidebar.session_rows, 2);
}
