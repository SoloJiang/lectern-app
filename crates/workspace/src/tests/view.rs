//! The root view in a test window (docs/design/workspace.md「测试」, the
//! `#[gpui::test]` list): regions, actions, guards, the proxy, restore,
//! state reports and frame recording.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{
    AppContext, Context, FocusHandle, InteractiveElement, IntoElement, KeyBinding, Render, Styled,
    TestAppContext, VisualTestContext, Window, actions, div,
};
use lectern_kernel::{
    Activation, Active, Context as KernelContext, Kernel, Manifest, Name, Plugin, PluginError,
    PluginSet, service,
};
use lectern_logging::testing;
use lectern_services::project::Projects;
use lectern_services::workspace::{
    Contribution, ContributionKind, Contributions, LeaveScope, WorkspaceCommand, screens,
};
use lectern_ui::contrib::KeyBindings;

use crate::WorkspacePlugin;
use crate::actions::{Back, CloseWindow, Command, OpenFolder};
use crate::host::WindowRoot;
use crate::hub::RecordStop;
use crate::nav::Base;
use crate::state::WindowState;

use super::support::*;
use super::ui_support::*;

#[gpui::test]
fn install_replaces_the_root_and_shows_nothing_before_a_screen_registers(cx: &mut TestAppContext) {
    let rig = UiRig::new(cx);
    let replaced = rig
        .window
        .update(cx, |root, _, _| root.downcast::<WindowRoot>().is_ok())
        .unwrap();
    assert!(replaced, "the window's root is the workspace");
    let tree = rig.tree(cx);
    assert!(tree.contains("workspace.content [Group] base=welcome column=full"));
    assert!(
        tree.contains("workspace.content.missing [Text] label=workspace-screen-missing"),
        "no screen is registered yet: {tree}"
    );
    assert!(!tree.contains("workspace.sidebar"));
    assert!(!tree.contains("workspace.toolbar"));

    // The Welcome screen registers: the content area shows it.
    let built = Arc::new(AtomicUsize::new(0));
    let _welcome = rig.contribute(
        Contributor::staged(vec![ui_screen(
            screens::WELCOME,
            welcome_layout(),
            built.clone(),
        )]),
        cx,
    );
    let tree = rig.tree(cx);
    assert!(tree.contains("workspace.content.screen[welcome] [Group] index=0"));
    assert!(!tree.contains("missing"));
    assert_eq!(built.load(Ordering::SeqCst), 1, "the factory ran once");
}

/// `app::startup::release` runs in the skeleton window's frame callback,
/// with that window leased: `install` must still end with the workspace as
/// the root.
#[gpui::test]
fn install_inside_the_windows_update_mounts_after_it(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(|cx| {
        lectern_ui::init_fixed(
            cx,
            lectern_i18n::Strings::of(lectern_i18n::Locale::En),
            lectern_ui::Appearance::Light,
            false,
        );
    });
    struct Placeholder;
    impl Render for Placeholder {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }
    let window: gpui::AnyWindowHandle = cx.add_window(|_, _| Placeholder).into();
    let received = Arc::new(Received::default());
    let deps = received.deps();
    let host = cx
        .update_window(window, |_, window, cx| {
            crate::install(window.window_handle(), deps, cx)
        })
        .unwrap();
    cx.run_until_parked();
    let replaced = window
        .update(cx, |root, _, _| root.downcast::<WindowRoot>().is_ok())
        .unwrap();
    assert!(replaced, "the deferred mount replaced the root");
    assert_eq!(host.window(), Some(window));
    let tree = cx.update(|cx| {
        let workspace = crate::host::workspace(cx).expect("installed");
        workspace.read_with(cx, |workspace, cx| {
            crate::semantic::render(&workspace.inspect(cx))
        })
    });
    assert!(tree.contains("workspace.content [Group] base=welcome"));
}

#[gpui::test]
fn project_home_shows_after_opened(cx: &mut TestAppContext) {
    let rig = UiRig::new(cx);
    let built = Arc::new(AtomicUsize::new(0));
    let mut registrations = standard_screens();
    registrations[1] = ui_screen(screens::PROJECT_HOME, home_layout(), built.clone());
    let _plugins = rig.contribute(Contributor::staged(registrations), cx);

    rig.open(project_path(), &project(), cx);
    assert_eq!(rig.nav(cx).base, Base::Project(project()));
    let tree = rig.tree(cx);
    assert!(
        tree.contains("workspace.sidebar [Sidebar] panel=project"),
        "{tree}"
    );
    assert!(tree.contains("workspace.sidebar.panel[project] [Group]"));
    assert!(tree.contains("workspace.toolbar [Toolbar] screen=project-home depth=0"));
    assert!(tree.contains("workspace.content [Group] base=project column=reading"));
    assert!(tree.contains("workspace.content.screen[project-home] [Group] index=0"));
    assert!(
        !tree.contains("workspace.toolbar.back"),
        "the home has no back button"
    );
    assert!(
        !tree.contains("workspace.inspector"),
        "the home has no inspector"
    );
    assert_eq!(built.load(Ordering::SeqCst), 1);
    assert_eq!(
        rig.host.hub().sidebar_entries().len(),
        2,
        "the sidebar entries are there for the project sidebar to render"
    );
}

#[gpui::test]
fn review_hides_the_sidebar_and_shows_the_bar(cx: &mut TestAppContext) {
    let rig = UiRig::new(cx);
    let _plugins = rig.contribute(Contributor::staged(standard_screens()), cx);
    rig.open(project_path(), &project(), cx);
    rig.push(screens::REVIEW, cx);

    let tree = rig.tree(cx);
    assert!(!tree.contains("workspace.sidebar"), "{tree}");
    assert!(tree.contains("workspace.toolbar [Toolbar] screen=review depth=1"));
    assert!(tree.contains("workspace.toolbar.back [Button] label=workspace-back"));
    assert!(tree.contains(
        "workspace.toolbar.inspector [Toggle] label=workspace-inspector-toggle checked=false"
    ));
    assert!(tree.contains("workspace.content.screen[project-home] [Group] index=0"));
    assert!(tree.contains("workspace.content.screen[review] [Group] index=1"));
    assert!(tree.contains("workspace.bar [Group]"));
}

#[gpui::test]
fn set_reading_column_widens_the_content(cx: &mut TestAppContext) {
    let rig = UiRig::new(cx);
    let _plugins = rig.contribute(Contributor::staged(standard_screens()), cx);
    rig.open(project_path(), &project(), cx);
    rig.push(screens::REVIEW, cx);
    assert!(
        rig.tree(cx)
            .contains("workspace.content [Group] base=project column=reading")
    );

    rig.dispatch(WorkspaceCommand::SetReadingColumn(false), cx);
    rig.settle(cx);
    assert!(
        rig.tree(cx)
            .contains("workspace.content [Group] base=project column=full")
    );

    rig.dispatch(WorkspaceCommand::Pop, cx);
    rig.settle(cx);
    assert!(
        rig.tree(cx).contains("column=reading"),
        "the home is back in the column"
    );
}

#[gpui::test]
fn escape_and_back_return_to_the_home(cx: &mut TestAppContext) {
    let rig = UiRig::new(cx);
    let _plugins = rig.contribute(Contributor::staged(standard_screens()), cx);
    rig.open(project_path(), &project(), cx);

    rig.push(screens::REVIEW, cx);
    assert_eq!(rig.nav(cx).depth(), 1);
    cx.simulate_keystrokes(rig.window, "escape");
    rig.settle(cx);
    assert_eq!(rig.nav(cx).depth(), 0, "Esc pops the review");

    rig.push(screens::REVIEW, cx);
    cx.dispatch_action(rig.window, Back);
    rig.settle(cx);
    assert_eq!(rig.nav(cx).depth(), 0, "Back returns to the home");

    rig.push(screens::REVIEW, cx);
    cx.simulate_keystrokes(rig.window, "cmd-[");
    rig.settle(cx);
    assert_eq!(rig.nav(cx).depth(), 0, "⌘[ returns to the home");
}

#[gpui::test]
fn inspector_toggles_with_alt_cmd_i(cx: &mut TestAppContext) {
    let rig = UiRig::new(cx);
    let _plugins = rig.contribute(Contributor::staged(standard_screens()), cx);
    rig.open(project_path(), &project(), cx);
    rig.push(screens::REVIEW, cx);

    cx.simulate_keystrokes(rig.window, "alt-cmd-i");
    rig.settle(cx);
    assert!(rig.nav(cx).inspector);
    let tree = rig.tree(cx);
    assert!(
        tree.contains("workspace.inspector [Panel] tab=files"),
        "{tree}"
    );
    assert!(tree.contains(
        "workspace.inspector.tab[files] [Tab] label=workspace-inspector-toggle selected"
    ));
    assert!(tree.contains(
        "workspace.inspector.tab[conversation] [Tab] label=workspace-inspector-toggle\n"
    ));
    assert!(tree.contains(
        "workspace.toolbar.inspector [Toggle] label=workspace-inspector-toggle checked=true"
    ));

    cx.simulate_keystrokes(rig.window, "cmd-\\");
    rig.settle(cx);
    let nav = rig.nav(cx);
    assert_eq!(nav.inspector_tab.as_deref(), Some("conversation"));
    assert!(
        rig.tree(cx)
            .contains("workspace.inspector [Panel] tab=conversation")
    );

    cx.simulate_keystrokes(rig.window, "alt-cmd-i");
    rig.settle(cx);
    assert!(!rig.nav(cx).inspector);
    assert!(!rig.tree(cx).contains("workspace.inspector [Panel]"));
}

#[gpui::test]
fn inspector_commands_are_ignored_without_an_inspector(cx: &mut TestAppContext) {
    let rig = UiRig::new(cx);
    let _plugins = rig.contribute(Contributor::staged(standard_screens()), cx);
    rig.open(project_path(), &project(), cx);

    cx.simulate_keystrokes(rig.window, "alt-cmd-i");
    cx.simulate_keystrokes(rig.window, "cmd-\\");
    rig.settle(cx);
    let nav = rig.nav(cx);
    assert!(!nav.inspector, "the home has no inspector");
    assert_eq!(nav.inspector_tab, None);
    assert_eq!(
        rig.received.states().len(),
        1,
        "only the open changed the state"
    );
}

#[gpui::test]
fn close_window_asks_the_guard_first(cx: &mut TestAppContext) {
    let rig = UiRig::new(cx);
    let refusing = FixedGuard::new(false);
    let guard = refusing.clone();
    let refuser = rig.contribute(
        Contributor::staged(vec![Box::new(move |c, t| {
            c.register_leave_guard(guard.clone(), t)
        })]),
        cx,
    );

    let mut visual = VisualTestContext::from_window(rig.window, cx);
    assert!(!visual.simulate_close(), "the close is intercepted");
    rig.settle(cx);
    assert_eq!(refusing.asked(), vec![LeaveScope::Window]);
    assert_eq!(cx.windows().len(), 1, "the refusing guard keeps the window");
    assert_eq!(rig.notice(cx), None, "the guard shows its own dialog");

    refuser.deactivate();
    rig.settle(cx);
    cx.dispatch_action(rig.window, CloseWindow);
    rig.settle(cx);
    assert!(
        cx.windows().is_empty(),
        "with no guard left the window closes"
    );
}

#[gpui::test]
fn restore_opens_the_last_project(cx: &mut TestAppContext) {
    let rig = UiRig::prepared(cx);
    rig.host.restore(WindowState {
        version: WindowState::VERSION,
        project: Some(PathBuf::from(project_path())),
        inspector: true,
    });
    cx.run_until_parked();
    assert!(
        rig.projects.calls().is_empty(),
        "the open waits for dyn Projects"
    );

    rig.activate(cx);
    assert_eq!(
        rig.projects.calls(),
        vec![ProjectCall::Open(PathBuf::from(project_path()))]
    );
    let _plugins = rig.contribute(Contributor::staged(standard_screens()), cx);
    rig.emit(opened(1, &project()), cx);
    let nav = rig.nav(cx);
    assert_eq!(nav.base, Base::Project(project()));
    assert!(nav.inspector, "the inspector flag was restored");

    // A version this crate does not know is ignored.
    rig.host.restore(WindowState {
        version: 99,
        project: Some(PathBuf::from("/elsewhere")),
        inspector: false,
    });
    rig.settle(cx);
    assert_eq!(rig.projects.calls().len(), 1);
}

#[gpui::test]
fn state_changes_are_reported(cx: &mut TestAppContext) {
    let rig = UiRig::new(cx);
    let _plugins = rig.contribute(Contributor::staged(standard_screens()), cx);
    rig.open(project_path(), &project(), cx);
    let states = rig.received.states();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].project, Some(PathBuf::from(project_path())));
    assert!(!states[0].inspector);

    rig.push(screens::REVIEW, cx);
    cx.simulate_keystrokes(rig.window, "alt-cmd-i");
    rig.settle(cx);
    let states = rig.received.states();
    assert_eq!(states.len(), 2);
    assert!(states[1].inspector);

    rig.emit(closed(&project()), cx);
    let states = rig.received.states();
    assert_eq!(states.last().map(|s| s.project.clone()), Some(None));
}

#[gpui::test]
fn open_folder_prompts_and_opens_the_chosen_path(cx: &mut TestAppContext) {
    let rig = UiRig::new(cx);
    cx.dispatch_action(rig.window, OpenFolder);
    rig.settle(cx);
    assert!(cx.did_prompt_for_paths(), "⌘O shows the open panel");
    cx.simulate_path_prompt_response(|_| Some(vec![PathBuf::from(project_path())]));
    rig.settle(cx);
    assert_eq!(
        rig.projects.calls(),
        vec![ProjectCall::Open(PathBuf::from(project_path()))]
    );
}

#[gpui::test]
fn screens_can_be_pushed_twice(cx: &mut TestAppContext) {
    let rig = UiRig::new(cx);
    let built = Arc::new(AtomicUsize::new(0));
    let mut registrations = standard_screens();
    registrations[2] = ui_screen(screens::REVIEW, review_layout(), built.clone());
    let _plugins = rig.contribute(Contributor::staged(registrations), cx);
    rig.open(project_path(), &project(), cx);
    rig.push(screens::REVIEW, cx);
    rig.push(screens::REVIEW, cx);
    assert_eq!(rig.nav(cx).depth(), 2);
    assert_eq!(built.load(Ordering::SeqCst), 2, "one view per stack entry");
    rig.dispatch(WorkspaceCommand::Pop, cx);
    assert_eq!(rig.nav(cx).depth(), 1);
    assert_eq!(
        built.load(Ordering::SeqCst),
        2,
        "the remaining view is kept"
    );
}

#[gpui::test]
fn record_frames_reports_once_per_scenario(cx: &mut TestAppContext) {
    let rig = UiRig::new(cx);
    rig.host
        .record_frames(name("scroll"), RecordStop::After(Duration::ZERO));
    rig.settle(cx);
    let frames = rig.received.frames();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].scenario, "scroll");

    rig.host.record_frames(name("input"), RecordStop::Manual);
    rig.settle(cx);
    assert_eq!(rig.received.frames().len(), 1, "manual waits for stop");
    rig.host.stop_recording();
    rig.settle(cx);
    let frames = rig.received.frames();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[1].scenario, "input");
    rig.host.stop_recording();
    rig.settle(cx);
    assert_eq!(
        rig.received.frames().len(),
        2,
        "a second stop reports nothing"
    );
}

#[gpui::test]
fn attach_moves_the_workspace_to_a_new_window(cx: &mut TestAppContext) {
    let rig = UiRig::new(cx);
    let _plugins = rig.contribute(Contributor::staged(standard_screens()), cx);
    rig.open(project_path(), &project(), cx);

    struct Empty;
    impl Render for Empty {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }
    let second: gpui::AnyWindowHandle = cx.add_window(|_, _| Empty).into();
    cx.update(|cx| rig.host.attach(second, cx));
    rig.settle(cx);
    assert_eq!(rig.host.window(), Some(second));
    let replaced = second
        .update(cx, |root, _, _| root.downcast::<WindowRoot>().is_ok())
        .unwrap();
    assert!(replaced);
    assert_eq!(
        rig.nav(cx).base,
        Base::Project(project()),
        "the navigation state stays"
    );
    assert!(
        rig.tree(cx)
            .contains("workspace.content.screen[project-home]")
    );
}

#[gpui::test]
fn contribution_closures_run_in_the_plugins_scope(cx: &mut TestAppContext) {
    let capture = testing::capture();
    testing::set_callsite_limit(1_000_000);
    let rig = UiRig::new(cx);
    let probe: Register = Box::new(|contributions, token| {
        let payload = lectern_ui::contrib::Screen {
            build: Arc::new(|_, _, cx| {
                lectern_logging::info!("factory probe");
                cx.new(|_| Probe).into()
            }),
            toolbar: Arc::new(|_| lectern_ui::contrib::ToolbarItems {
                title: lectern_ui::Text::verbatim(String::from("probe")),
                subtitle: None,
                controls: Vec::new(),
            }),
        };
        contributions.register(
            Contribution {
                kind: ContributionKind::Screen(welcome_layout()),
                id: Name::Borrowed(screens::WELCOME),
                order: 0,
                payload: payload.into_payload(),
            },
            token,
        )
    });
    struct Probe;
    impl Render for Probe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }
    let _plugin = rig.contribute(Contributor::staged(vec![probe]), cx);
    let lines = capture.lines();
    let probe_line = lines
        .iter()
        .find(|line| line.contains("\"msg\":\"factory probe\""))
        .expect("the factory ran and logged");
    assert!(
        probe_line.contains("\"plugin\":\"plugin-under-test\"") && probe_line.contains("\"act\":"),
        "the factory ran in its plugin's scope: {probe_line}"
    );
}

actions!(workspace_test, [Alpha, Beta, Quit]);

/// A screen that handles one action when it has the focus, counting the
/// calls.
struct Handler<A: gpui::Action> {
    focus: FocusHandle,
    count: Arc<AtomicUsize>,
    _action: std::marker::PhantomData<A>,
}

impl<A: gpui::Action> Render for Handler<A> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focus.contains_focused(window, cx) {
            window.focus(&self.focus, cx);
        }
        let count = self.count.clone();
        div()
            .size_full()
            .track_focus(&self.focus)
            .on_action(move |_: &A, _, _| {
                count.fetch_add(1, Ordering::SeqCst);
            })
    }
}

/// A screen whose view handles `A`, with a key binding of `key` to `A`.
fn handling_screen<A: gpui::Action + Clone + Default>(
    id: &'static str,
    layout: lectern_services::workspace::ScreenLayout,
    key: &'static str,
    count: Arc<AtomicUsize>,
) -> Vec<Register> {
    let screen_registration: Register = Box::new(move |contributions, token| {
        let count = count.clone();
        let payload = lectern_ui::contrib::Screen {
            build: Arc::new(move |_, window, cx| {
                let view = cx.new(|cx| Handler::<A> {
                    focus: cx.focus_handle(),
                    count: count.clone(),
                    _action: std::marker::PhantomData,
                });
                let focus = view.read(cx).focus.clone();
                window.focus(&focus, cx);
                view.into()
            }),
            toolbar: Arc::new(|_| lectern_ui::contrib::ToolbarItems {
                title: lectern_ui::Text::verbatim(String::from("handler")),
                subtitle: None,
                controls: Vec::new(),
            }),
        };
        contributions.register(
            Contribution {
                kind: ContributionKind::Screen(layout),
                id: Name::Borrowed(id),
                order: 0,
                payload: payload.into_payload(),
            },
            token,
        )
    });
    let bindings: Register = Box::new(move |contributions, token| {
        contributions.register(
            Contribution {
                kind: ContributionKind::KeyBindings,
                id: Name::Owned(format!("{id}-keys")),
                order: 0,
                payload: KeyBindings(vec![(key, Box::new(A::default()), None)]).into_payload(),
            },
            token,
        )
    });
    vec![screen_registration, bindings]
}

#[gpui::test]
fn stale_bindings_fall_through(cx: &mut TestAppContext) {
    let rig = UiRig::new(cx);
    // The app's own global binding, as ⌘Q is.
    let quits = Arc::new(AtomicUsize::new(0));
    let counted = quits.clone();
    cx.update(|cx| {
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        cx.on_action(move |_: &Quit, _| {
            counted.fetch_add(1, Ordering::SeqCst);
        });
    });

    let alphas = Arc::new(AtomicUsize::new(0));
    let betas = Arc::new(AtomicUsize::new(0));
    let mut home = handling_screen::<Beta>(
        screens::PROJECT_HOME,
        home_layout(),
        "ctrl-t",
        betas.clone(),
    );
    home.push(sidebar_panel("project"));
    let _home = rig.contribute(Contributor::staged(home), cx);
    let alpha = rig.contribute(
        Contributor::staged(handling_screen::<Alpha>(
            screens::REVIEW,
            review_layout(),
            "ctrl-t",
            alphas.clone(),
        )),
        cx,
    );
    rig.open(project_path(), &project(), cx);

    cx.simulate_keystrokes(rig.window, "ctrl-t");
    rig.settle(cx);
    assert_eq!(
        (betas.load(Ordering::SeqCst), alphas.load(Ordering::SeqCst)),
        (1, 0)
    );

    rig.push(screens::REVIEW, cx);
    cx.simulate_keystrokes(rig.window, "ctrl-t");
    rig.settle(cx);
    assert_eq!(
        (betas.load(Ordering::SeqCst), alphas.load(Ordering::SeqCst)),
        (1, 1)
    );

    rig.dispatch(WorkspaceCommand::Pop, cx);
    alpha.deactivate();
    rig.settle(cx);
    cx.simulate_keystrokes(rig.window, "ctrl-t");
    rig.settle(cx);
    assert_eq!(
        (betas.load(Ordering::SeqCst), alphas.load(Ordering::SeqCst)),
        (2, 1),
        "the deactivated plugin's binding falls through to the home's handler"
    );

    cx.simulate_keystrokes(rig.window, "cmd-q");
    rig.settle(cx);
    assert_eq!(
        quits.load(Ordering::SeqCst),
        1,
        "the app's global binding still works"
    );
}

// ----- the proxy: a real kernel with a command-activated plugin -------------

/// Provides `dyn Projects` in the real kernel.
struct ProjectsPlugin(Arc<FakeProjects>);

impl Plugin for ProjectsPlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: vec![service::<dyn Projects>()],
            requires: Vec::new(),
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    fn activate(&self, ctx: &mut KernelContext<'_>) -> Result<Box<dyn Active>, PluginError> {
        let projects: Arc<dyn Projects> = self.0.clone();
        ctx.provide::<dyn Projects>(projects)?;
        Ok(Box::new(()))
    }
}

/// Activated by its command; registers the handler in `activate`.
struct CommandPlugin {
    command: &'static str,
    calls: Arc<AtomicUsize>,
    /// Register no handler: the activation succeeds but the command stays
    /// without one.
    forgetful: bool,
}

impl Plugin for CommandPlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: Vec::new(),
            requires: vec![service::<dyn Contributions>()],
            activation: vec![Activation::Command(Name::Borrowed(self.command))],
        }
    }

    fn activate(&self, ctx: &mut KernelContext<'_>) -> Result<Box<dyn Active>, PluginError> {
        if !self.forgetful {
            let contributions = ctx.service::<dyn Contributions>();
            let service = contributions.get().expect("the hub is live");
            let calls = self.calls.clone();
            ctx.own_staged(service.handle_command(
                Name::Borrowed(self.command),
                Box::new(move |_| {
                    calls.fetch_add(1, Ordering::SeqCst);
                }),
                None,
            ));
        }
        Ok(Box::new(()))
    }
}

/// A started kernel, shut down when dropped.
struct RealKernel(Arc<Kernel>);

impl Drop for RealKernel {
    fn drop(&mut self) {
        let (done, wait) = std::sync::mpsc::channel();
        self.0.shutdown(move || {
            let _ = done.send(());
        });
        let _ = wait.recv_timeout(Duration::from_secs(10));
    }
}

#[gpui::test]
fn proxy_action_activates_the_command_plugin_and_replays_once(cx: &mut TestAppContext) {
    let capture = testing::capture();
    testing::set_callsite_limit(1_000_000);
    cx.executor().allow_parking();
    cx.update(|cx| {
        lectern_ui::init_fixed(
            cx,
            lectern_i18n::Strings::of(lectern_i18n::Locale::En),
            lectern_ui::Appearance::Light,
            false,
        );
    });
    struct Placeholder;
    impl Render for Placeholder {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }
    let window: gpui::AnyWindowHandle = cx.add_window(|_, _| Placeholder).into();
    let received = Arc::new(Received::default());
    let host = cx.update(|cx| crate::install(window, received.deps(), cx));
    let projects = Arc::new(FakeProjects::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let kernel = RealKernel(Kernel::start(
        PluginSet::new()
            .with("projects", ProjectsPlugin(projects))
            .with("workspace", WorkspacePlugin::new(host.clone()))
            .with(
                "probe",
                CommandPlugin {
                    command: "probe.run",
                    calls: calls.clone(),
                    forgetful: false,
                },
            )
            .with(
                "forgetful",
                CommandPlugin {
                    command: "probe.forget",
                    calls: Arc::default(),
                    forgetful: true,
                },
            ),
    ));
    let settle = |cx: &mut TestAppContext| {
        kernel.0.barrier();
        cx.run_until_parked();
        kernel.0.barrier();
        cx.run_until_parked();
    };
    settle(cx);
    let workspace = cx.update(|cx| crate::host::workspace(cx).expect("installed"));
    let notice = |cx: &mut TestAppContext| workspace.read_with(cx, |w, _| w.notice());

    // The first trigger activates the plugin and replays once.
    cx.dispatch_action(
        window,
        Command {
            name: "probe.run".to_owned(),
        },
    );
    settle(cx);
    host.hub().enqueue(crate::hub::Item::Tick);
    settle(cx);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "activated and replayed once"
    );
    assert_eq!(notice(cx), None);

    // The second trigger finds the handler.
    cx.dispatch_action(
        window,
        Command {
            name: "probe.run".to_owned(),
        },
    );
    settle(cx);
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // An unknown command fails once, with the notice.
    cx.dispatch_action(
        window,
        Command {
            name: "probe.missing".to_owned(),
        },
    );
    settle(cx);
    assert_eq!(notice(cx), Some("workspace-command-failed"));
    assert!(
        capture.lines().iter().any(|line| {
            line.contains("\"msg\":\"command activation failed\"")
                && line.contains("\"error\":\"unknown_command\"")
        }),
        "the failure is logged with the error's kind"
    );

    // A plugin that activates but registers no handler: the notice too.
    cx.dispatch_action(
        window,
        Command {
            name: "probe.forget".to_owned(),
        },
    );
    settle(cx);
    host.hub().enqueue(crate::hub::Item::Tick);
    settle(cx);
    assert_eq!(notice(cx), Some("workspace-command-failed"));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    drop(kernel);
}

#[gpui::test]
fn ask_leave_answers_through_the_guards(cx: &mut TestAppContext) {
    let rig = UiRig::new(cx);
    let answers = Arc::new(Mutex::new(Vec::new()));
    let ask = |rig: &UiRig, cx: &mut TestAppContext| {
        let answers = answers.clone();
        rig.host.ask_leave(
            LeaveScope::App,
            Box::new(move |allowed| answers.lock().unwrap().push(allowed)),
        );
        rig.settle(cx);
    };
    ask(&rig, cx);
    assert_eq!(
        *answers.lock().unwrap(),
        vec![true],
        "no guard: allowed at once"
    );

    let refusing = FixedGuard::new(false);
    let guard = refusing.clone();
    let _refuser = rig.contribute(
        Contributor::staged(vec![Box::new(move |c, t| {
            c.register_leave_guard(guard.clone(), t)
        })]),
        cx,
    );
    ask(&rig, cx);
    assert_eq!(*answers.lock().unwrap(), vec![true, false]);
    assert_eq!(refusing.asked(), vec![LeaveScope::App]);
}

#[test]
fn the_panel_is_420pt_and_narrows_with_the_window() {
    use crate::view::{PANEL_INSET, PANEL_WIDTH, SIDEBAR_WIDTH, panel_width};
    use gpui::px;
    assert_eq!(panel_width(px(1180.0), true), PANEL_WIDTH);
    assert_eq!(panel_width(px(820.0), false), PANEL_WIDTH);
    // Too narrow: the content area less the two insets.
    let narrow = SIDEBAR_WIDTH + px(300.0);
    assert_eq!(panel_width(narrow, true), px(300.0) - PANEL_INSET * 2.0);
    assert_eq!(panel_width(px(10.0), false), px(0.0));
}
