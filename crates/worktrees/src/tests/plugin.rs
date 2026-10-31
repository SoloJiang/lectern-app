//! The plugins in a real kernel (docs/design/worktrees.md「测试」「插件」): the
//! services are fakes, events are emitted by the test, and the composing task
//! runs when the test runs the manual executor.

use std::sync::Arc;

use lectern_services::project::{ChangedParts, ProjectEvent, ProjectKey};
use lectern_services::usage::{UsageEvent, UsageState};
use lectern_services::workspace::{
    ContributionKind, Route, SidebarSection, Target, WorkspaceCommand, screens,
};
use lectern_ui::Icon;
use lectern_ui::contrib::InspectorTab;

use crate::snapshot::RowUsage;
use crate::testing::{
    MINUTE, ProjectFx, Rig, branch, branch_usage, mr_label, mr_ref, mr_snapshot, sess,
    sessions_event, tokens, worktree_query, worktree_snapshot,
};
use lectern_services::usage::UsageQuery;

/// A project with a main and a feature worktree and one session in each, set
/// in the fake `Projects` but not yet announced.
fn project(rig: &Rig, name: &str) -> (ProjectFx, ProjectKey) {
    let fx = ProjectFx::git(name).linked("feat", branch("feat/x"));
    let key = fx.key();
    rig.projects.set(fx.snapshot());
    rig.sessions.set(vec![
        sess("fake", "s1", &key, Some(&fx.wt("main")))
            .ago(MINUTE)
            .build(),
        sess("fake", "s2", &key, Some(&fx.wt("feat")))
            .ago(2 * MINUTE)
            .build(),
    ]);
    (fx, key)
}

fn active_parts() -> ChangedParts {
    ChangedParts {
        active: true,
        ..ChangedParts::default()
    }
}

#[test]
fn activate_registers_contributions_only() {
    let rig = Rig::new();
    let (_, key) = project(&rig, "app");
    rig.projects.remove(&key);
    rig.activate_main();
    rig.activate_usage();
    // Only the usage plugin's reconcile task of `started` runs.
    assert_eq!(rig.run(), 1);
    assert_eq!(rig.projects.snapshot_calls(), 0);
    assert!(rig.review.watched().is_empty());
    assert!(rig.usage.watched().is_empty());
    assert!(rig.snapshot().is_none());

    // The screen, the inspector page and the key bindings; no sidebar entry
    // (it exists only while a git project is open), and no token yet: the
    // guards go to the activation's commit.
    assert_eq!(
        rig.contributions.kinds(),
        [
            (
                ContributionKind::Screen(crate::contribute::LAYOUT),
                "worktrees".to_owned()
            ),
            (ContributionKind::InspectorTab, "worktree".to_owned()),
            (ContributionKind::KeyBindings, "worktrees".to_owned()),
        ]
    );
    assert_eq!(rig.contributions.tokens_given(), [false, false, false]);
    assert!(rig.contributions.entries().is_empty());
    let tab: InspectorTab = rig.contributions.payload("worktree").expect("the page");
    assert_eq!(
        tab.screen,
        Some(std::borrow::Cow::Borrowed(screens::WORKTREES)),
        "the page is only for the Worktrees screen"
    );
    let layout = crate::contribute::LAYOUT;
    assert!(layout.back_button && layout.reading_column && layout.inspector);
    assert!(!layout.floating_bar);
}

#[test]
fn git_project_registers_the_sidebar_entry() {
    let rig = Rig::new();
    rig.activate_main();
    let (_, key) = project(&rig, "app");
    assert!(rig.contributions.entries().is_empty(), "no project yet");

    rig.opened(&key);
    assert_eq!(
        rig.contributions.entries(),
        [(
            ContributionKind::SidebarEntry {
                section: SidebarSection::Views
            },
            "worktrees".to_owned()
        )]
    );
    assert_eq!(rig.contributions.order("worktrees"), Some(0));
    assert_eq!(
        rig.contributions.tokens_given(),
        [false, false, false, true],
        "the entry is registered after `started`, with the activation's token"
    );
    let item = rig.contributions.sidebar_item("worktrees").unwrap();
    assert_eq!(
        item.label.name(),
        lectern_i18n::keys::worktrees::ENTRY.name()
    );
    assert_eq!(item.icon, Some(Icon::Worktrees));
    assert_eq!(
        item.on_select,
        WorkspaceCommand::Push(Route {
            screen: screens::WORKTREES.into(),
            target: Target::None,
        })
    );
    // Nothing else happens while the screen is not the current one.
    assert_eq!(rig.run(), 0);
    assert_eq!(rig.projects.snapshot_calls(), 0);
    assert!(rig.review.watched().is_empty());
    assert!(rig.snapshot().is_none());
}

#[test]
fn folder_project_registers_no_entry() {
    let rig = Rig::new();
    rig.activate_main();
    let fx = ProjectFx::folder("notes");
    let key = fx.key();
    rig.projects.set(fx.snapshot());
    rig.opened(&key);
    assert!(rig.contributions.entries().is_empty());

    // Pushed by the probe, the screen shows the folder's empty state.
    rig.show();
    assert!(
        rig.review.watched().is_empty(),
        "review compares nothing for a folder"
    );
    rig.run();
    let snapshot = rig.snapshot().expect("a snapshot");
    assert!(snapshot.folder);
    assert!(snapshot.active_rows.is_empty() && snapshot.idle_rows.is_empty());
}

#[test]
fn closed_drops_the_entry() {
    let rig = Rig::new();
    rig.activate_main();
    let (_, key) = project(&rig, "app");
    rig.opened(&key);
    assert_eq!(rig.contributions.released(), 0);
    rig.projects.remove(&key);
    rig.closed(&key);
    assert_eq!(rig.contributions.released(), 1);
    // A `Changed` after `Closed` does not open it again.
    rig.changed(&key);
    assert_eq!(rig.contributions.entries().len(), 1);
    assert_eq!(rig.run(), 0);
}

#[test]
fn opening_another_project_replaces_the_entry() {
    let rig = Rig::new();
    rig.activate_main();
    let (_, first) = project(&rig, "one");
    let (_, second) = project(&rig, "two");
    rig.opened(&first);
    rig.opened(&first);
    assert_eq!(
        rig.contributions.entries().len(),
        1,
        "the same project keeps its entry"
    );
    rig.opened(&second);
    assert_eq!(rig.contributions.entries().len(), 2);
    assert_eq!(rig.contributions.released(), 1, "the old one went first");
    // A project that is not the base closing changes nothing.
    rig.closed(&first);
    assert_eq!(rig.contributions.released(), 1);
    // A folder replaces a git project with no entry.
    let folder = ProjectFx::folder("notes");
    rig.projects.set(folder.snapshot());
    rig.opened(&folder.key());
    assert_eq!(rig.contributions.released(), 2);
    assert_eq!(rig.contributions.entries().len(), 2);
}

#[test]
fn interests_follow_the_screen() {
    let rig = Rig::new();
    rig.activate_main();
    rig.activate_usage();
    rig.run();
    let fx = ProjectFx::git("app")
        .linked("feat", branch("feat/x"))
        .prunable("gone", branch("gone"));
    let key = fx.key();
    rig.projects.set(fx.snapshot());
    rig.opened(&key);
    assert!(rig.review.watched().is_empty(), "not shown yet");

    rig.show();
    assert_eq!(rig.review.watched(), std::slice::from_ref(&key));
    assert!(rig.snapshot().is_none(), "composed in a task");
    assert!(rig.run() > 0);
    assert!(rig.snapshot().is_some());
    assert_eq!(
        rig.usage.watched(),
        [
            worktree_query(&key, &fx.wt("main")),
            worktree_query(&key, &fx.wt("feat")),
        ],
        "one query per worktree that is not prunable"
    );
    assert_eq!((rig.review.released(), rig.usage.released()), (0, 0));

    // The review screen is pushed on top: everything is let go.
    rig.screen(screens::REVIEW);
    assert_eq!(rig.review.released(), 1);
    assert_eq!(rig.usage.released(), 2);
    assert!(rig.snapshot().is_none(), "the hub drops its snapshot");
    assert_eq!(rig.run(), 0, "and composes nothing");

    // Back again: the interests are taken once more.
    rig.show();
    assert_eq!(rig.review.watched().len(), 2);
    rig.run();
    assert_eq!(rig.usage.watched().len(), 4);
    assert!(rig.snapshot().is_some());

    // The same screen announced twice changes nothing.
    rig.show();
    assert_eq!(rig.review.watched().len(), 2);
    // The project closing lets go of what is left.
    rig.closed(&key);
    assert_eq!(rig.review.released(), 2);
    assert_eq!(rig.usage.released(), 4);
}

#[test]
fn hidden_screen_ignores_events() {
    let rig = Rig::new();
    rig.activate_main();
    let (_, key) = project(&rig, "app");
    rig.opened(&key);
    rig.sessions_changed();
    rig.changed(&key);
    rig.worktrees_changed(&key);
    rig.changed_parts(&key, active_parts());
    rig.changed_parts(&key, active_parts());
    assert_eq!(rig.run(), 0, "nothing is composed while hidden");
    assert_eq!(rig.projects.snapshot_calls(), 0);
    assert!(rig.review.watched().is_empty());
    assert_eq!(
        rig.hub().switches(),
        2,
        "but the switches of the current worktree are counted"
    );

    // The first composition after the screen is shown sees them.
    rig.show();
    rig.run();
    assert_eq!(rig.snapshot().unwrap().switches, 2);

    // And hidden again.
    rig.hide();
    rig.sessions_changed();
    rig.worktrees_changed(&key);
    assert_eq!(rig.run(), 0);
}

#[test]
fn events_coalesce_into_one_build() {
    let rig = Rig::new();
    rig.activate_main();
    let (_, key) = project(&rig, "app");
    rig.opened(&key);
    rig.show();
    for _ in 0..4 {
        rig.sessions_changed();
        rig.changed(&key);
        rig.worktrees_changed(&key);
    }
    rig.run();
    assert_eq!(
        rig.projects.snapshot_calls(),
        1,
        "events before the task runs share the one task"
    );

    // Five events arrive while a composition is running: one more round.
    let kernel = rig.kernel.clone();
    let k = key.clone();
    rig.projects.on_next_snapshot(move || {
        let emitter = kernel.emitter();
        for _ in 0..3 {
            emitter.emit(sessions_event());
        }
        emitter.emit(ProjectEvent::Changed {
            project: k.clone(),
            generation: 3,
            parts: ChangedParts::default(),
        });
        emitter.emit(lectern_services::review::ReviewEvent::Worktrees {
            project: k,
            generation: 9,
        });
    });
    rig.sessions_changed();
    rig.run();
    assert_eq!(
        rig.projects.snapshot_calls(),
        3,
        "the trigger's round and one more for the five events"
    );
    rig.run();
    assert_eq!(rig.projects.snapshot_calls(), 3, "nothing is left to do");

    // Events of another project do not compose.
    let (_, other) = project(&rig, "other");
    rig.worktrees_changed(&other);
    assert_eq!(rig.run(), 0);
}

#[test]
fn switches_count_active_changes() {
    let rig = Rig::new();
    rig.activate_main();
    let (_, key) = project(&rig, "app");
    rig.opened(&key);
    assert_eq!(rig.hub().switches(), 0);
    rig.changed(&key);
    assert_eq!(rig.hub().switches(), 0, "a change of something else");
    rig.changed_parts(&key, active_parts());
    assert_eq!(rig.hub().switches(), 1);
    let (_, other) = project(&rig, "other");
    rig.changed_parts(&other, active_parts());
    assert_eq!(rig.hub().switches(), 1, "another project's worktree");

    rig.show();
    rig.run();
    assert_eq!(rig.snapshot().unwrap().switches, 1);
    rig.changed_parts(&key, active_parts());
    rig.run();
    assert_eq!(rig.hub().switches(), 2);
    assert_eq!(
        rig.snapshot().unwrap().switches,
        2,
        "the change composes a new snapshot"
    );
    // A closed project's changes are not counted.
    rig.projects.remove(&key);
    rig.closed(&key);
    rig.changed_parts(&key, active_parts());
    assert_eq!(rig.hub().switches(), 2);
}

#[test]
fn usage_link_empty_builds_without_usage() {
    let rig = Rig::new();
    rig.activate_main();
    let (_, key) = project(&rig, "app");
    rig.opened(&key);
    rig.show();
    rig.run();
    let snapshot = rig.snapshot().unwrap();
    let rows = || snapshot.active_rows.iter().chain(snapshot.idle_rows.iter());
    assert_eq!(rows().count(), 2);
    assert!(
        rows().all(|row| row.usage == RowUsage::Absent),
        "no usage plugin: no tokens"
    );
    assert!(rig.usage.watched().is_empty());
    // A usage event without a usage plugin does nothing.
    rig.kernel
        .emitter()
        .emit(UsageEvent::Limits { generation: 2 });
    assert_eq!(rig.run(), 0);
}

#[test]
fn usage_activating_after_shown_takes_the_watches() {
    let rig = Rig::new();
    rig.activate_main();
    let (fx, key) = project(&rig, "app");
    rig.usage.set_snapshot(
        worktree_query(&key, &fx.wt("feat")),
        worktree_snapshot(branch_usage(
            UsageState::Ready,
            Some(tokens(500, 100)),
            vec![],
        )),
    );
    rig.opened(&key);
    rig.show();
    rig.run();
    assert!(
        rig.snapshot()
            .unwrap()
            .idle_rows
            .iter()
            .all(|row| row.usage == RowUsage::Absent)
    );

    rig.activate_usage();
    assert!(
        rig.usage.watched().is_empty(),
        "`started` only records: the token is still pending there"
    );
    let before = rig.projects.snapshot_calls();
    rig.run();
    assert_eq!(
        rig.usage.watched(),
        [
            worktree_query(&key, &fx.wt("main")),
            worktree_query(&key, &fx.wt("feat")),
        ]
    );
    assert!(
        rig.projects.snapshot_calls() > before,
        "composed again with the usage plugin there"
    );
    let snapshot = rig.snapshot().unwrap();
    let feat = snapshot
        .idle_rows
        .iter()
        .find(|row| row.key == fx.wt("feat"))
        .unwrap();
    let RowUsage::Branch(branch) = &feat.usage else {
        panic!("branch usage: {:?}", feat.usage);
    };
    assert_eq!(branch.cumulative, tokens(500, 100));
    let main = snapshot
        .idle_rows
        .iter()
        .find(|row| row.key == fx.wt("main"))
        .unwrap();
    assert_eq!(main.usage, RowUsage::Pending, "no snapshot of main yet");
    // Nothing more happens.
    assert_eq!(rig.run(), 0);
}

#[test]
fn usage_plugin_deactivation_rebuilds() {
    let rig = Rig::new();
    rig.activate_main();
    rig.activate_usage();
    let (_, key) = project(&rig, "app");
    rig.opened(&key);
    rig.show();
    rig.run();
    assert!(
        rig.snapshot()
            .unwrap()
            .idle_rows
            .iter()
            .all(|row| row.usage != RowUsage::Absent)
    );
    assert_eq!(rig.usage.released(), 0);

    let before = rig.projects.snapshot_calls();
    rig.deactivate_usage();
    assert_eq!(
        rig.usage.released(),
        2,
        "the interests are dropped with the plugin"
    );
    rig.run();
    assert_eq!(rig.projects.snapshot_calls(), before + 1, "composed once");
    assert!(
        rig.snapshot()
            .unwrap()
            .idle_rows
            .iter()
            .all(|row| row.usage == RowUsage::Absent),
        "the tokens are gone"
    );
    assert_eq!(rig.snapshot().unwrap().idle_rows.len(), 2, "the rest stays");
}

#[test]
fn same_wanted_set_does_not_rebuild() {
    let rig = Rig::new();
    rig.activate_main();
    rig.activate_usage();
    let (_, key) = project(&rig, "app");
    rig.opened(&key);
    rig.show();
    rig.run();
    let watched = rig.usage.watched().len();
    let calls = rig.projects.snapshot_calls();
    let generation = rig.link.generation();

    // One more event: one more composition, which hands back the same
    // queries and takes no new interest; that must not compose again.
    rig.sessions_changed();
    rig.run();
    assert_eq!(rig.projects.snapshot_calls(), calls + 1);
    assert_eq!(rig.usage.watched().len(), watched);
    assert_eq!(
        rig.link.generation(),
        generation,
        "and the link did not change"
    );
    rig.run();
    assert_eq!(rig.projects.snapshot_calls(), calls + 1);
}

#[test]
fn usage_events_of_wanted_queries_recompose() {
    let rig = Rig::new();
    rig.activate_main();
    rig.activate_usage();
    let (fx, key) = project(&rig, "app");
    rig.opened(&key);
    rig.show();
    rig.run();
    let calls = rig.projects.snapshot_calls();

    rig.usage_changed(UsageQuery::Mr { mr: mr_ref(1) });
    assert_eq!(rig.run(), 0, "a query nobody wants");
    rig.usage_changed(worktree_query(&key, &fx.wt("feat")));
    assert!(rig.run() > 0);
    assert_eq!(rig.projects.snapshot_calls(), calls + 1);
    assert_eq!(rig.snapshot().unwrap().sources.usage, rig.link.generation());
}

#[test]
fn usage_of_a_branch_in_several_mrs_asks_for_each_mr() {
    let rig = Rig::new();
    rig.activate_main();
    rig.activate_usage();
    let (fx, key) = project(&rig, "app");
    rig.usage.set_snapshot(
        worktree_query(&key, &fx.wt("feat")),
        worktree_snapshot(branch_usage(
            UsageState::Ready,
            Some(tokens(900, 100)),
            vec![mr_label(21, "First"), mr_label(22, "Second")],
        )),
    );
    rig.usage.set_snapshot(
        UsageQuery::Mr { mr: mr_ref(21) },
        mr_snapshot(21, UsageState::Ready, tokens(600, 60), vec![22]),
    );
    rig.opened(&key);
    rig.show();
    rig.run();
    let watched = rig.usage.watched();
    assert!(watched.contains(&UsageQuery::Mr { mr: mr_ref(21) }));
    assert!(watched.contains(&UsageQuery::Mr { mr: mr_ref(22) }));
    assert_eq!(watched.len(), 4, "two worktrees and two MRs, each once");
    let snapshot = rig.snapshot().unwrap();
    let feat = snapshot
        .idle_rows
        .iter()
        .find(|row| row.key == fx.wt("feat"))
        .unwrap();
    let RowUsage::Mrs(items) = &feat.usage else {
        panic!("MR usage");
    };
    assert_eq!(items[0].tokens, Some(tokens(600, 60)));
    assert_eq!(items[0].shared_with, 1);
    assert_eq!(items[1].tokens, None, "the second MR has no snapshot yet");
}

#[test]
fn started_handles_an_already_open_project() {
    let rig = Rig::new();
    let (_, key) = project(&rig, "app");
    rig.activate_main();
    assert_eq!(
        rig.contributions.entries().len(),
        1,
        "the entry is registered in `started`"
    );
    assert_eq!(
        rig.run(),
        0,
        "and nothing is composed until the screen shows"
    );
    rig.show();
    assert_eq!(rig.review.watched(), std::slice::from_ref(&key));
    assert!(rig.run() > 0);
    assert_eq!(rig.snapshot().unwrap().project, key);
}

#[test]
fn changed_for_an_unknown_project_counts_as_opened() {
    let rig = Rig::new();
    rig.activate_main();
    let (_, key) = project(&rig, "app");
    // The `Opened` fell between `started` and the activation going live.
    rig.changed_parts(&key, active_parts());
    assert_eq!(rig.contributions.entries().len(), 1, "the entry is there");
    assert_eq!(rig.hub().switches(), 0, "an opening is not a switch");
    rig.show();
    assert_eq!(rig.review.watched(), std::slice::from_ref(&key));
    rig.run();
    assert!(rig.snapshot().is_some());
    // Another project's `Changed` while one is the base is not a new base.
    let (_, other) = project(&rig, "other");
    rig.changed(&other);
    assert_eq!(rig.contributions.entries().len(), 1);
    assert_eq!(rig.review.watched().len(), 1);
}

#[test]
fn complete_is_requested_once_per_pending_session() {
    let rig = Rig::new();
    rig.activate_main();
    let fx = ProjectFx::git("app");
    let key = fx.key();
    let main = fx.wt("main");
    rig.projects.set(fx.snapshot());
    let a = sess("fake", "a", &key, Some(&main))
        .pending()
        .ago(MINUTE)
        .build();
    let b = sess("fake", "b", &key, Some(&main)).ago(2 * MINUTE).build();
    let c = sess("fake", "c", &key, Some(&main))
        .pending()
        .ago(3 * MINUTE)
        .build();
    rig.sessions.set(vec![a.clone(), b.clone(), c.clone()]);
    rig.opened(&key);
    rig.show();
    rig.run();
    assert_eq!(
        rig.sessions.completed(),
        [vec![a.key.clone(), c.key.clone()]]
    );

    rig.sessions_changed();
    rig.run();
    assert_eq!(rig.sessions.completed().len(), 1, "not asked for again");

    // The tails arrive: the sessions are no longer pending, a new one is.
    let d = sess("fake", "d", &key, Some(&main))
        .pending()
        .ago(4 * MINUTE)
        .build();
    rig.sessions.set(vec![
        sess("fake", "a", &key, Some(&main)).ago(MINUTE).build(),
        b,
        c.clone(),
        d.clone(),
    ]);
    rig.sessions_changed();
    rig.run();
    assert_eq!(
        rig.sessions.completed(),
        [vec![a.key, c.key.clone()], vec![d.key.clone()]]
    );

    // A project that is opened again forgets what was asked.
    rig.closed(&key);
    rig.opened(&key);
    rig.run();
    assert_eq!(rig.sessions.completed().last(), Some(&vec![c.key, d.key]));
}

#[test]
fn a_result_for_a_hidden_screen_is_dropped() {
    let rig = Rig::new();
    rig.activate_main();
    rig.activate_usage();
    let (_, key) = project(&rig, "app");
    rig.opened(&key);
    rig.show();
    let kernel = rig.kernel.clone();
    rig.projects.on_next_snapshot(move || {
        kernel
            .emitter()
            .emit(lectern_services::workspace::WorkspaceEvent::ScreenChanged {
                window: lectern_services::workspace::WindowId(0),
                from: None,
                to: Route {
                    screen: screens::REVIEW.into(),
                    target: Target::None,
                },
            });
    });
    rig.run();
    assert!(rig.snapshot().is_none(), "left while composing");
    assert_eq!(rig.review.released(), 1);
    assert_eq!(
        rig.usage.watched().len(),
        0,
        "and the usage interests were not taken for it"
    );
    assert_eq!(rig.usage.released(), 0);
}

#[test]
fn a_result_for_a_closed_project_is_dropped() {
    let rig = Rig::new();
    rig.activate_main();
    let (_, key) = project(&rig, "app");
    rig.opened(&key);
    rig.show();
    let kernel = rig.kernel.clone();
    let k = key.clone();
    rig.projects.on_next_snapshot(move || {
        kernel.emitter().emit(ProjectEvent::Closed { project: k });
    });
    rig.run();
    assert!(rig.snapshot().is_none(), "closed while composing");
    assert_eq!(rig.review.released(), 1);
}

#[test]
fn a_project_without_a_snapshot_is_skipped_and_recovers() {
    let rig = Rig::new();
    rig.activate_main();
    let fx = ProjectFx::git("app");
    let key = fx.key();
    rig.opened(&key);
    rig.show();
    rig.run();
    assert!(rig.snapshot().is_none(), "`Projects::snapshot` had nothing");
    rig.projects.set(fx.snapshot());
    rig.changed(&key);
    rig.run();
    assert!(rig.snapshot().is_some());
}

#[test]
fn views_get_each_snapshot_and_an_empty_one_when_the_project_closes() {
    let rig = Rig::new();
    rig.activate_main();
    let (_, key) = project(&rig, "app");
    let mut view = rig.hub().attach_view("screen");
    rig.opened(&key);
    rig.show();
    assert!(view.try_recv().is_err(), "nothing yet");
    rig.run();
    let first = view.try_recv().expect("a snapshot");
    assert_eq!(first.project, key);
    assert_eq!(first.idle_rows.len(), 2);
    assert!(Arc::ptr_eq(&first, &rig.snapshot().unwrap()));

    rig.closed(&key);
    let empty = view.try_recv().expect("an empty snapshot");
    assert!(empty.active_rows.is_empty() && empty.idle_rows.is_empty());
    assert_eq!(empty.project, key);
    assert_eq!(rig.hub().view_count(), 1);

    // A view that was dropped is forgotten.
    drop(view);
    let (_, other) = project(&rig, "other");
    rig.opened(&other);
    rig.run();
    assert_eq!(rig.hub().view_count(), 0);
}

#[test]
fn deactivation_drops_everything() {
    let rig = Rig::new();
    rig.activate_main();
    rig.activate_usage();
    let (_, key) = project(&rig, "app");
    rig.opened(&key);
    rig.show();
    rig.run();
    rig.deactivate_main();
    assert_eq!(
        rig.contributions.released(),
        4,
        "the entry and the three contributions of `activate`"
    );
    assert_eq!(rig.review.released(), 1);
    assert_eq!(rig.usage.released(), 2);
}
