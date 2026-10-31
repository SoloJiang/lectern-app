//! `build_*` (docs/design/worktrees.md「测试」): the composition rules of
//! `snapshot::build`, with the whole model rendered as text for the
//! `snapshot_*` insta snapshots.

use std::sync::Arc;

use lectern_services::project::{Head, ProjectSnapshot, WorktreeKey};
use lectern_services::review::{ChangeCount, WorktreesOverview};
use lectern_services::session::{SessionList, SessionSummary, SourceId, Timestamp};
use lectern_services::usage::{UsageQuery, UsageSnapshot, UsageState};

use crate::snapshot::render::render;
use crate::snapshot::{
    Built, HeadLabel, Inputs, MrItem, Overlaps, RowUsage, UsageInputs, WorktreeRow, build,
};
use crate::testing::{
    DAY, MINUTE, NOW, ProjectFx, branch, branch_usage, changes_of, counted, fixture_1000,
    folder_key, list, mr_label, mr_ref, mr_snapshot, overlap, overview_of, sess, tokens,
    worktree_query, worktree_snapshot, wt_key,
};

/// Composes with the test clock.
fn compose(
    project: &ProjectSnapshot,
    lists: &[(&str, Vec<SessionSummary>)],
    changes: Option<&WorktreesOverview>,
    usage: Option<&UsageInputs>,
) -> Built {
    let lists: Vec<(SourceId, Arc<SessionList>)> = lists
        .iter()
        .map(|(id, sessions)| (SourceId(Arc::from(*id)), list(3, sessions.clone())))
        .collect();
    build(&Inputs {
        project,
        sessions: &lists,
        changes,
        usage,
        switches: 0,
        usage_generation: 0,
        now: Timestamp(NOW),
    })
}

fn row<'a>(built: &'a Built, worktree: &WorktreeKey) -> &'a WorktreeRow {
    built
        .snapshot
        .active_rows
        .iter()
        .chain(built.snapshot.idle_rows.iter())
        .find(|row| row.key == *worktree)
        .expect("the worktree has a row")
}

fn keys(rows: &[WorktreeRow]) -> Vec<String> {
    rows.iter()
        .map(|row| row.root.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

fn ids(row: &WorktreeRow) -> Vec<String> {
    row.recent.iter().map(|s| s.key.id.to_string()).collect()
}

#[test]
fn build_one_row_per_worktree_in_project_order() {
    let fx = ProjectFx::git("app")
        .linked("alpha", branch("feat/a"))
        .prunable("gone", branch("gone"))
        .linked("zed", branch("zed"))
        .locked("alpha")
        .active_in("alpha");
    let key = fx.key();
    let sessions = vec![
        // Running in `zed`: that row is Active.
        sess("a", "zed-run", &key, Some(&fx.wt("zed")))
            .open_turn()
            .ago(2 * MINUTE)
            .build(),
        // Running in a prunable worktree: still Idle, and last.
        sess("a", "gone-run", &key, Some(&fx.wt("gone")))
            .open_turn()
            .ago(MINUTE)
            .build(),
    ];
    let built = compose(&fx.snapshot(), &[("a", sessions)], None, None);
    let snapshot = &built.snapshot;
    assert_eq!(keys(&snapshot.active_rows), ["app-zed"]);
    assert_eq!(
        keys(&snapshot.idle_rows),
        ["app-main", "app-alpha", "app-gone"],
        "the project's order, the prunable one last"
    );
    let alpha = row(&built, &fx.wt("alpha"));
    assert!(alpha.is_active && alpha.locked && !alpha.is_main && !alpha.prunable);
    assert!(row(&built, &fx.wt("main")).is_main);
    assert!(!row(&built, &fx.wt("main")).is_active);
    let gone = row(&built, &fx.wt("gone"));
    assert!(gone.prunable);
    assert_eq!((gone.sessions, gone.running), (1, 1));
    assert_eq!(snapshot.active, Some(fx.wt("alpha")));
    assert!(snapshot.files_settled);
    assert!(!snapshot.folder);
    assert_eq!(&*alpha.root, std::path::Path::new("/fixture/app-alpha"));
    assert_eq!(
        alpha.node.as_str(),
        "worktrees.list.item[/fixture/app/.git/worktrees/alpha]"
    );
    insta::assert_snapshot!("snapshot_rows", render(&built));
}

#[test]
fn build_counts_sessions_per_worktree() {
    let fx = ProjectFx::git("app").linked("feat", branch("feat"));
    let key = fx.key();
    let main = fx.wt("main");
    let feat = fx.wt("feat");
    let elsewhere = folder_key("elsewhere");
    let parent = sess("a", "p", &key, Some(&main)).key();
    let sessions = vec![
        // Located and resolved both count.
        sess("a", "p", &key, Some(&main)).ago(5 * MINUTE).build(),
        sess("a", "resolved", &key, Some(&main))
            .resolved()
            .ago(6 * MINUTE)
            .build(),
        // Exactly 14 days counts, a millisecond more does not.
        sess("a", "edge", &key, Some(&main)).ago(14 * DAY).build(),
        sess("a", "old", &key, Some(&main))
            .ago(14 * DAY + 1)
            .build(),
        // A sub-agent session in the parent's worktree is merged into it.
        sess("a", "child", &key, Some(&main))
            .child_of(&parent)
            .ago(MINUTE)
            .build(),
        // A sub-agent session in another worktree counts there.
        sess("a", "other-wt", &key, Some(&feat))
            .child_of(&parent)
            .ago(2 * MINUTE)
            .build(),
        // A sub-agent session whose parent is not in any list counts alone.
        sess("a", "orphan", &key, Some(&feat))
            .child_of(&sess("a", "nowhere", &key, Some(&feat)).key())
            .ago(3 * MINUTE)
            .build(),
        // Not placed, placed nowhere in the project, or in another project.
        sess("a", "pending", &key, Some(&main)).unplaced().build(),
        sess("a", "no-worktree", &key, None).build(),
        sess("a", "foreign", &elsewhere, None).build(),
    ];
    let built = compose(&fx.snapshot(), &[("a", sessions)], None, None);
    let main_row = row(&built, &main);
    assert_eq!(main_row.sessions, 3, "p (with child), resolved, edge");
    assert_eq!(ids(main_row), ["p", "resolved", "edge"]);
    let feat_row = row(&built, &feat);
    assert_eq!(feat_row.sessions, 2);
    assert_eq!(ids(feat_row), ["other-wt", "orphan"]);
    assert_eq!(built.snapshot.sources.sessions.len(), 1);
}

#[test]
fn build_merges_a_child_only_into_a_parent_that_counts() {
    let fx = ProjectFx::git("app");
    let key = fx.key();
    let main = fx.wt("main");
    let old_parent = sess("a", "old-parent", &key, Some(&main)).ago(20 * DAY);
    let grand = sess("a", "grand", &key, Some(&main)).ago(5 * MINUTE);
    let mid = sess("a", "mid", &key, Some(&main))
        .child_of(&grand.key())
        .ago(4 * MINUTE);
    let sessions = vec![
        old_parent.build(),
        // The parent is older than 14 days: the child counts on its own.
        sess("a", "child-of-old", &key, Some(&main))
            .child_of(&sess("a", "old-parent", &key, Some(&main)).key())
            .ago(MINUTE)
            .build(),
        // A chain merges into its first session.
        grand.build(),
        mid.build(),
        sess("a", "leaf", &key, Some(&main))
            .child_of(&sess("a", "mid", &key, Some(&main)).key())
            .open_turn()
            .ago(MINUTE)
            .build(),
        // A parent that is not placed yet does not absorb its child.
        sess("a", "unplaced-parent", &key, Some(&main))
            .unplaced()
            .build(),
        sess("a", "child-of-unplaced", &key, Some(&main))
            .child_of(&sess("a", "unplaced-parent", &key, Some(&main)).key())
            .ago(7 * MINUTE)
            .build(),
    ];
    let built = compose(&fx.snapshot(), &[("a", sessions)], None, None);
    let main_row = row(&built, &main);
    assert_eq!(
        ids(main_row),
        ["child-of-old", "grand", "child-of-unplaced"],
        "newest first; the chain is one session"
    );
    assert_eq!(main_row.sessions, 3);
    assert!(
        main_row.recent[1].running,
        "the leaf of the chain is running, so its first session is"
    );
    assert_eq!(main_row.running, 1);
}

#[test]
fn build_cuts_a_cycle_of_parents() {
    let fx = ProjectFx::git("app");
    let key = fx.key();
    let main = fx.wt("main");
    let a = sess("a", "a", &key, Some(&main));
    let b = sess("a", "b", &key, Some(&main));
    let (ka, kb) = (a.key(), b.key());
    let sessions = vec![
        a.child_of(&kb).ago(MINUTE).build(),
        b.child_of(&ka).ago(2 * MINUTE).build(),
    ];
    let built = compose(&fx.snapshot(), &[("a", sessions)], None, None);
    assert!(
        row(&built, &main).sessions >= 1,
        "a damaged chain still terminates and counts something"
    );
}

#[test]
fn build_active_means_running() {
    let fx = ProjectFx::git("app")
        .linked("ten", branch("ten"))
        .linked("eleven", branch("eleven"))
        .linked("done", branch("done"))
        .linked("sub", branch("sub"));
    let key = fx.key();
    let parent = sess("a", "parent", &key, Some(&fx.wt("sub")));
    let parent_key = parent.key();
    let sessions = vec![
        sess("a", "ten", &key, Some(&fx.wt("ten")))
            .open_turn()
            .ago(10 * MINUTE)
            .build(),
        sess("a", "eleven", &key, Some(&fx.wt("eleven")))
            .open_turn()
            .ago(10 * MINUTE + 1)
            .build(),
        sess("a", "done", &key, Some(&fx.wt("done")))
            .ago(MINUTE)
            .build(),
        // The parent's turn ended; its sub-agent session is still running.
        parent.ago(3 * MINUTE).build(),
        sess("a", "sub-child", &key, Some(&fx.wt("sub")))
            .child_of(&parent_key)
            .open_turn()
            .ago(MINUTE)
            .build(),
    ];
    let built = compose(&fx.snapshot(), &[("a", sessions)], None, None);
    assert_eq!(keys(&built.snapshot.active_rows), ["app-ten", "app-sub"]);
    assert_eq!(
        keys(&built.snapshot.idle_rows),
        ["app-main", "app-eleven", "app-done"]
    );
    let sub = row(&built, &fx.wt("sub"));
    assert_eq!((sub.sessions, sub.running), (1, 1));
    assert!(sub.recent[0].running, "the parent's entry is running");
    assert_eq!(sub.recent[0].last_activity, Timestamp(NOW - 3 * MINUTE));
    assert_eq!(row(&built, &fx.wt("eleven")).running, 0);
}

#[test]
fn build_next_expiry_is_the_earliest() {
    let fx = ProjectFx::git("app").linked("feat", branch("feat"));
    let key = fx.key();
    let main = fx.wt("main");
    let feat = fx.wt("feat");
    let none = compose(
        &fx.snapshot(),
        &[(
            "a",
            vec![sess("a", "idle", &key, Some(&main)).ago(MINUTE).build()],
        )],
        None,
        None,
    );
    assert_eq!(none.snapshot.next_expiry, None, "nothing is running");

    let parent = sess("a", "p", &key, Some(&feat));
    let parent_key = parent.key();
    let sessions = vec![
        sess("a", "a", &key, Some(&main))
            .open_turn()
            .ago(2 * MINUTE)
            .build(),
        parent.ago(MINUTE).build(),
        // A merged sub-agent session expires before the others.
        sess("a", "c", &key, Some(&feat))
            .child_of(&parent_key)
            .open_turn()
            .ago(9 * MINUTE)
            .build(),
        // Not running: its time does not count.
        sess("a", "stale", &key, Some(&main))
            .open_turn()
            .ago(11 * MINUTE)
            .build(),
    ];
    let built = compose(&fx.snapshot(), &[("a", sessions)], None, None);
    assert_eq!(
        built.snapshot.next_expiry,
        Some(Timestamp(NOW - 9 * MINUTE + 10 * MINUTE + 1)),
        "the first instant at which the merged child is no longer running"
    );
    // At that instant the session is no longer running.
    let at = built.snapshot.next_expiry.unwrap();
    assert!(at.0 - (NOW - 9 * MINUTE) > 10 * MINUTE);
}

#[test]
fn build_changes_follow_the_overview() {
    let fx = ProjectFx::git("app")
        .linked("pend", branch("pend"))
        .linked("capped", branch("capped"))
        .linked("unborn", Head::Unborn(Arc::from("refs/heads/fresh")))
        .prunable("gone", branch("gone"));
    let key = fx.key();
    let overview = overview_of(
        &key,
        Some("refs/remotes/origin/main"),
        vec![
            counted(&fx.wt("main"), 1, 2, 3, vec![]),
            changes_of(
                &fx.wt("capped"),
                ChangeCount::Capped,
                ChangeCount::Files(4),
                ChangeCount::Capped,
                None,
            ),
            changes_of(
                &fx.wt("unborn"),
                ChangeCount::Files(2),
                ChangeCount::Unavailable,
                ChangeCount::Unavailable,
                Some(vec![]),
            ),
        ],
    );
    let built = compose(&fx.snapshot(), &[], Some(&overview), None);
    let base = built.snapshot.base.clone().unwrap();
    assert_eq!(&*base.full, "refs/remotes/origin/main");
    assert_eq!(&*base.short, "origin/main");
    assert_eq!(built.snapshot.sources.changes, Some(5));

    let changes = row(&built, &fx.wt("main")).changes;
    assert_eq!(
        (changes.uncommitted, changes.committed, changes.vs_base),
        (
            ChangeCount::Files(1),
            ChangeCount::Files(2),
            ChangeCount::Files(3)
        )
    );
    assert_eq!(changes.counted_at, Some(Timestamp(NOW - 2 * MINUTE)));
    let pend = row(&built, &fx.wt("pend")).changes;
    assert_eq!(
        (pend.uncommitted, pend.committed, pend.vs_base),
        (
            ChangeCount::Pending,
            ChangeCount::Pending,
            ChangeCount::Pending
        ),
        "no entry yet"
    );
    assert_eq!(pend.counted_at, None);
    let capped = row(&built, &fx.wt("capped")).changes;
    assert_eq!(capped.uncommitted, ChangeCount::Capped);
    let gone = row(&built, &fx.wt("gone")).changes;
    assert_eq!(
        (gone.uncommitted, gone.committed, gone.vs_base),
        (
            ChangeCount::Unavailable,
            ChangeCount::Unavailable,
            ChangeCount::Unavailable
        )
    );
    assert_eq!(gone.counted_at, None);
    insta::assert_snapshot!("snapshot_changes", render(&built));

    // Without a base, and before the overview has arrived.
    let no_base = overview_of(&key, None, vec![counted(&fx.wt("main"), 1, 0, 0, vec![])]);
    let built = compose(&fx.snapshot(), &[], Some(&no_base), None);
    assert!(built.snapshot.base.is_none());
    let built = compose(&fx.snapshot(), &[], None, None);
    assert!(built.snapshot.base.is_none());
    assert_eq!(built.snapshot.sources.changes, None);
    assert_eq!(
        row(&built, &fx.wt("main")).changes.uncommitted,
        ChangeCount::Pending
    );
    // A short name for a local base.
    let local = overview_of(&key, Some("refs/heads/main"), vec![]);
    let built = compose(&fx.snapshot(), &[], Some(&local), None);
    assert_eq!(&*built.snapshot.base.unwrap().short, "main");
}

#[test]
fn build_overlaps_drop_unknown_worktrees() {
    let fx = ProjectFx::git("app")
        .linked("feat", branch("feat/x"))
        .linked("big", branch("big"))
        .linked("fresh", branch("fresh"))
        .linked("late", branch("late"))
        .prunable("gone", branch("gone"));
    let key = fx.key();
    let stranger = wt_key("other-project", "x");
    let overview = overview_of(
        &key,
        Some("refs/heads/main"),
        vec![
            counted(
                &fx.wt("main"),
                2,
                1,
                3,
                vec![
                    overlap(&fx.wt("feat"), 2, &["a.rs", "b.rs"]),
                    // Not in the project: dropped.
                    overlap(&stranger, 1, &["c.rs"]),
                    overlap(&fx.wt("big"), 1, &["d.rs"]),
                ],
            ),
            counted(&fx.wt("feat"), 2, 0, 2, vec![]),
            // Too many changes: the counts are there, the paths are not.
            changes_of(
                &fx.wt("big"),
                ChangeCount::Capped,
                ChangeCount::Capped,
                ChangeCount::Capped,
                None,
            ),
            // The first comparison is not done: the counts are pending.
            changes_of(
                &fx.wt("fresh"),
                ChangeCount::Pending,
                ChangeCount::Pending,
                ChangeCount::Pending,
                None,
            ),
            // The current worktree before the overview's first round.
            changes_of(
                &fx.wt("late"),
                ChangeCount::Pending,
                ChangeCount::Files(1),
                ChangeCount::Pending,
                None,
            ),
        ],
    );
    let built = compose(&fx.snapshot(), &[], Some(&overview), None);
    let Overlaps::List(list) = &row(&built, &fx.wt("main")).overlaps else {
        panic!("a list");
    };
    assert_eq!(list.len(), 2, "the stranger is dropped");
    assert_eq!(list[0].other, fx.wt("feat"));
    assert_eq!(list[0].files, 2);
    assert_eq!(list[0].other_head, HeadLabel::Branch(Arc::from("feat/x")));
    assert_eq!(&*list[0].paths[1], "b.rs");
    assert_eq!(
        list[0].node.as_str(),
        "worktrees.detail.overlaps.item[/fixture/app/.git/worktrees/feat]"
    );
    assert_eq!(list[1].other, fx.wt("big"));
    assert_eq!(
        row(&built, &fx.wt("feat")).overlaps,
        Overlaps::List(Arc::from(Vec::new())),
        "an empty list is known: nothing in common"
    );
    assert_eq!(row(&built, &fx.wt("big")).overlaps, Overlaps::Unknown);
    assert_eq!(row(&built, &fx.wt("fresh")).overlaps, Overlaps::Pending);
    assert_eq!(row(&built, &fx.wt("late")).overlaps, Overlaps::Pending);
    assert_eq!(
        row(&built, &fx.wt("gone")).overlaps,
        Overlaps::Unknown,
        "review does not compare a prunable worktree"
    );
    let built = compose(&fx.snapshot(), &[], None, None);
    assert_eq!(row(&built, &fx.wt("main")).overlaps, Overlaps::Pending);
}

fn usage_with(
    project: &ProjectFx,
    entries: Vec<(&str, UsageSnapshot)>,
    mrs: Vec<(u64, UsageSnapshot)>,
) -> UsageInputs {
    UsageInputs {
        worktrees: entries
            .into_iter()
            .map(|(name, snapshot)| (project.wt(name), Arc::new(snapshot)))
            .collect(),
        mrs: mrs
            .into_iter()
            .map(|(number, snapshot)| (mr_ref(number), Arc::new(snapshot)))
            .collect(),
    }
}

#[test]
fn build_usage_branch_mr_and_not_counted() {
    let fx = ProjectFx::git("app")
        .linked("feat", branch("feat"))
        .linked("loading", branch("loading"))
        .linked("nothing", branch("nothing"))
        .linked("back", branch("back"))
        .linked("one", branch("one"))
        .linked("two", branch("two"))
        .linked("det", Head::Detached(Arc::from("0123456789abcdef0123")))
        .prunable("gone", branch("gone"));
    let key = fx.key();
    let mut inherited = branch_usage(UsageState::Ready, Some(tokens(300, 120)), vec![]);
    inherited.header.may_include_inherited = true;
    let usage = usage_with(
        &fx,
        vec![
            // The default branch: not counted.
            (
                "main",
                worktree_snapshot(branch_usage(UsageState::Ready, None, vec![])),
            ),
            ("feat", worktree_snapshot(inherited)),
            (
                "loading",
                worktree_snapshot(branch_usage(
                    UsageState::Loading,
                    Some(tokens(1, 1)),
                    vec![],
                )),
            ),
            (
                "back",
                worktree_snapshot(branch_usage(
                    UsageState::Backfilling { done: 1, total: 4 },
                    Some(tokens(7, 3)),
                    vec![],
                )),
            ),
            (
                "one",
                worktree_snapshot(branch_usage(
                    UsageState::Ready,
                    Some(tokens(40, 2)),
                    vec![mr_label(11, "Add the thing")],
                )),
            ),
            (
                "two",
                worktree_snapshot(branch_usage(
                    UsageState::Ready,
                    Some(tokens(900, 100)),
                    vec![
                        mr_label(21, "First"),
                        mr_label(22, "Second"),
                        mr_label(23, "Third"),
                    ],
                )),
            ),
            // A snapshot of another kind answers the wrong question: pending.
            (
                "det",
                mr_snapshot(1, UsageState::Ready, tokens(1, 1), vec![]),
            ),
        ],
        vec![
            (
                21,
                mr_snapshot(21, UsageState::Ready, tokens(500, 50), vec![22, 23]),
            ),
            (
                22,
                mr_snapshot(22, UsageState::Ready, tokens(200, 20), vec![21]),
            ),
            // The snapshot of #23 is not there yet.
        ],
    );
    let built = compose(&fx.snapshot(), &[], None, Some(&usage));

    assert_eq!(row(&built, &fx.wt("main")).usage, RowUsage::NotCounted);
    let RowUsage::Branch(feat) = &row(&built, &fx.wt("feat")).usage else {
        panic!("branch usage");
    };
    assert_eq!(feat.cumulative, tokens(300, 120));
    assert_eq!(feat.review, tokens(10, 5));
    assert_eq!(feat.since, Some(Timestamp(NOW - 3 * DAY)));
    assert!(feat.may_include_inherited && !feat.backfilling);
    assert_eq!(row(&built, &fx.wt("loading")).usage, RowUsage::Pending);
    assert_eq!(
        row(&built, &fx.wt("nothing")).usage,
        RowUsage::Pending,
        "no snapshot yet"
    );
    let RowUsage::Branch(back) = &row(&built, &fx.wt("back")).usage else {
        panic!("branch usage");
    };
    assert!(back.backfilling);
    assert_eq!(row(&built, &fx.wt("det")).usage, RowUsage::Pending);
    assert_eq!(row(&built, &fx.wt("gone")).usage, RowUsage::Absent);

    let RowUsage::Mrs(one) = &row(&built, &fx.wt("one")).usage else {
        panic!("one MR");
    };
    assert_eq!(
        one.as_ref(),
        [MrItem {
            label: mr_label(11, "Add the thing"),
            tokens: Some(tokens(40, 2)),
            shared_with: 0,
        }],
        "one MR takes the worktree's cumulative"
    );
    let RowUsage::Mrs(two) = &row(&built, &fx.wt("two")).usage else {
        panic!("three MRs");
    };
    assert_eq!(
        two.iter()
            .map(|item| (item.label.mr.number, item.tokens, item.shared_with))
            .collect::<Vec<_>>(),
        [
            (21, Some(tokens(500, 50)), 2),
            (22, Some(tokens(200, 20)), 1),
            (23, None, 0),
        ],
        "each MR takes its own write, the missing one has none yet"
    );

    // One query per worktree that is not prunable, and one per MR of the
    // worktree whose branch is in several.
    assert_eq!(
        built.queries.iter().cloned().collect::<Vec<_>>(),
        [
            worktree_query(&key, &fx.wt("main")),
            worktree_query(&key, &fx.wt("feat")),
            worktree_query(&key, &fx.wt("loading")),
            worktree_query(&key, &fx.wt("nothing")),
            worktree_query(&key, &fx.wt("back")),
            worktree_query(&key, &fx.wt("one")),
            worktree_query(&key, &fx.wt("two")),
            UsageQuery::Mr { mr: mr_ref(21) },
            UsageQuery::Mr { mr: mr_ref(22) },
            UsageQuery::Mr { mr: mr_ref(23) },
            worktree_query(&key, &fx.wt("det")),
        ]
    );
    insta::assert_snapshot!("snapshot_usage", render(&built));
}

#[test]
fn build_queries_an_mr_once() {
    let fx = ProjectFx::git("app").linked("a", branch("a"));
    let both = vec![mr_label(5, "Shared"), mr_label(6, "Other")];
    let usage = usage_with(
        &fx,
        vec![
            (
                "main",
                worktree_snapshot(branch_usage(
                    UsageState::Ready,
                    Some(tokens(1, 1)),
                    both.clone(),
                )),
            ),
            (
                "a",
                worktree_snapshot(branch_usage(UsageState::Ready, Some(tokens(1, 1)), both)),
            ),
        ],
        vec![],
    );
    let built = compose(&fx.snapshot(), &[], None, Some(&usage));
    let mr_queries = built
        .queries
        .iter()
        .filter(|query| matches!(query, UsageQuery::Mr { .. }))
        .count();
    assert_eq!(mr_queries, 2, "#5 and #6 once each");
}

#[test]
fn build_without_usage() {
    let fx = ProjectFx::git("app").linked("feat", branch("feat"));
    let built = compose(&fx.snapshot(), &[], None, None);
    for row in built
        .snapshot
        .active_rows
        .iter()
        .chain(built.snapshot.idle_rows.iter())
    {
        assert_eq!(row.usage, RowUsage::Absent);
    }
    assert_eq!(
        built.queries.len(),
        2,
        "the queries are handed out all the same: the link keeps them for a usage plugin that comes later"
    );
}

#[test]
fn build_head_labels() {
    let fx = ProjectFx::git("app")
        .linked("slash", branch("feat/deep/name"))
        .linked(
            "det",
            Head::Detached(Arc::from("0123456789abcdef0123456789abcdef01234567")),
        )
        .linked("short", Head::Detached(Arc::from("abc")))
        .linked("unborn", Head::Unborn(Arc::from("refs/heads/fresh")))
        .linked("unknown", Head::Unknown);
    let built = compose(&fx.snapshot(), &[], None, None);
    let label = |name: &str| row(&built, &fx.wt(name)).head.clone();
    assert_eq!(label("main"), HeadLabel::Branch(Arc::from("main")));
    assert_eq!(
        label("slash"),
        HeadLabel::Branch(Arc::from("feat/deep/name"))
    );
    assert_eq!(label("det"), HeadLabel::Detached(Arc::from("01234567")));
    assert_eq!(label("short"), HeadLabel::Detached(Arc::from("abc")));
    assert_eq!(label("unborn"), HeadLabel::Unborn(Arc::from("fresh")));
    assert_eq!(label("unknown"), HeadLabel::Unknown);
}

#[test]
fn build_requests_complete_once() {
    let fx = ProjectFx::git("app").linked("feat", branch("feat"));
    let key = fx.key();
    let main = fx.wt("main");
    let mut sessions: Vec<SessionSummary> = (0..7)
        .map(|i| {
            sess("a", &format!("s{i}"), &key, Some(&main))
                .ago((i + 1) * MINUTE)
                .pending()
                .build()
        })
        .collect();
    sessions.push(
        sess("a", "f", &key, Some(&fx.wt("feat")))
            .pending()
            .ago(MINUTE)
            .build(),
    );
    sessions.push(
        sess("a", "settled", &key, Some(&fx.wt("feat")))
            .ago(2 * MINUTE)
            .build(),
    );
    let built = compose(&fx.snapshot(), &[("a", sessions)], None, None);
    let wanted: Vec<&str> = built.pending.iter().map(|k| &*k.id).collect();
    assert_eq!(
        wanted,
        ["s0", "s1", "s2", "s3", "s4", "f"],
        "the five listed in a row, each once; not the sixth, not a settled one"
    );
    let main_row = row(&built, &main);
    assert_eq!(main_row.sessions, 7);
    assert_eq!(main_row.recent.len(), 5, "at most five are listed");
    assert_eq!(ids(main_row), ["s0", "s1", "s2", "s3", "s4"]);
}

#[test]
fn build_folder_project() {
    let fx = ProjectFx::folder("notes");
    let key = fx.key();
    let sessions = vec![sess("a", "s", &key, None).build()];
    let built = compose(&fx.snapshot(), &[("a", sessions)], None, None);
    assert!(built.snapshot.folder);
    assert!(built.snapshot.active_rows.is_empty() && built.snapshot.idle_rows.is_empty());
    assert!(built.snapshot.files_settled);
    assert!(built.queries.is_empty());
    assert_eq!(built.snapshot.active, None);
    insta::assert_snapshot!("snapshot_folder", render(&built));
}

#[test]
fn build_files_settled_and_generations() {
    let fx = ProjectFx::git("app").files_pending().generation(9);
    let lists = [(SourceId(Arc::from("a")), list(4, Vec::new()))];
    let built = build(&Inputs {
        project: &fx.snapshot(),
        sessions: &lists,
        changes: None,
        usage: None,
        switches: 3,
        usage_generation: 8,
        now: Timestamp(NOW + 5),
    });
    let snapshot = &built.snapshot;
    assert!(!snapshot.files_settled, "the file list is still pending");
    assert_eq!(snapshot.switches, 3);
    assert_eq!(snapshot.sources.project, 9);
    assert_eq!(snapshot.sources.usage, 8);
    assert_eq!(
        snapshot.sources.sessions.as_ref(),
        [(SourceId(Arc::from("a")), 4)]
    );
    assert_eq!(snapshot.built_at, Timestamp(NOW + 5));
}

#[test]
fn build_1000_fixture_shape() {
    let fixture = fixture_1000();
    let built = build(&Inputs {
        project: &fixture.project,
        sessions: &fixture.lists,
        changes: Some(&fixture.changes),
        usage: Some(&fixture.usage),
        switches: 0,
        usage_generation: 0,
        now: Timestamp(NOW),
    });
    let rows: Vec<&WorktreeRow> = built
        .snapshot
        .active_rows
        .iter()
        .chain(built.snapshot.idle_rows.iter())
        .collect();
    assert_eq!(rows.len(), 17);
    assert_eq!(rows.iter().map(|r| r.sessions).sum::<u32>(), 1000);
    assert!(rows.iter().all(|r| r.recent.len() == 5));
    assert!(
        rows.iter()
            .all(|r| matches!(&r.overlaps, Overlaps::List(l) if l.len() == 8))
    );
    assert!(rows.iter().all(|r| matches!(r.usage, RowUsage::Branch(_))));
    assert_eq!(built.queries.len(), 17);
    assert!(!built.snapshot.active_rows.is_empty(), "some sessions run");
}
