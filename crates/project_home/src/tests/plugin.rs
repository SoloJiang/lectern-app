//! The plugins in a real kernel (docs/design/project_home.md「测试」「插件」):
//! the services are fakes, events are emitted by the test, and the composing
//! task runs when the test runs the manual executor.

use lectern_services::project::ProjectKey;
use lectern_services::usage::{ProjectRef, UsageEvent, UsageQuery, UsageState, Window};

use crate::testing::{
    MINUTE, ProjectFx, Rig, entry, overview, progress, project_usage, sess, sessions_event,
    usage_limits,
};
use lectern_services::review::OutsideCount;
use lectern_services::session::Agent;
use lectern_services::workspace::{ContributionKind, SidebarSection};

/// A project with two sessions in its main worktree, set in the fake `Projects`
/// but not yet announced.
fn project(rig: &Rig, name: &str) -> (ProjectFx, ProjectKey) {
    let fx = ProjectFx::git(name);
    let key = fx.key();
    rig.projects.set(fx.snapshot());
    rig.sessions.set(vec![
        sess("fake", "s1", &key, Some(&fx.wt("main")))
            .ago(MINUTE)
            .build(),
        sess("fake", "s2", &key, Some(&fx.wt("main")))
            .ago(2 * MINUTE)
            .build(),
    ]);
    (fx, key)
}

#[test]
fn activate_registers_contributions_only() {
    let rig = Rig::new();
    let (_, key) = project(&rig, "app");
    // Nothing is open yet: the project is only in the fake's table.
    rig.projects.remove(&key);
    rig.activate_home();
    assert_eq!(rig.run(), 0, "no task is submitted by activation");
    assert_eq!(rig.projects.snapshot_calls(), 0);
    assert!(rig.review.watched().is_empty());
    assert!(rig.usage.watched().is_empty());
    assert_eq!(
        rig.contributions.kinds(),
        [
            (
                ContributionKind::Screen(crate::contribute::LAYOUT),
                "project-home".to_owned()
            ),
            (ContributionKind::SidebarPanel, "project".to_owned()),
            (
                ContributionKind::SidebarEntry {
                    section: SidebarSection::Project
                },
                "overview".to_owned()
            ),
            (ContributionKind::KeyBindings, "project-home".to_owned()),
        ],
        "the screen, the sidebar panel, the Overview entry and the key bindings"
    );
    assert_eq!(rig.contributions.registered(), 4, "and nothing else");
    assert!(rig.contributions.commands().is_empty());
    assert!(rig.snapshot().is_none());
}

#[test]
fn opened_takes_the_overview_interest_and_closed_drops_it() {
    let rig = Rig::new();
    rig.activate_home();
    let (_, key) = project(&rig, "app");

    rig.opened(&key);
    assert_eq!(rig.review.watched(), std::slice::from_ref(&key));
    assert_eq!(rig.review.released(), 0);
    assert!(
        rig.snapshot().is_none(),
        "composed in a task, not in the listener"
    );
    assert!(rig.run() > 0);
    let snapshot = rig.snapshot().expect("a snapshot");
    assert_eq!(snapshot.project, key);
    assert_eq!(snapshot.sidebar.session_rows, 2);
    assert!(
        !snapshot.main.overview_ready,
        "the fake has no overview yet"
    );

    rig.projects.remove(&key);
    rig.closed(&key);
    assert_eq!(
        rig.review.released(),
        1,
        "the interest is dropped on Closed"
    );
    assert!(rig.snapshot().is_none());
    rig.changed(&key);
    assert_eq!(
        rig.run(),
        0,
        "a `Changed` after `Closed` does not open it again"
    );
    assert_eq!(rig.review.watched().len(), 1);
}

#[test]
fn opening_another_project_drops_the_old_interest_first() {
    let rig = Rig::new();
    rig.activate_home();
    let (_, first) = project(&rig, "one");
    let (_, second) = project(&rig, "two");
    rig.opened(&first);
    rig.run();
    rig.opened(&first);
    assert_eq!(
        rig.review.watched().len(),
        1,
        "the same project keeps its interest"
    );
    rig.opened(&second);
    assert_eq!(rig.review.watched(), [first.clone(), second.clone()]);
    assert_eq!(rig.review.released(), 1);
    rig.run();
    assert_eq!(rig.snapshot().unwrap().project, second);
    // A Closed for a project that is not the base changes nothing.
    rig.closed(&first);
    assert_eq!(rig.review.released(), 1);
    assert!(rig.snapshot().is_some());
}

#[test]
fn events_coalesce_into_one_build() {
    let rig = Rig::new();
    rig.activate_home();
    let (_, key) = project(&rig, "app");
    rig.opened(&key);
    for _ in 0..4 {
        rig.sessions_changed();
        rig.changed(&key);
        rig.overview_changed(&key);
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
        emitter.emit(lectern_services::project::ProjectEvent::Changed {
            project: k.clone(),
            generation: 3,
            parts: Default::default(),
        });
        emitter.emit(lectern_services::review::ReviewEvent::Overview {
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
}

#[test]
fn complete_is_requested_once_per_pending_session() {
    let rig = Rig::new();
    rig.activate_home();
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
    rig.review.set_overview(Some(overview(
        &key,
        vec![entry(&a.key, progress(0, 2, 0), None, 0)],
        OutsideCount::Files(0),
    )));
    rig.opened(&key);
    rig.run();
    assert_eq!(
        rig.sessions.completed(),
        [vec![a.key.clone(), c.key.clone()]],
        "a is a row and a card, and is asked for once"
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
fn usage_slot_empty_builds_without_usage() {
    let rig = Rig::new();
    rig.activate_home();
    let (_, key) = project(&rig, "app");
    rig.opened(&key);
    rig.run();
    let snapshot = rig.snapshot().unwrap();
    assert!(snapshot.usage.is_none(), "no usage plugin: no Usage row");
    assert!(rig.usage.watched().is_empty());
    // A usage event without a usage plugin does nothing.
    rig.kernel
        .emitter()
        .emit(UsageEvent::Limits { generation: 2 });
    assert_eq!(rig.run(), 0);
}

fn usage_query(key: &ProjectKey) -> UsageQuery {
    UsageQuery::Project {
        project: ProjectRef::Open(key.clone()),
        window: Window::D7,
    }
}

#[test]
fn usage_activating_after_opened_takes_the_watch() {
    let rig = Rig::new();
    rig.activate_home();
    let (_, key) = project(&rig, "app");
    rig.usage
        .set_snapshot(Some((*project_usage(UsageState::Ready, 5_000)).clone()));
    rig.usage
        .set_limits((*usage_limits(vec![(Agent::ClaudeCode, 61.0)], Some(0))).clone());
    rig.opened(&key);
    rig.run();
    assert!(rig.snapshot().unwrap().usage.is_none());

    rig.activate_usage();
    assert!(
        rig.usage.watched().is_empty(),
        "`started` only records: the token is still pending there"
    );
    let before = rig.projects.snapshot_calls();
    rig.run();
    assert_eq!(rig.usage.watched(), [usage_query(&key)]);
    assert_eq!(rig.projects.snapshot_calls(), before + 1, "recomposed once");
    let usage = rig
        .snapshot()
        .unwrap()
        .usage
        .clone()
        .expect("a usage model");
    assert_eq!(usage.tokens_7d, Some(5_000));
    assert_eq!(usage.tightest.unwrap().used_percent, 61);
}

#[test]
fn opened_after_usage_takes_the_watch() {
    let rig = Rig::new();
    rig.activate_home();
    rig.activate_usage();
    assert_eq!(rig.run(), 1, "the reconcile task of `started`");
    assert!(rig.usage.watched().is_empty(), "no project yet");
    let (_, key) = project(&rig, "app");
    rig.usage
        .set_snapshot(Some((*project_usage(UsageState::Ready, 77)).clone()));

    rig.opened(&key);
    assert_eq!(rig.usage.watched(), [usage_query(&key)], "taken by Opened");
    rig.run();
    assert_eq!(rig.usage.watched().len(), 1);
    assert_eq!(
        rig.snapshot().unwrap().usage.clone().unwrap().tokens_7d,
        Some(77)
    );

    // The interest follows the project.
    rig.closed(&key);
    assert_eq!(rig.usage.released(), 1);
}

#[test]
fn usage_plugin_deactivation_rebuilds() {
    let rig = Rig::new();
    rig.activate_home();
    rig.activate_usage();
    let (_, key) = project(&rig, "app");
    rig.usage
        .set_snapshot(Some((*project_usage(UsageState::Ready, 9)).clone()));
    rig.opened(&key);
    rig.run();
    assert!(rig.snapshot().unwrap().usage.is_some());
    assert_eq!(rig.usage.released(), 0);

    let before = rig.projects.snapshot_calls();
    rig.deactivate_usage();
    assert_eq!(
        rig.usage.released(),
        1,
        "the interest is dropped with the plugin"
    );
    rig.run();
    assert_eq!(rig.projects.snapshot_calls(), before + 1, "recomposed once");
    assert!(
        rig.snapshot().unwrap().usage.is_none(),
        "the Usage row is gone"
    );
    assert_eq!(
        rig.snapshot().unwrap().sidebar.session_rows,
        2,
        "the rest is unchanged"
    );
}

#[test]
fn usage_events_recompose() {
    let rig = Rig::new();
    rig.activate_home();
    rig.activate_usage();
    let (_, key) = project(&rig, "app");
    rig.opened(&key);
    rig.run();
    let before = rig.projects.snapshot_calls();
    rig.kernel
        .emitter()
        .emit(UsageEvent::Limits { generation: 5 });
    rig.run();
    assert_eq!(rig.projects.snapshot_calls(), before + 1);
}

#[test]
fn started_handles_an_already_open_project() {
    let rig = Rig::new();
    let (_, key) = project(&rig, "app");
    rig.activate_home();
    assert_eq!(
        rig.review.watched(),
        std::slice::from_ref(&key),
        "taken in `started`"
    );
    assert!(rig.run() > 0);
    assert_eq!(rig.snapshot().unwrap().project, key);
}

#[test]
fn changed_for_an_unknown_project_counts_as_opened() {
    let rig = Rig::new();
    rig.activate_home();
    let (_, key) = project(&rig, "app");
    // The `Opened` fell between `started` and the activation going live.
    rig.changed(&key);
    assert_eq!(rig.review.watched(), std::slice::from_ref(&key));
    rig.run();
    assert!(rig.snapshot().is_some());
    // Another project's `Changed` while one is the base is not a new base.
    let (_, other) = project(&rig, "other");
    rig.changed(&other);
    assert_eq!(rig.review.watched().len(), 1);
}

#[test]
fn a_project_without_a_snapshot_is_skipped_and_recovers() {
    let rig = Rig::new();
    rig.activate_home();
    let fx = ProjectFx::git("app");
    let key = fx.key();
    rig.opened(&key);
    rig.run();
    assert!(rig.snapshot().is_none(), "`Projects::snapshot` had nothing");
    rig.projects.set(fx.snapshot());
    rig.changed(&key);
    rig.run();
    assert!(rig.snapshot().is_some());
}

#[test]
fn a_result_for_a_closed_project_is_dropped() {
    let rig = Rig::new();
    rig.activate_home();
    let (_, key) = project(&rig, "app");
    rig.opened(&key);
    let kernel = rig.kernel.clone();
    let k = key.clone();
    rig.projects.on_next_snapshot(move || {
        kernel
            .emitter()
            .emit(lectern_services::project::ProjectEvent::Closed { project: k });
    });
    rig.run();
    assert!(rig.snapshot().is_none(), "closed while composing");
    assert_eq!(rig.review.released(), 1);
}

#[test]
fn deactivation_drops_the_interests() {
    let rig = Rig::new();
    rig.activate_home();
    rig.activate_usage();
    let (_, key) = project(&rig, "app");
    rig.opened(&key);
    rig.run();
    rig.kernel.set_enabled(crate::PLUGIN_ID, false);
    rig.kernel.barrier();
    assert_eq!(rig.review.released(), 1);
    assert_eq!(rig.usage.released(), 1);
}
