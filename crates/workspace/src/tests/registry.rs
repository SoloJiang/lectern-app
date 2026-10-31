//! The contribution registry (docs/design/workspace.md「贡献注册表」) through
//! real activations, and the driver over it: what applies when, what is
//! rejected, and how the leave guards are asked.

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use lectern_kernel::{Activation, Plugin, service};
use lectern_services::project::{ChangedParts, ProjectEvent, Projects};
use lectern_services::workspace::{
    Contribution, ContributionKind, Contributions, LeaveScope, Payload, Route, SidebarSection,
    Target, WorkspaceCommand, WorkspaceEvent, screens,
};

use super::support::*;
use crate::WorkspacePlugin;
use crate::effect::Effect;
use crate::nav::Base;
use crate::registry::{Kind, Serial};

fn review_route() -> Route {
    Route {
        screen: name(screens::REVIEW),
        target: Target::None,
    }
}

/// The contributor every driver test starts from: the project home and the
/// review screen.
fn screens_plugin() -> Contributor {
    Contributor::staged(vec![
        Box::new(|c, t| c.register(screen(screens::PROJECT_HOME, home_layout(), "Home"), t)),
        Box::new(|c, t| c.register(screen(screens::REVIEW, review_layout(), "Review"), t)),
    ])
}

#[test]
fn plugin_manifest_matches_the_design() {
    let rig = Rig::prepared();
    let manifest = WorkspacePlugin::from_hub(rig.hub.clone()).manifest();
    assert_eq!(manifest.provides, vec![service::<dyn Contributions>()]);
    assert_eq!(manifest.requires, vec![service::<dyn Projects>()]);
    assert_eq!(manifest.activation, vec![Activation::AfterFirstFrame]);
}

#[test]
fn duplicate_ids_are_rejected() {
    let rig = Rig::prepared();
    let first = rig
        .hub
        .register(screen(screens::REVIEW, review_layout(), "a"), None);
    let duplicate = rig
        .hub
        .register(screen(screens::REVIEW, review_layout(), "b"), None);
    assert_eq!(rig.queue.len(), 0, "nothing is queued before a commit");
    // The rejected guard does nothing: the slot is still held by `first`.
    drop(duplicate);
    let again = rig
        .hub
        .register(screen(screens::REVIEW, review_layout(), "c"), None);
    assert_eq!(rig.queue.len(), 0);
    drop(again);
    // Releasing the holder frees the ID.
    drop(first);
    assert_eq!(rig.queue.len(), 0, "a staged entry is dropped in place");
    let _third = rig
        .hub
        .register(screen(screens::REVIEW, review_layout(), "d"), None);
    let contested = rig
        .hub
        .register(screen(screens::REVIEW, review_layout(), "e"), None);
    drop(contested);
    assert!(rig.hub.registry().applied().is_empty());
}

#[test]
fn wrong_payload_is_rejected() {
    let rig = Rig::prepared();
    let wrong = rig.hub.register(
        Contribution {
            kind: ContributionKind::Screen(review_layout()),
            id: name(screens::REVIEW),
            order: 0,
            payload: Payload(Box::new(Marker)),
        },
        None,
    );
    let wrong_item = rig.hub.register(
        Contribution {
            kind: ContributionKind::SidebarEntry {
                section: SidebarSection::Views,
            },
            id: name("files"),
            order: 0,
            payload: Payload(Box::new(ScreenPayload("x".to_owned()))),
        },
        None,
    );
    // Rejected registrations hold no slot.
    let _right = rig
        .hub
        .register(screen(screens::REVIEW, review_layout(), "a"), None);
    let _right_item = rig
        .hub
        .register(item("files", SidebarSection::Views, 0, "Files"), None);
    drop(wrong);
    drop(wrong_item);
    assert_eq!(rig.queue.len(), 0);
}

#[test]
fn invalid_ids_are_rejected() {
    let rig = Rig::prepared();
    for id in ["", "Bad Id", "with/slash", &"x".repeat(65)] {
        let _rejected = rig.hub.register(
            Contribution {
                kind: ContributionKind::Panel,
                id: name(id),
                order: 0,
                payload: Payload(Box::new(Marker)),
            },
            None,
        );
        let _rejected_command = rig.hub.handle_command(name(id), Box::new(|_| {}), None);
    }
    let _ok = rig
        .hub
        .register(plain(ContributionKind::Panel, &"x".repeat(64)), None);
    let _ok_command = rig
        .hub
        .handle_command(name("finder-open_2"), Box::new(|_| {}), None);
    assert_eq!(rig.queue.len(), 0);
}

#[test]
fn staged_registration_applies_on_commit_only() {
    let mut rig = Rig::new();
    let kernel = contributor(&rig.hub, screens_plugin());
    assert_eq!(rig.queue.len(), 0, "nothing before the activation");
    kernel.activate().unwrap();
    assert_eq!(rig.queue.len(), 2, "one apply per staged registration");
    assert!(
        !rig.hub.registry().has(Kind::Screen, screens::REVIEW),
        "queued, not applied"
    );
    let effects = rig.drain();
    assert!(rig.hub.registry().has(Kind::Screen, screens::REVIEW));
    assert_eq!(
        rig.hub.registry().screen_layout(screens::REVIEW),
        Some(review_layout())
    );
    assert_eq!(
        effects
            .iter()
            .filter(|e| **e == Effect::Event(WorkspaceEvent::ContributionsChanged))
            .count(),
        2
    );
}

#[test]
fn dead_token_is_not_applied() {
    let mut rig = Rig::new();
    let kernel = contributor(&rig.hub, screens_plugin());
    kernel.activate().unwrap();
    kernel.deactivate();
    assert_eq!(rig.queue.len(), 4, "two applies, two removes");
    let effects = rig.drain();
    assert!(rig.hub.registry().applied().is_empty());
    assert!(effects.is_empty(), "nothing applied, nothing removed");
}

#[test]
fn pending_token_is_deferred() {
    let mut rig = Rig::new();
    let (in_started, started) = mpsc::channel();
    let (go, wait) = mpsc::channel::<()>();
    let wait = Mutex::new(wait);
    let mut plugin = screens_plugin();
    plugin.on_started = Some(Arc::new(move |_| {
        in_started.send(()).unwrap();
        wait.lock().unwrap().recv().unwrap();
    }));
    let kernel = Arc::new(contributor(&rig.hub, plugin));
    let activating = {
        let kernel = kernel.clone();
        std::thread::spawn(move || kernel.activate())
    };
    started.recv().unwrap();
    // The commits ran, the token is still pending: the applies wait.
    assert_eq!(rig.queue.len(), 2);
    let effects = rig.drain();
    assert!(effects.is_empty());
    assert_eq!(rig.driver.deferred(), 2);
    assert!(!rig.hub.registry().has(Kind::Screen, screens::REVIEW));
    go.send(()).unwrap();
    activating.join().unwrap().unwrap();
    rig.hub.enqueue(crate::Item::Tick);
    rig.drain();
    assert_eq!(rig.driver.deferred(), 0);
    assert!(rig.hub.registry().has(Kind::Screen, screens::REVIEW));
}

#[test]
fn late_registration_applies_with_its_token() {
    let mut rig = Rig::new();
    let act = Arc::new(Mutex::new(None));
    let seen = act.clone();
    let kernel = contributor(
        &rig.hub,
        Contributor {
            staged: Arc::new(vec![Box::new(|c, t| {
                c.register(screen(screens::PROJECT_HOME, home_layout(), "Home"), t)
            })]),
            late: Arc::new(vec![Box::new(|c, t| {
                c.register(item("overview", SidebarSection::Project, 0, "Overview"), t)
            })]),
            on_started: Some(Arc::new(move |handle| {
                *seen.lock().unwrap() = Some(handle.token().id());
            })),
        },
    );
    kernel.activate().unwrap();
    assert_eq!(rig.queue.len(), 2, "the staged commit and the late apply");
    let act = act.lock().unwrap().expect("started ran");
    assert!(!rig.hub.registry().activation_applied(act));
    rig.drain();
    assert!(rig.hub.registry().activation_applied(act));
    assert_eq!(
        rig.hub.registry().applied(),
        vec![
            (Kind::Screen, name(screens::PROJECT_HOME)),
            (Kind::SidebarEntry, name("overview")),
        ]
    );
    // A registration without a token outside `activate` waits for a commit
    // that never comes: it is never shown.
    let _orphan = rig
        .hub
        .register(plain(ContributionKind::Panel, "context"), None);
    assert_eq!(rig.queue.len(), 0);
    assert!(!rig.hub.registry().has(Kind::Panel, "context"));
}

#[test]
fn late_registration_needs_no_earlier_commit() {
    let mut rig = Rig::new();
    let kernel = contributor(
        &rig.hub,
        Contributor {
            staged: Arc::new(Vec::new()),
            late: Arc::new(vec![
                Box::new(|c, t| c.register(screen(screens::REVIEW, review_layout(), "Review"), t)),
                Box::new(|c, t| c.register_leave_guard(FixedGuard::new(true), t)),
                Box::new(|c, t| c.handle_command(name("finder.open"), Box::new(|_| {}), t)),
            ]),
            on_started: None,
        },
    );
    kernel.activate().unwrap();
    assert_eq!(rig.queue.len(), 3, "one apply per late registration");
    rig.drain();
    assert!(rig.hub.registry().has(Kind::Screen, screens::REVIEW));
    assert_eq!(rig.hub.registry().guards().len(), 1);
    assert!(rig.hub.registry().handler("finder.open").is_some());
    kernel.deactivate();
    rig.drain();
    assert!(rig.hub.registry().applied().is_empty());
    assert_eq!(rig.queue.len(), 0);
}

#[test]
fn late_registration_inside_started_is_deferred_until_live() {
    let mut rig = Rig::new();
    let (registered, wait_registered) = mpsc::channel();
    let (go, wait_go) = mpsc::channel::<()>();
    let wait_go = Mutex::new(wait_go);
    let plugin = Contributor {
        staged: Arc::new(Vec::new()),
        late: Arc::new(vec![Box::new(move |c, t| {
            let registration = c.register(screen(screens::REVIEW, review_layout(), "Review"), t);
            // Holds `started` here: the token stays pending while the main
            // thread handles the apply.
            registered.send(()).unwrap();
            wait_go.lock().unwrap().recv().unwrap();
            registration
        })]),
        on_started: None,
    };
    let kernel = Arc::new(contributor(&rig.hub, plugin));
    let activating = {
        let kernel = kernel.clone();
        std::thread::spawn(move || kernel.activate())
    };
    wait_registered.recv().unwrap();
    assert_eq!(rig.queue.len(), 1, "the apply is queued at once");
    let effects = rig.drain();
    assert!(effects.is_empty());
    assert_eq!(rig.driver.deferred(), 1);
    assert!(!rig.hub.registry().has(Kind::Screen, screens::REVIEW));
    go.send(()).unwrap();
    activating.join().unwrap().unwrap();
    rig.hub.enqueue(crate::Item::Tick);
    rig.drain();
    assert_eq!(rig.driver.deferred(), 0);
    assert!(rig.hub.registry().has(Kind::Screen, screens::REVIEW));
}

#[test]
fn late_registration_with_a_dead_token_is_rejected() {
    let capture = lectern_logging::testing::capture();
    let mut rig = Rig::new();
    let token = Arc::new(Mutex::new(None));
    let seen = token.clone();
    let kernel = contributor(
        &rig.hub,
        Contributor {
            staged: Arc::new(Vec::new()),
            late: Arc::new(Vec::new()),
            on_started: Some(Arc::new(move |handle| {
                *seen.lock().unwrap() = Some(handle.token());
            })),
        },
    );
    kernel.activate().unwrap();
    let token = token.lock().unwrap().clone().expect("started ran");
    kernel.deactivate();
    rig.drain();
    // A task of the deactivated plugin registers: nothing is queued, the
    // slot stays free, the payload drops on this thread.
    let late = rig.hub.register(
        screen(screens::REVIEW, review_layout(), "Review"),
        Some(token.clone()),
    );
    let late_guard = rig
        .hub
        .register_leave_guard(FixedGuard::new(true), Some(token));
    assert_eq!(rig.queue.len(), 0);
    assert!(!rig.hub.registry().has(Kind::Screen, screens::REVIEW));
    assert!(rig.hub.registry().guards().is_empty());
    drop(late);
    drop(late_guard);
    assert_eq!(rig.queue.len(), 0);
    let _free = rig
        .hub
        .register(screen(screens::REVIEW, review_layout(), "Review"), None);
    assert_eq!(
        capture
            .lines()
            .iter()
            .filter(|l| l.contains("\"msg\":\"contribution rejected\"")
                && l.contains("\"reason\":\"token-dead\""))
            .count(),
        2
    );
}

#[test]
fn removal_drops_the_shown_screen() {
    let mut rig = Rig::new();
    let refusing = FixedGuard::new(false);
    let guard = refusing.clone();
    let mut plugin = screens_plugin();
    Arc::get_mut(&mut plugin.staged)
        .unwrap()
        .push(Box::new(move |c, t| {
            c.register_leave_guard(guard.clone(), t)
        }));
    let kernel = contributor(&rig.hub, plugin);
    kernel.activate().unwrap();
    rig.drain();
    rig.open("/repos/a", &project_key("/repos/a"));
    rig.dispatch(WorkspaceCommand::Push(review_route()));
    assert_eq!(rig.driver.nav().nav().depth(), 1);

    kernel.deactivate();
    let effects = rig.drain();
    assert_eq!(rig.driver.nav().nav().depth(), 0, "popped");
    assert!(refusing.asked().is_empty(), "no guard was asked");
    assert!(changed_to(&effects, screens::PROJECT_HOME));
    assert!(rig.hub.registry().applied().is_empty());
    // The base screen is gone too: the content area shows nothing, the
    // state machine stays on the project.
    assert_eq!(
        rig.driver.nav().nav().base,
        Base::Project(project_key("/repos/a"))
    );
}

#[test]
fn screens_can_be_pushed_twice() {
    let mut rig = Rig::new();
    let kernel = contributor(&rig.hub, screens_plugin());
    kernel.activate().unwrap();
    rig.drain();
    rig.open("/repos/a", &project_key("/repos/a"));
    rig.dispatch(WorkspaceCommand::Push(review_route()));
    // Another target: the same route on top would be ignored.
    rig.dispatch(WorkspaceCommand::Push(Route {
        screen: name(screens::REVIEW),
        target: Target::Project(project_key("/repos/a")),
    }));
    assert_eq!(rig.driver.nav().nav().depth(), 2);
    // The factory stays in the registry: it is taken once per push.
    for _ in 0..2 {
        let title = rig
            .hub
            .registry()
            .with_payload(Kind::Screen, screens::REVIEW, |payload| {
                payload.0.downcast_ref::<ScreenPayload>().unwrap().0.clone()
            });
        assert_eq!(title.as_deref(), Some("Review"));
    }
}

#[test]
fn release_frees_the_id_before_the_main_thread_removes_it() {
    let mut rig = Rig::new();
    let first = contributor(&rig.hub, screens_plugin());
    first.activate().unwrap();
    rig.drain();
    first.deactivate();
    assert_eq!(rig.queue.len(), 2, "the removes wait for the main thread");

    let second = contributor(
        &rig.hub,
        Contributor::staged(vec![Box::new(|c, t| {
            c.register(screen(screens::REVIEW, review_layout(), "Review 2"), t)
        })]),
    );
    second.activate().unwrap();
    rig.drain();
    assert_eq!(
        rig.hub.registry().applied(),
        vec![(Kind::Screen, name(screens::REVIEW))]
    );
    let title = rig
        .hub
        .registry()
        .with_payload(Kind::Screen, screens::REVIEW, |payload| {
            payload.0.downcast_ref::<ScreenPayload>().unwrap().0.clone()
        });
    assert_eq!(title.as_deref(), Some("Review 2"));
}

#[test]
fn sidebar_entries_are_sorted_and_shared() {
    let mut rig = Rig::new();
    let kernel = contributor(
        &rig.hub,
        Contributor::staged(vec![
            Box::new(|c, t| c.register(item("usage", SidebarSection::Bottom, 0, "Usage"), t)),
            Box::new(|c, t| {
                c.register(item("sessions", SidebarSection::Project, 1, "Sessions"), t)
            }),
            Box::new(|c, t| {
                c.register(item("overview", SidebarSection::Project, 0, "Overview"), t)
            }),
            Box::new(|c, t| c.register(item("files", SidebarSection::Views, 0, "Files"), t)),
            Box::new(|c, t| {
                c.register(item("worktrees", SidebarSection::Views, 0, "Worktrees"), t)
            }),
        ]),
    );
    assert!(rig.hub.sidebar_entries().is_empty());
    kernel.activate().unwrap();
    rig.drain();
    let entries = rig.hub.sidebar_entries();
    let ids: Vec<&str> = entries.iter().map(|e| &*e.id).collect();
    assert_eq!(ids, ["overview", "sessions", "files", "worktrees", "usage"]);
    let labels: Vec<String> = entries
        .iter()
        .map(|e| {
            e.payload
                .0
                .clone()
                .downcast::<ItemPayload>()
                .unwrap()
                .0
                .clone()
        })
        .collect();
    assert_eq!(labels[0], "Overview");
    // The snapshot is one `Arc`, replaced whole.
    let same = rig.hub.sidebar_entries();
    assert!(Arc::ptr_eq(&entries, &same));
    kernel.deactivate();
    let effects = rig.drain();
    assert!(rig.hub.sidebar_entries().is_empty());
    assert!(effects.contains(&Effect::Event(WorkspaceEvent::ContributionsChanged)));
}

#[test]
fn command_handlers_guards_and_key_bindings_apply() {
    let mut rig = Rig::new();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = calls.clone();
    let guard = FixedGuard::new(true);
    let registered = guard.clone();
    let kernel = contributor(
        &rig.hub,
        Contributor::staged(vec![
            Box::new(move |c, t| {
                let seen = seen.clone();
                c.handle_command(
                    name("finder.open"),
                    Box::new(move |call| seen.lock().unwrap().push(call.clone())),
                    t,
                )
            }),
            Box::new(move |c, t| c.register_leave_guard(registered.clone(), t)),
            Box::new(|c, t| c.register(plain(ContributionKind::KeyBindings, "review-keys"), t)),
        ]),
    );
    kernel.activate().unwrap();
    let effects = rig.drain();
    let bound: Vec<Serial> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::BindKeys(serial) => Some(*serial),
            _ => None,
        })
        .collect();
    assert_eq!(bound.len(), 1);
    assert!(
        rig.hub
            .registry()
            .with_serial(bound[0], |payload| payload.0.is::<Marker>())
            .unwrap()
    );
    let handler = rig.hub.registry().handler("finder.open").expect("applied");
    (handler.handler)(&lectern_services::workspace::CommandCall {
        window: WINDOW,
        name: name("finder.open"),
    });
    assert_eq!(calls.lock().unwrap().len(), 1);
    let guards = rig.hub.registry().guards();
    assert_eq!(guards.len(), 1);
    assert!(rig.hub.registry().guard(guards[0]).is_some());
    assert!(rig.hub.registry().handler("nope").is_none());
    kernel.deactivate();
    rig.drain();
    assert!(rig.hub.registry().handler("finder.open").is_none());
    assert!(rig.hub.registry().guards().is_empty());
}

#[test]
fn leave_guards_are_asked_through_the_queue() {
    let mut rig = Rig::new();
    let allowing = FixedGuard::new(true);
    let refusing = FixedGuard::new(false);
    let mut plugin = screens_plugin();
    let guard = allowing.clone();
    Arc::get_mut(&mut plugin.staged)
        .unwrap()
        .push(Box::new(move |c, t| {
            c.register_leave_guard(guard.clone(), t)
        }));
    let screens = contributor(&rig.hub, plugin);
    screens.activate().unwrap();
    rig.drain();
    rig.open("/repos/a", &project_key("/repos/a"));
    rig.dispatch(WorkspaceCommand::Push(review_route()));

    // One allowing guard: asked for `View`, answered through the queue.
    rig.hub.dispatch(WorkspaceCommand::Pop);
    let mut out = Vec::new();
    rig.driver.handle(rig.queue.pop().unwrap(), &mut out);
    assert_eq!(allowing.asked(), vec![LeaveScope::View]);
    assert!(!out.iter().any(|e| matches!(e, Effect::Ask { .. })));
    assert_eq!(rig.driver.nav().nav().depth(), 1, "not before the answer");
    assert_eq!(
        rig.queue.items(),
        ["GuardAnswer { ask: AskId(1), allowed: true }"]
    );
    rig.drain();
    assert_eq!(rig.driver.nav().nav().depth(), 0);

    // A refusing guard registered later: asked after the first, stops it.
    let guard = refusing.clone();
    let editor = contributor(
        &rig.hub,
        Contributor::staged(vec![Box::new(move |c, t| {
            c.register_leave_guard(guard.clone(), t)
        })]),
    );
    editor.activate().unwrap();
    rig.drain();
    rig.dispatch(WorkspaceCommand::Push(review_route()));
    rig.dispatch(WorkspaceCommand::Pop);
    assert_eq!(allowing.asked().len(), 2);
    assert_eq!(refusing.asked(), vec![LeaveScope::View]);
    assert_eq!(rig.driver.nav().nav().depth(), 1, "refused");

    // Its plugin deactivated, the guard is gone: the pop goes through.
    editor.deactivate();
    rig.drain();
    rig.dispatch(WorkspaceCommand::Pop);
    assert_eq!(refusing.asked().len(), 1);
    assert_eq!(rig.driver.nav().nav().depth(), 0);
}

#[test]
fn commands_wait_for_projects() {
    let mut rig = Rig::prepared();
    rig.hub
        .dispatch(WorkspaceCommand::OpenProject(PathBuf::from("/repos/a")));
    rig.hub.dispatch(WorkspaceCommand::PromptOpenProject);
    let effects = rig.drain();
    assert_eq!(effects, vec![Effect::PromptOpenProject]);
    assert_eq!(rig.driver.deferred(), 1);
    assert!(rig.projects.calls().is_empty());

    rig.kernel.activate().unwrap();
    rig.drain();
    assert_eq!(rig.driver.deferred(), 0);
    assert_eq!(
        rig.projects.calls(),
        vec![ProjectCall::Open(PathBuf::from("/repos/a"))]
    );
}

#[test]
fn project_events_reach_the_state_machine() {
    let mut rig = Rig::new();
    let key = project_key("/repos/a");
    let effects = rig.open("/repos/a", &key);
    assert!(changed_to(&effects, screens::PROJECT_HOME));
    assert_eq!(rig.driver.nav().nav().base, Base::Project(key.clone()));
    assert_eq!(
        rig.driver.nav().state().project,
        Some(PathBuf::from("/repos/a"))
    );
    rig.emit(ProjectEvent::Changed {
        project: key.clone(),
        generation: 1,
        parts: ChangedParts::default(),
    });
    assert_eq!(rig.queue.len(), 0, "Changed is not queued");
    rig.emit(closed(&key));
    assert_eq!(rig.driver.nav().nav().base, Base::Welcome);
    // Milestones pass through as effects.
    rig.hub
        .report_milestone(lectern_services::workspace::Milestone::ProjectHome);
    assert_eq!(
        rig.drain(),
        vec![Effect::Milestone(
            lectern_services::workspace::Milestone::ProjectHome
        )]
    );
}

#[test]
fn dispatch_after_shutdown_is_dropped() {
    let rig = Rig::new();
    let Rig {
        queue, hub, kernel, ..
    } = rig;
    drop(kernel);
    hub.dispatch(WorkspaceCommand::Escape);
    assert_eq!(queue.len(), 0);
}

/// A guard that drops `done` without answering.
struct Silent;

impl lectern_services::workspace::LeaveGuard for Silent {
    fn ask(&self, _scope: LeaveScope, done: Box<dyn FnOnce(bool) + Send>) {
        drop(done);
    }
}

#[test]
fn a_guard_that_drops_its_answer_allows() {
    let mut rig = Rig::new();
    let mut plugin = screens_plugin();
    Arc::get_mut(&mut plugin.staged)
        .unwrap()
        .push(Box::new(|c, t| c.register_leave_guard(Arc::new(Silent), t)));
    let kernel = contributor(&rig.hub, plugin);
    kernel.activate().unwrap();
    rig.drain();
    rig.open("/repos/a", &project_key("/repos/a"));
    rig.dispatch(WorkspaceCommand::Push(review_route()));
    rig.dispatch(WorkspaceCommand::Pop);
    assert_eq!(rig.driver.nav().nav().depth(), 0, "the round ended");
    assert!(!rig.driver.nav().asking());
}

#[test]
fn a_registration_without_a_token_outside_activate_is_reported_on_release() {
    let capture = lectern_logging::testing::capture();
    let rig = Rig::new();
    let orphan = rig
        .hub
        .register(plain(ContributionKind::Panel, "context"), None);
    assert_eq!(rig.queue.len(), 0);
    drop(orphan);
    assert_eq!(rig.queue.len(), 0, "dropped on the caller's thread");
    assert!(capture.lines().iter().any(|l| {
        l.contains("\"msg\":\"contribution released without a commit\"")
            && l.contains("\"id\":\"context\"")
    }));
}
