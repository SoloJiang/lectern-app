//! The navigation state machine (docs/design/workspace.md「导航状态机」):
//! one test per row of the table, the guard rounds, and what the window
//! state follows.

use std::path::PathBuf;

use lectern_services::project::{OpenError, OpenRequest, ProjectEvent};
use lectern_services::workspace::{
    Layer, LeaveScope, Route, Target, WindowId, WorkspaceCommand, WorkspaceEvent, screens,
};

use super::support::*;
use crate::effect::{Effect, Notice};
use crate::nav::{AskId, Base, GuardId, Navigator};
use crate::state::WindowState;

fn review() -> Route {
    Route {
        screen: name(screens::REVIEW),
        target: Target::None,
    }
}

fn reader() -> Route {
    Route {
        screen: name(screens::READER),
        target: Target::None,
    }
}

/// A navigator on project `a`, with `env` standard.
fn on_project(env: &FakeEnv) -> Navigator {
    let mut nav = Navigator::new(WINDOW);
    let mut out = Vec::new();
    nav.dispatch(
        WorkspaceCommand::OpenProject(PathBuf::from("/repos/a")),
        env,
        &mut out,
    );
    nav.project_event(&opened(1, &project_key("/repos/a")), env, &mut out);
    assert_eq!(nav.nav().base, Base::Project(project_key("/repos/a")));
    nav
}

fn screen_changed(effects: &[Effect]) -> Vec<(String, String)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Event(WorkspaceEvent::ScreenChanged { from, to, .. }) => Some((
                from.as_ref()
                    .map(|r| r.screen.to_string())
                    .unwrap_or_default(),
                to.screen.to_string(),
            )),
            _ => None,
        })
        .collect()
}

fn layers(effects: &[Effect]) -> Vec<(Layer, bool)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Event(WorkspaceEvent::LayerChanged { layer, open, .. }) => {
                Some((layer.clone(), *open))
            }
            _ => None,
        })
        .collect()
}

fn asks(effects: &[Effect]) -> Vec<(AskId, GuardId, LeaveScope)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Ask { ask, guard, scope } => Some((*ask, *guard, scope.clone())),
            _ => None,
        })
        .collect()
}

fn states(effects: &[Effect]) -> Vec<WindowState> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::StateChanged(state) => Some(state.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn opened_switches_the_base_and_closes_the_old_project() {
    let env = FakeEnv::standard();
    let mut nav = on_project(&env);
    let mut out = Vec::new();
    nav.dispatch(WorkspaceCommand::Push(review()), &env, &mut out);
    nav.dispatch(
        WorkspaceCommand::ShowSidebarPanel(Some(name("files"))),
        &env,
        &mut out,
    );
    out.clear();

    // Opened with another key, nothing on the stack: the base switches, the
    // stack and the sidebar panel reset, the old project is closed.
    nav.dispatch(WorkspaceCommand::PopToBase, &env, &mut out);
    out.clear();
    let b = project_key("/repos/b");
    nav.dispatch(
        WorkspaceCommand::OpenProject(PathBuf::from("/repos/b")),
        &env,
        &mut out,
    );
    nav.project_event(&opened(2, &b), &env, &mut out);
    assert_eq!(nav.nav().base, Base::Project(b.clone()));
    assert!(nav.nav().stack.is_empty());
    assert_eq!(nav.nav().sidebar_panel, None);
    assert_eq!(
        screen_changed(&out),
        vec![(
            screens::PROJECT_HOME.to_owned(),
            screens::PROJECT_HOME.to_owned()
        )]
    );
    assert_eq!(
        states(&out),
        vec![WindowState {
            version: 1,
            project: Some(PathBuf::from("/repos/b")),
            inspector: false,
        }]
    );
    assert_eq!(
        env.calls().last(),
        Some(&ProjectCall::Close(project_key("/repos/a")))
    );
}

#[test]
fn switching_projects_closes_the_old_one() {
    let env = FakeEnv::standard();
    let mut nav = on_project(&env);
    let mut out = Vec::new();
    nav.project_event(&opened(99, &project_key("/repos/b")), &env, &mut out);
    assert_eq!(
        env.calls(),
        vec![
            ProjectCall::Open(PathBuf::from("/repos/a")),
            ProjectCall::Close(project_key("/repos/a")),
        ]
    );
    // The path of an open this window did not request is unknown.
    assert_eq!(nav.state().project, None);
}

#[test]
fn reopening_the_same_project_keeps_the_stack() {
    let env = FakeEnv::standard();
    let mut nav = on_project(&env);
    let mut out = Vec::new();
    nav.dispatch(WorkspaceCommand::Push(review()), &env, &mut out);
    out.clear();
    nav.dispatch(
        WorkspaceCommand::OpenProject(PathBuf::from("/repos/a/sub")),
        &env,
        &mut out,
    );
    nav.project_event(&opened(2, &project_key("/repos/a")), &env, &mut out);
    assert_eq!(nav.nav().stack, vec![review()]);
    assert!(screen_changed(&out).is_empty());
    // The newest path is what a restart reopens.
    assert_eq!(nav.state().project, Some(PathBuf::from("/repos/a/sub")));
    assert_eq!(states(&out).len(), 1);
    assert!(
        !env.calls()
            .contains(&ProjectCall::Close(project_key("/repos/a")))
    );
}

#[test]
fn closed_returns_to_welcome() {
    let env = FakeEnv::standard();
    let mut nav = on_project(&env);
    let mut out = Vec::new();
    nav.dispatch(WorkspaceCommand::Push(review()), &env, &mut out);
    nav.dispatch(WorkspaceCommand::SetInspector(true), &env, &mut out);
    out.clear();
    nav.project_event(&closed(&project_key("/repos/a")), &env, &mut out);
    assert_eq!(nav.nav().base, Base::Welcome);
    assert!(nav.nav().stack.is_empty());
    assert!(!nav.nav().inspector);
    assert_eq!(
        layers(&out),
        vec![(Layer::Inspector, false)],
        "the open layers close"
    );
    assert_eq!(
        screen_changed(&out),
        vec![(screens::REVIEW.to_owned(), screens::WELCOME.to_owned())]
    );
    assert_eq!(states(&out).last(), Some(&WindowState::default()));
}

#[test]
fn closed_for_another_project_is_ignored() {
    let env = FakeEnv::standard();
    let mut nav = on_project(&env);
    let mut out = Vec::new();
    nav.project_event(&closed(&project_key("/repos/b")), &env, &mut out);
    assert_eq!(nav.nav().base, Base::Project(project_key("/repos/a")));
    assert!(out.is_empty());
}

#[test]
fn open_project_from_welcome_needs_no_guard() {
    let mut env = FakeEnv::standard();
    env.guards = vec![GuardId(1)];
    let mut nav = Navigator::new(WINDOW);
    let mut out = Vec::new();
    nav.dispatch(
        WorkspaceCommand::OpenProject(PathBuf::from("/repos/a")),
        &env,
        &mut out,
    );
    assert!(asks(&out).is_empty());
    assert_eq!(
        env.calls(),
        vec![ProjectCall::Open(PathBuf::from("/repos/a"))]
    );
    out.clear();
    nav.dispatch(WorkspaceCommand::PromptOpenProject, &env, &mut out);
    assert_eq!(out, vec![Effect::PromptOpenProject]);
}

#[test]
fn open_project_with_a_project_asks_the_project_guard() {
    let mut env = FakeEnv::standard();
    let mut nav = on_project(&env);
    env.guards = vec![GuardId(7)];
    let mut out = Vec::new();
    nav.dispatch(
        WorkspaceCommand::OpenProject(PathBuf::from("/repos/b")),
        &env,
        &mut out,
    );
    let asked = asks(&out);
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].1, GuardId(7));
    assert_eq!(asked[0].2, LeaveScope::Project(project_key("/repos/a")));
    assert_eq!(env.calls().len(), 1, "not opened before the answer");
    nav.guard_answered(asked[0].0, true, &env, &mut out);
    assert_eq!(
        env.calls().last(),
        Some(&ProjectCall::Open(PathBuf::from("/repos/b")))
    );
}

#[test]
fn close_project_asks_the_project_guard() {
    let mut env = FakeEnv::standard();
    let mut nav = on_project(&env);
    env.guards = vec![GuardId(1)];
    let mut out = Vec::new();
    nav.dispatch(WorkspaceCommand::CloseProject, &env, &mut out);
    let asked = asks(&out);
    assert_eq!(asked[0].2, LeaveScope::Project(project_key("/repos/a")));
    nav.guard_answered(asked[0].0, true, &env, &mut out);
    assert_eq!(
        env.calls().last(),
        Some(&ProjectCall::Close(project_key("/repos/a")))
    );
}

#[test]
fn set_active_worktree_asks_the_worktree_guard() {
    let mut env = FakeEnv::standard();
    let mut nav = on_project(&env);
    env.guards = vec![GuardId(1)];
    let mut out = Vec::new();
    nav.dispatch(
        WorkspaceCommand::SetActiveWorktree(worktree_key("/repos/a/.git/worktrees/x")),
        &env,
        &mut out,
    );
    let asked = asks(&out);
    assert_eq!(asked[0].2, LeaveScope::Worktree(project_key("/repos/a")));
    nav.guard_answered(asked[0].0, true, &env, &mut out);
    assert_eq!(
        env.calls().last(),
        Some(&ProjectCall::SetActiveWorktree(
            project_key("/repos/a"),
            worktree_key("/repos/a/.git/worktrees/x")
        ))
    );
}

#[test]
fn push_needs_a_registered_screen() {
    let mut env = FakeEnv::standard();
    env.panels.insert("context".to_owned());
    env.overlays.insert("finder".to_owned());
    let mut nav = on_project(&env);
    let mut out = Vec::new();
    nav.dispatch(WorkspaceCommand::OpenPanel(name("context")), &env, &mut out);
    nav.dispatch(
        WorkspaceCommand::OpenOverlay(name("finder")),
        &env,
        &mut out,
    );
    out.clear();

    nav.dispatch(WorkspaceCommand::Push(reader()), &env, &mut out);
    assert!(nav.nav().stack.is_empty(), "the reader is not registered");
    assert!(out.is_empty());

    nav.dispatch(WorkspaceCommand::Push(review()), &env, &mut out);
    assert_eq!(nav.nav().stack, vec![review()]);
    assert_eq!(nav.nav().depth(), 1);
    assert_eq!(nav.nav().panel, None);
    assert_eq!(nav.nav().overlay, None);
    assert_eq!(
        layers(&out),
        vec![
            (Layer::Panel(name("context")), false),
            (Layer::Overlay(name("finder")), false),
        ]
    );
    assert_eq!(
        screen_changed(&out),
        vec![(screens::PROJECT_HOME.to_owned(), screens::REVIEW.to_owned())]
    );
}

#[test]
fn pop_asks_the_view_guard() {
    let mut env = FakeEnv::standard();
    let mut nav = on_project(&env);
    let mut out = Vec::new();
    nav.dispatch(WorkspaceCommand::Push(review()), &env, &mut out);
    env.guards = vec![GuardId(1)];
    out.clear();
    nav.dispatch(WorkspaceCommand::Pop, &env, &mut out);
    let asked = asks(&out);
    assert_eq!(asked[0].2, LeaveScope::View);
    assert_eq!(nav.nav().depth(), 1);
    nav.guard_answered(asked[0].0, true, &env, &mut out);
    assert_eq!(nav.nav().depth(), 0);
    assert_eq!(
        screen_changed(&out),
        vec![(screens::REVIEW.to_owned(), screens::PROJECT_HOME.to_owned())]
    );
}

#[test]
fn set_reading_column_overrides_the_top_screen_until_it_pops() {
    let env = FakeEnv::standard();
    let mut nav = on_project(&env);
    let mut out = Vec::new();
    nav.dispatch(WorkspaceCommand::Push(review()), &env, &mut out);
    assert!(nav.layout(&env).reading_column);

    out.clear();
    nav.dispatch(WorkspaceCommand::SetReadingColumn(false), &env, &mut out);
    assert!(!nav.layout(&env).reading_column);
    assert!(nav.layout(&env).inspector, "only the column changes");
    assert!(out.is_empty(), "no event: the root view redraws on its own");

    // Another screen above the review has its own layout; the override
    // resumes when it pops.
    nav.dispatch(WorkspaceCommand::Push(review()), &env, &mut out);
    assert!(
        nav.layout(&env).reading_column,
        "the screen above is not overridden"
    );
    nav.dispatch(WorkspaceCommand::SetReadingColumn(false), &env, &mut out);
    nav.dispatch(WorkspaceCommand::Pop, &env, &mut out);
    assert!(
        !nav.layout(&env).reading_column,
        "the lower override came back"
    );

    nav.dispatch(WorkspaceCommand::SetReadingColumn(true), &env, &mut out);
    assert!(nav.layout(&env).reading_column);
    nav.dispatch(WorkspaceCommand::SetReadingColumn(false), &env, &mut out);
    nav.dispatch(WorkspaceCommand::Pop, &env, &mut out);
    assert!(nav.nav().reading_column.is_empty(), "popping drops it");
    nav.dispatch(WorkspaceCommand::Push(review()), &env, &mut out);
    assert!(
        nav.layout(&env).reading_column,
        "a new screen starts from its layout"
    );
}

#[test]
fn set_reading_column_is_ignored_without_a_project_and_cleared_with_it() {
    let env = FakeEnv::standard();
    let mut nav = Navigator::new(WINDOW);
    let mut out = Vec::new();
    nav.dispatch(WorkspaceCommand::SetReadingColumn(true), &env, &mut out);
    assert!(nav.nav().reading_column.is_empty(), "Welcome ignores it");

    let mut nav = on_project(&env);
    nav.dispatch(WorkspaceCommand::SetReadingColumn(false), &env, &mut out);
    assert!(
        !nav.layout(&env).reading_column,
        "the base screen takes it too"
    );
    nav.project_event(&closed(&project_key("/repos/a")), &env, &mut out);
    assert!(
        nav.nav().reading_column.is_empty(),
        "closing the project clears it"
    );
}

#[test]
fn set_reading_column_is_cleared_when_the_screen_is_removed() {
    let mut env = FakeEnv::standard();
    let mut nav = on_project(&env);
    let mut out = Vec::new();
    nav.dispatch(WorkspaceCommand::Push(review()), &env, &mut out);
    nav.dispatch(WorkspaceCommand::SetReadingColumn(false), &env, &mut out);
    env.screens.remove(screens::REVIEW);
    nav.screen_removed(screens::REVIEW, &env, &mut out);
    assert!(nav.nav().reading_column.is_empty());
}

#[test]
fn pop_to_base_asks_once_per_level() {
    let mut env = FakeEnv::standard();
    env.screens
        .insert(screens::READER.to_owned(), review_layout());
    let mut nav = on_project(&env);
    let mut out = Vec::new();
    nav.dispatch(WorkspaceCommand::Push(review()), &env, &mut out);
    nav.dispatch(WorkspaceCommand::Push(reader()), &env, &mut out);
    env.guards = vec![GuardId(1)];
    out.clear();
    nav.dispatch(WorkspaceCommand::PopToBase, &env, &mut out);
    let first = asks(&out);
    assert_eq!(first.len(), 1);
    nav.guard_answered(first[0].0, true, &env, &mut out);
    assert_eq!(nav.nav().depth(), 1);
    let second = asks(&out);
    assert_eq!(second.len(), 2, "a second round for the second level");
    assert_ne!(second[1].0, first[0].0);
    nav.guard_answered(second[1].0, true, &env, &mut out);
    assert_eq!(nav.nav().depth(), 0);
    assert_eq!(
        screen_changed(&out),
        vec![
            (screens::READER.to_owned(), screens::REVIEW.to_owned()),
            (screens::REVIEW.to_owned(), screens::PROJECT_HOME.to_owned()),
        ]
    );
}

#[test]
fn pop_to_base_stops_at_a_refusal() {
    let mut env = FakeEnv::standard();
    env.screens
        .insert(screens::READER.to_owned(), review_layout());
    let mut nav = on_project(&env);
    let mut out = Vec::new();
    nav.dispatch(WorkspaceCommand::Push(review()), &env, &mut out);
    nav.dispatch(WorkspaceCommand::Push(reader()), &env, &mut out);
    env.guards = vec![GuardId(1)];
    out.clear();
    nav.dispatch(WorkspaceCommand::PopToBase, &env, &mut out);
    let first = asks(&out);
    nav.guard_answered(first[0].0, true, &env, &mut out);
    let second = asks(&out);
    nav.guard_answered(second[1].0, false, &env, &mut out);
    assert_eq!(
        nav.nav().stack,
        vec![review()],
        "stopped at the refused level"
    );
    assert!(!nav.asking());
}

#[test]
fn escape_closes_the_topmost_layer() {
    let mut env = FakeEnv::standard();
    env.panels.insert("context".to_owned());
    env.overlays.insert("finder".to_owned());
    env.sidebar_panels.insert("files".to_owned());
    let mut nav = on_project(&env);
    let mut out = Vec::new();
    nav.dispatch(WorkspaceCommand::Push(review()), &env, &mut out);
    nav.dispatch(
        WorkspaceCommand::ShowSidebarPanel(Some(name("files"))),
        &env,
        &mut out,
    );
    nav.dispatch(WorkspaceCommand::OpenPanel(name("context")), &env, &mut out);
    nav.dispatch(
        WorkspaceCommand::OpenOverlay(name("finder")),
        &env,
        &mut out,
    );
    out.clear();

    nav.dispatch(WorkspaceCommand::Escape, &env, &mut out);
    assert_eq!(nav.nav().overlay, None);
    assert_eq!(nav.nav().panel, Some(name("context")));
    nav.dispatch(WorkspaceCommand::Escape, &env, &mut out);
    assert_eq!(nav.nav().panel, None);
    assert_eq!(nav.nav().sidebar_panel, Some(name("files")));
    nav.dispatch(WorkspaceCommand::Escape, &env, &mut out);
    assert_eq!(nav.nav().sidebar_panel, None);
    assert_eq!(nav.nav().depth(), 1);
    nav.dispatch(WorkspaceCommand::Escape, &env, &mut out);
    assert_eq!(nav.nav().depth(), 0, "no guards: popped at once");
    assert_eq!(
        layers(&out),
        vec![
            (Layer::Overlay(name("finder")), false),
            (Layer::Panel(name("context")), false),
            (Layer::SidebarPanel(name("files")), false),
        ]
    );
    out.clear();
    nav.dispatch(WorkspaceCommand::Escape, &env, &mut out);
    assert!(out.is_empty(), "nothing left to close");
}

#[test]
fn show_sidebar_panel_needs_a_registered_panel() {
    let mut env = FakeEnv::standard();
    env.sidebar_panels.insert("files".to_owned());
    let mut nav = on_project(&env);
    let mut out = Vec::new();
    nav.dispatch(
        WorkspaceCommand::ShowSidebarPanel(Some(name("nope"))),
        &env,
        &mut out,
    );
    assert_eq!(nav.nav().sidebar_panel, None);
    assert!(out.is_empty());
    nav.dispatch(
        WorkspaceCommand::ShowSidebarPanel(Some(name("files"))),
        &env,
        &mut out,
    );
    assert_eq!(nav.nav().sidebar_panel, Some(name("files")));
    nav.dispatch(WorkspaceCommand::ShowSidebarPanel(None), &env, &mut out);
    assert_eq!(nav.nav().sidebar_panel, None);
    assert_eq!(
        layers(&out),
        vec![
            (Layer::SidebarPanel(name("files")), true),
            (Layer::SidebarPanel(name("files")), false),
        ]
    );
}

#[test]
fn inspector_commands_follow_the_layout() {
    let env = FakeEnv::standard();
    let mut nav = on_project(&env);
    let mut out = Vec::new();
    // The project home has no inspector: both commands are ignored.
    nav.dispatch(WorkspaceCommand::SetInspector(true), &env, &mut out);
    nav.dispatch(
        WorkspaceCommand::SelectInspectorTab(name("files")),
        &env,
        &mut out,
    );
    assert!(!nav.nav().inspector);
    assert!(out.is_empty());

    nav.dispatch(WorkspaceCommand::Push(review()), &env, &mut out);
    out.clear();
    nav.dispatch(WorkspaceCommand::SetInspector(true), &env, &mut out);
    assert!(nav.nav().inspector);
    assert_eq!(layers(&out), vec![(Layer::Inspector, true)]);
    assert!(states(&out)[0].inspector);
    out.clear();
    nav.dispatch(WorkspaceCommand::SetInspector(true), &env, &mut out);
    assert!(out.is_empty(), "already shown");
    nav.dispatch(WorkspaceCommand::SetInspector(false), &env, &mut out);
    out.clear();
    nav.dispatch(
        WorkspaceCommand::SelectInspectorTab(name("conversation")),
        &env,
        &mut out,
    );
    assert!(nav.nav().inspector, "selecting a tab shows the inspector");
    assert_eq!(nav.nav().inspector_tab, Some(name("conversation")));
    assert_eq!(layers(&out), vec![(Layer::Inspector, true)]);
}

#[test]
fn panel_and_overlay_replace_each_other() {
    let mut env = FakeEnv::standard();
    env.panels.insert("context".to_owned());
    env.panels.insert("other".to_owned());
    env.overlays.insert("finder".to_owned());
    let mut nav = on_project(&env);
    let mut out = Vec::new();
    nav.dispatch(WorkspaceCommand::OpenPanel(name("nope")), &env, &mut out);
    assert_eq!(nav.nav().panel, None);
    nav.dispatch(WorkspaceCommand::OpenPanel(name("context")), &env, &mut out);
    nav.dispatch(WorkspaceCommand::OpenPanel(name("other")), &env, &mut out);
    assert_eq!(nav.nav().panel, Some(name("other")));
    nav.dispatch(WorkspaceCommand::OpenOverlay(name("nope")), &env, &mut out);
    assert_eq!(nav.nav().overlay, None);
    nav.dispatch(
        WorkspaceCommand::OpenOverlay(name("finder")),
        &env,
        &mut out,
    );
    nav.dispatch(WorkspaceCommand::CloseOverlay, &env, &mut out);
    nav.dispatch(WorkspaceCommand::ClosePanel, &env, &mut out);
    assert_eq!(
        layers(&out),
        vec![
            (Layer::Panel(name("context")), true),
            (Layer::Panel(name("context")), false),
            (Layer::Panel(name("other")), true),
            (Layer::Overlay(name("finder")), true),
            (Layer::Overlay(name("finder")), false),
            (Layer::Panel(name("other")), false),
        ]
    );
}

#[test]
fn close_window_asks_the_window_guard() {
    let mut env = FakeEnv::standard();
    env.guards = vec![GuardId(1)];
    let mut nav = Navigator::new(WINDOW);
    let mut out = Vec::new();
    nav.dispatch(WorkspaceCommand::CloseWindow, &env, &mut out);
    let asked = asks(&out);
    assert_eq!(asked[0].2, LeaveScope::Window);
    nav.guard_answered(asked[0].0, false, &env, &mut out);
    assert!(!out.contains(&Effect::CloseWindow));
    out.clear();
    nav.dispatch(WorkspaceCommand::CloseWindow, &env, &mut out);
    let asked = asks(&out);
    nav.guard_answered(asked[0].0, true, &env, &mut out);
    assert_eq!(
        out[1..],
        [
            Effect::CloseWindow,
            Effect::Event(WorkspaceEvent::WindowClosed { window: WINDOW }),
        ]
    );
}

#[test]
fn welcome_ignores_stack_commands() {
    let env = FakeEnv::standard();
    let mut nav = Navigator::new(WINDOW);
    let mut out = Vec::new();
    nav.dispatch(WorkspaceCommand::Push(review()), &env, &mut out);
    nav.dispatch(WorkspaceCommand::Pop, &env, &mut out);
    nav.dispatch(WorkspaceCommand::PopToBase, &env, &mut out);
    nav.dispatch(WorkspaceCommand::CloseProject, &env, &mut out);
    nav.dispatch(WorkspaceCommand::Escape, &env, &mut out);
    assert!(nav.nav().stack.is_empty());
    assert!(out.is_empty());
    assert!(env.calls().is_empty());
}

#[test]
fn guards_are_asked_in_order_and_a_refusal_stops() {
    let mut env = FakeEnv::standard();
    let mut nav = on_project(&env);
    let mut out = Vec::new();
    nav.dispatch(WorkspaceCommand::Push(review()), &env, &mut out);
    env.guards = vec![GuardId(3), GuardId(1), GuardId(2)];
    out.clear();

    nav.dispatch(WorkspaceCommand::Pop, &env, &mut out);
    let asked = asks(&out);
    assert_eq!(asked.len(), 1, "one guard at a time");
    assert_eq!(asked[0].1, GuardId(3));
    nav.guard_answered(asked[0].0, true, &env, &mut out);
    let asked = asks(&out);
    assert_eq!(asked.len(), 2);
    assert_eq!(asked[1].1, GuardId(1));
    assert_eq!(asked[1].0, asked[0].0, "the same round");
    nav.guard_answered(asked[1].0, false, &env, &mut out);
    assert_eq!(asks(&out).len(), 2, "the third guard is not asked");
    assert_eq!(nav.nav().depth(), 1, "the command is void");
    assert!(!nav.asking());
}

#[test]
fn commands_during_a_pending_ask_queue_up() {
    let mut env = FakeEnv::standard();
    env.panels.insert("context".to_owned());
    let mut nav = on_project(&env);
    let mut out = Vec::new();
    nav.dispatch(WorkspaceCommand::Push(review()), &env, &mut out);
    env.guards = vec![GuardId(1)];
    out.clear();

    nav.dispatch(WorkspaceCommand::Pop, &env, &mut out);
    let asked = asks(&out);
    nav.dispatch(WorkspaceCommand::OpenPanel(name("context")), &env, &mut out);
    nav.dispatch(WorkspaceCommand::Pop, &env, &mut out);
    assert_eq!(nav.queued(), 2);
    assert_eq!(nav.nav().panel, None, "not run during the round");
    assert_eq!(asks(&out).len(), 1, "no concurrent round");

    nav.guard_answered(asked[0].0, true, &env, &mut out);
    assert_eq!(nav.nav().depth(), 0);
    assert_eq!(
        nav.nav().panel,
        Some(name("context")),
        "queued commands ran in order"
    );
    assert_eq!(nav.queued(), 0, "the second Pop was ignored on the base");
}

#[test]
fn opened_with_a_refusing_view_keeps_the_screen() {
    let mut env = FakeEnv::standard();
    let mut nav = on_project(&env);
    let mut out = Vec::new();
    nav.dispatch(WorkspaceCommand::Push(review()), &env, &mut out);
    env.guards = vec![GuardId(1)];
    out.clear();

    let b = project_key("/repos/b");
    nav.project_event(&opened(42, &b), &env, &mut out);
    let asked = asks(&out);
    assert_eq!(asked[0].2, LeaveScope::View);
    nav.guard_answered(asked[0].0, false, &env, &mut out);
    assert_eq!(nav.nav().base, Base::Project(project_key("/repos/a")));
    assert_eq!(nav.nav().stack, vec![review()]);
    assert!(out.contains(&Effect::Notice(
        Notice::ProjectOpenedWhileAViewRefusedToLeave
    )));
    assert!(
        !env.calls()
            .contains(&ProjectCall::Close(project_key("/repos/a"))),
        "the old project stays"
    );

    // Leaving later and opening again switches.
    out.clear();
    env.guards.clear();
    nav.dispatch(WorkspaceCommand::Pop, &env, &mut out);
    nav.project_event(&opened(43, &b), &env, &mut out);
    assert_eq!(nav.nav().base, Base::Project(b));
}

#[test]
fn open_error_leaves_the_screen() {
    let env = FakeEnv::standard();
    let mut nav = Navigator::new(WINDOW);
    let mut out = Vec::new();
    nav.dispatch(
        WorkspaceCommand::OpenProject(PathBuf::from("/nowhere")),
        &env,
        &mut out,
    );
    nav.project_event(
        &ProjectEvent::Opened {
            request: OpenRequest(1),
            result: Err(OpenError::NotFound),
        },
        &env,
        &mut out,
    );
    assert_eq!(nav.nav().base, Base::Welcome);
    assert!(out.is_empty());
    assert_eq!(nav.state(), WindowState::default());
}

#[test]
fn window_state_follows_the_open_path() {
    let env = FakeEnv::standard();
    let mut nav = Navigator::new(WINDOW);
    let mut out = Vec::new();
    assert_eq!(nav.state(), WindowState::default());
    nav.dispatch(
        WorkspaceCommand::OpenProject(PathBuf::from("/repos/a/src")),
        &env,
        &mut out,
    );
    assert_eq!(nav.state().project, None, "not before Opened");
    nav.project_event(&opened(1, &project_key("/repos/a")), &env, &mut out);
    assert_eq!(nav.state().project, Some(PathBuf::from("/repos/a/src")));
    nav.project_event(&closed(&project_key("/repos/a")), &env, &mut out);
    assert_eq!(nav.state(), WindowState::default());
}

#[test]
fn screen_removed_pops_without_a_guard() {
    let mut env = FakeEnv::standard();
    let mut nav = on_project(&env);
    let mut out = Vec::new();
    nav.dispatch(WorkspaceCommand::Push(review()), &env, &mut out);
    env.guards = vec![GuardId(1)];
    env.screens.remove(screens::REVIEW);
    out.clear();
    nav.screen_removed(screens::REVIEW, &env, &mut out);
    assert_eq!(nav.nav().depth(), 0);
    assert!(asks(&out).is_empty());
    assert_eq!(
        screen_changed(&out),
        vec![(screens::REVIEW.to_owned(), screens::PROJECT_HOME.to_owned())]
    );
    // The base screen going away changes nothing: the content shows the
    // missing-screen text.
    out.clear();
    env.screens.remove(screens::PROJECT_HOME);
    nav.screen_removed(screens::PROJECT_HOME, &env, &mut out);
    assert!(out.is_empty());
}

#[test]
fn stale_guard_answers_are_ignored() {
    let mut env = FakeEnv::standard();
    let mut nav = on_project(&env);
    let mut out = Vec::new();
    nav.dispatch(WorkspaceCommand::Push(review()), &env, &mut out);
    env.guards = vec![GuardId(1)];
    out.clear();
    nav.dispatch(WorkspaceCommand::Pop, &env, &mut out);
    let asked = asks(&out);
    nav.guard_answered(asked[0].0, false, &env, &mut out);
    // A late answer for the ended round does nothing.
    nav.guard_answered(asked[0].0, true, &env, &mut out);
    nav.guard_answered(AskId(999), true, &env, &mut out);
    assert_eq!(nav.nav().depth(), 1);
}

#[test]
fn restore_sets_the_inspector_without_an_event() {
    let env = FakeEnv::standard();
    let mut nav = Navigator::new(WindowId(0));
    nav.restore_inspector(true);
    assert!(nav.nav().inspector);
    assert!(nav.state().inspector);
    let mut out = Vec::new();
    let mut nav2 = on_project(&env);
    nav2.restore_inspector(true);
    nav2.dispatch(WorkspaceCommand::Push(review()), &env, &mut out);
    assert!(
        nav2.nav().inspector,
        "shown on the first screen with an inspector"
    );
}

#[test]
fn window_state_round_trips_as_json() {
    let state = WindowState {
        version: 1,
        project: Some(PathBuf::from("/repos/a")),
        inspector: true,
    };
    let json = serde_json::to_string(&state).unwrap();
    assert_eq!(
        json,
        r#"{"version":1,"project":"/repos/a","inspector":true}"#
    );
    let back: WindowState = serde_json::from_str(&json).unwrap();
    assert_eq!(back, state);
    assert!(back.is_supported());
    let future: WindowState =
        serde_json::from_str(r#"{"version":2,"project":null,"inspector":false}"#).unwrap();
    assert!(!future.is_supported());
}
