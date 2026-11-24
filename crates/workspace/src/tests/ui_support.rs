//! The GPUI rig of the step-2 tests: `lectern_ui::init_fixed`, a test
//! window, `install`, the workspace plugin in a `TestKernel` with the fake
//! `dyn Projects`, and contributors with real `ui::contrib` payloads.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use gpui::{
    AnyWindowHandle, AppContext, Context, Entity, FocusHandle, InteractiveElement, IntoElement,
    Render, Styled, TestAppContext, Window, div,
};
use lectern_i18n::{Locale, Strings, keys};
use lectern_kernel::Name;
use lectern_kernel::testing::TestKernel;
use lectern_services::project::{ProjectEvent, ProjectKey, Projects};
use lectern_services::workspace::{
    Contribution, ContributionKind, Contributions, Milestone, ScreenLayout, SidebarSection,
    WorkspaceCommand, screens,
};
use lectern_ui::contrib::{InspectorTab, Screen, SidebarItem, SidebarPanel, ToolbarItems};
use lectern_ui::semantic::Recorder;
use lectern_ui::{Appearance, Text};

use crate::WorkspacePlugin;
use crate::host::{self, FrameRecord, WorkspaceDeps, WorkspaceHost, install};
use crate::nav::Nav;
use crate::semantic;
use crate::state::WindowState;
use crate::view::Workspace;

use super::support::{
    Contributor, FakeProjects, Register, home_layout, name, opened, review_layout, welcome_layout,
};

/// What `app` would receive through `WorkspaceDeps`.
#[derive(Default)]
pub struct Received {
    pub states: Mutex<Vec<WindowState>>,
    pub frames: Mutex<Vec<FrameRecord>>,
    pub milestones: Mutex<Vec<(Milestone, SystemTime)>>,
}

impl Received {
    pub fn deps(self: &Arc<Self>) -> WorkspaceDeps {
        let states = self.clone();
        let frames = self.clone();
        let milestones = self.clone();
        WorkspaceDeps {
            on_state_changed: Arc::new(move |state| {
                states.states.lock().unwrap().push(state);
            }),
            on_milestone: Arc::new(move |milestone, presented| {
                milestones
                    .milestones
                    .lock()
                    .unwrap()
                    .push((milestone, presented));
            }),
            on_frames: Arc::new(move |record| {
                frames.frames.lock().unwrap().push(record);
            }),
        }
    }

    pub fn states(&self) -> Vec<WindowState> {
        self.states.lock().unwrap().clone()
    }

    /// The scenario of each record received, and the kind of its error
    /// (`HookError::as_str`; None for a recording). `FrameRecord` is not
    /// `Clone`: `HookError` holds an `io::Error`.
    pub fn frames(&self) -> Vec<(String, Option<&'static str>)> {
        self.frames
            .lock()
            .unwrap()
            .iter()
            .map(|record| {
                (
                    record.scenario.to_string(),
                    record.result.as_ref().err().map(|e| e.as_str()),
                )
            })
            .collect()
    }

    pub fn milestones(&self) -> Vec<(Milestone, SystemTime)> {
        self.milestones.lock().unwrap().clone()
    }
}

/// A window with the workspace installed and the workspace plugin in a
/// test kernel.
pub struct UiRig {
    pub window: AnyWindowHandle,
    pub host: WorkspaceHost,
    pub kernel: TestKernel,
    pub projects: Arc<FakeProjects>,
    pub received: Arc<Received>,
}

/// The window's root before `install`.
struct Placeholder;

impl Render for Placeholder {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl UiRig {
    /// Everything installed, the workspace plugin not activated yet.
    pub fn prepared(cx: &mut TestAppContext) -> UiRig {
        // The kernel thread wakes the queue's foreground task; the test
        // scheduler accepts a wake from another thread only with parking
        // allowed.
        cx.executor().allow_parking();
        cx.update(|cx| {
            lectern_ui::init_fixed(cx, Strings::of(Locale::En), Appearance::Light, false);
        });
        let window: AnyWindowHandle = cx.add_window(|_, _| Placeholder).into();
        let received = Arc::new(Received::default());
        let deps = received.deps();
        let host = cx.update(|cx| install(window, deps, cx));
        let projects = Arc::new(FakeProjects::default());
        let projects_service: Arc<dyn Projects> = projects.clone();
        let kernel = TestKernel::new(WorkspacePlugin::new(host.clone()))
            .mock::<dyn Projects>(projects_service);
        UiRig {
            window,
            host,
            kernel,
            projects,
            received,
        }
    }

    /// The workspace plugin activated and the queue drained.
    pub fn new(cx: &mut TestAppContext) -> UiRig {
        let rig = UiRig::prepared(cx);
        rig.activate(cx);
        rig
    }

    pub fn activate(&self, cx: &mut TestAppContext) {
        self.kernel
            .activate()
            .expect("the workspace plugin activates");
        self.settle(cx);
    }

    /// Delivers one frame: runs the callbacks registered with
    /// `Window::on_next_frame` (a test window has no frame loop), then lets
    /// what they queued finish.
    pub fn next_frame(&self, cx: &mut TestAppContext) {
        self.window
            .update(cx, |_, window, cx| window.simulate_next_frame(cx))
            .expect("the window is open");
        self.settle(cx);
    }

    /// Draws the window once: a refresh, then the effect cycle that draws it.
    pub fn draw(&self, cx: &mut TestAppContext) {
        self.window
            .update(cx, |_, window, _| window.refresh())
            .expect("the window is open");
        cx.run_until_parked();
    }

    /// Draws one frame with the probe's recorder armed and returns the nodes
    /// the controls declared (the real `ui::semantic` records, not
    /// `Workspace::inspect`).
    pub fn record(&self, cx: &mut TestAppContext) -> Vec<lectern_ui::semantic::Record> {
        Recorder::begin(self.window.window_id());
        self.window
            .update(cx, |_, window, cx| window.draw(cx).clear(cx))
            .expect("the window is open");
        Recorder::finish()
    }

    /// Lets the kernel thread and the main-thread queue finish.
    pub fn settle(&self, cx: &mut TestAppContext) {
        if let Some(kernel) = self.kernel.inner() {
            kernel.barrier();
        }
        cx.run_until_parked();
    }

    pub fn workspace(&self, cx: &mut TestAppContext) -> Entity<Workspace> {
        cx.update(|cx| host::workspace(cx).expect("install ran"))
    }

    pub fn nav(&self, cx: &mut TestAppContext) -> Nav {
        self.workspace(cx)
            .read_with(cx, |workspace, _| workspace.driver().nav().nav().clone())
    }

    /// The regions' semantic tree as text.
    pub fn tree(&self, cx: &mut TestAppContext) -> String {
        self.workspace(cx)
            .read_with(cx, |workspace, cx| semantic::render(&workspace.inspect(cx)))
    }

    pub fn notice(&self, cx: &mut TestAppContext) -> Option<&'static str> {
        self.workspace(cx)
            .read_with(cx, |workspace, _| workspace.notice())
    }

    pub fn dispatch(&self, command: WorkspaceCommand, cx: &mut TestAppContext) {
        self.host.hub().dispatch(command);
        self.settle(cx);
    }

    /// Emits a `ProjectEvent` the way the `project` plugin would.
    pub fn emit(&self, event: ProjectEvent, cx: &mut TestAppContext) {
        let kernel = self.kernel.inner().expect("the kernel runs");
        kernel.emitter().emit(event);
        self.settle(cx);
    }

    /// Opens a project: `OpenProject`, then `Opened` for the request it made.
    pub fn open(&self, path: &str, key: &ProjectKey, cx: &mut TestAppContext) {
        self.dispatch(WorkspaceCommand::OpenProject(PathBuf::from(path)), cx);
        let request = self.projects.calls().len() as u64;
        self.emit(opened(request, key), cx);
    }

    pub fn push(&self, screen: &str, cx: &mut TestAppContext) {
        self.dispatch(
            WorkspaceCommand::Push(lectern_services::workspace::Route {
                screen: name(screen),
                target: lectern_services::workspace::Target::None,
            }),
            cx,
        );
    }

    /// Registers a contributor plugin in its own test kernel against the
    /// hub and activates it.
    pub fn contribute(&self, plugin: Contributor, cx: &mut TestAppContext) -> TestKernel {
        let kernel = super::support::contributor(self.host.hub(), plugin);
        kernel.activate().expect("the contributor activates");
        self.settle(cx);
        kernel
    }
}

/// A contributed view that counts how often it was built and takes the
/// focus when shown, as a real screen would.
pub struct Blank {
    focus: FocusHandle,
}

impl Render for Blank {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The screen on top keeps the focus, as a real screen would.
        if !self.focus.contains_focused(window, cx) {
            window.focus(&self.focus, cx);
        }
        div().size_full().track_focus(&self.focus)
    }
}

/// A screen contribution with a verbatim title; `built` counts the factory
/// calls.
pub fn ui_screen(id: &'static str, layout: ScreenLayout, built: Arc<AtomicUsize>) -> Register {
    ui_screen_counted(id, layout, built, Arc::default())
}

/// `ui_screen` whose toolbar closure counts its calls in `toolbar_calls`.
pub fn ui_screen_counted(
    id: &'static str,
    layout: ScreenLayout,
    built: Arc<AtomicUsize>,
    toolbar_calls: Arc<AtomicUsize>,
) -> Register {
    Box::new(move |contributions, token| {
        let built = built.clone();
        let payload = Screen {
            build: Arc::new(move |_, window, cx| {
                built.fetch_add(1, Ordering::SeqCst);
                let view = cx.new(|cx| Blank {
                    focus: cx.focus_handle(),
                });
                let focus = view.read(cx).focus.clone();
                window.focus(&focus, cx);
                view.into()
            }),
            toolbar: {
                let toolbar_calls = toolbar_calls.clone();
                Arc::new(move |_| {
                    toolbar_calls.fetch_add(1, Ordering::SeqCst);
                    ToolbarItems {
                        title: Text::verbatim(format!("Title of {id}")),
                        subtitle: None,
                        controls: Vec::new(),
                    }
                })
            },
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
    })
}

/// A sidebar panel contribution.
pub fn sidebar_panel(id: &'static str) -> Register {
    Box::new(move |contributions, token| {
        let payload = SidebarPanel {
            build: Arc::new(|_, _, cx| {
                cx.new(|cx| Blank {
                    focus: cx.focus_handle(),
                })
                .into()
            }),
        };
        contributions.register(
            Contribution {
                kind: ContributionKind::SidebarPanel,
                id: Name::Borrowed(id),
                order: 0,
                payload: payload.into_payload(),
            },
            token,
        )
    })
}

/// A sidebar entry.
pub fn sidebar_item(id: &'static str, order: u16) -> Register {
    Box::new(move |contributions, token| {
        let payload = SidebarItem {
            label: keys::workspace::BACK,
            icon: None,
            on_select: WorkspaceCommand::PopToBase,
        };
        contributions.register(
            Contribution {
                kind: ContributionKind::SidebarEntry {
                    section: SidebarSection::Project,
                },
                id: Name::Borrowed(id),
                order,
                payload: payload.into_payload(),
            },
            token,
        )
    })
}

/// An inspector tab labelled with this crate's own key (a real one uses
/// its contributor's key).
pub fn inspector_tab(id: &'static str, order: u16) -> Register {
    scoped_inspector_tab(id, order, None)
}

/// An inspector tab shown only on `screen` (`None`: on every screen with an
/// inspector).
pub fn scoped_inspector_tab(
    id: &'static str,
    order: u16,
    screen: Option<&'static str>,
) -> Register {
    Box::new(move |contributions, token| {
        let payload = InspectorTab {
            label: keys::workspace::INSPECTOR_TOGGLE,
            build: Arc::new(|_, _, cx| {
                cx.new(|cx| Blank {
                    focus: cx.focus_handle(),
                })
                .into()
            }),
            screen: screen.map(Name::Borrowed),
        };
        contributions.register(
            Contribution {
                kind: ContributionKind::InspectorTab,
                id: Name::Borrowed(id),
                order,
                payload: payload.into_payload(),
            },
            token,
        )
    })
}

/// The screens of the first milestone: Welcome, the project home with its
/// sidebar panel and two entries, Review with two inspector tabs.
pub fn standard_screens() -> Vec<Register> {
    vec![
        ui_screen(screens::WELCOME, welcome_layout(), Arc::default()),
        ui_screen(screens::PROJECT_HOME, home_layout(), Arc::default()),
        ui_screen(screens::REVIEW, review_layout(), Arc::default()),
        sidebar_panel("project"),
        sidebar_item("overview", 0),
        sidebar_item("sessions", 1),
        inspector_tab("files", 0),
        inspector_tab("conversation", 1),
    ]
}

pub fn project_path() -> &'static str {
    "/repos/demo"
}

pub fn project() -> ProjectKey {
    super::support::project_key(project_path())
}
