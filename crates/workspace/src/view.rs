//! The root view (docs/design/workspace.md「区域与三层」「界面」): the six
//! regions as containers, the three layers' appearance and collapse, the
//! proxied actions, the one-time notice, and the effects of the driver that
//! need a window. Everything drawn inside the regions is a contributed view.
//!
//! Colours, text roles and motion come from `lectern_ui`; this module writes
//! no colour value and no duration (ADR 0015). The layout sizes are the
//! interaction spec's (「窗口结构」).

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use gpui::{
    AnyElement, AnyView, AnyWindowHandle, App, Context, FocusHandle, InteractiveElement,
    IntoElement, KeyBinding, KeyBindingContextPredicate, KeyContext, ParentElement,
    PathPromptOptions, Pixels, Render, Styled, Task, WeakEntity, Window, div,
    prelude::FluentBuilder, px,
};
use lectern_i18n::keys;
use lectern_kernel::{Name, PluginError, contain};
use lectern_logging::{debug, error, warn};
use lectern_services::workspace::{
    Contributions, Route, Visibility, WorkspaceCommand, WorkspaceEvent,
};
use lectern_ui::contrib::{InspectorTab, KeyBindings, Overlay, Panel, Screen, SidebarPanel};
use lectern_ui::motion::{self, Effective, PANEL_CLOSE, PANEL_OPEN, Transition};
use lectern_ui::semantic::NodeId as UiNode;
use lectern_ui::{Button, Icon, Segmented, Text, TextRole, TextRoleExt, colors, strings};

use crate::actions::{
    self, Back, CloseWindow, Command, Escape, OpenFolder, ShowConversation, ToggleInspector,
};
use crate::diag::{id_text, milestone_name};
use crate::driver::Driver;
use crate::effect::{Effect, Notice};
use crate::host::{FrameRecord, WorkspaceDeps};
use crate::hub::{Hub, Item, Queued, RecordStop};
use crate::payloads::payload;
use crate::registry::{Kind, Registry, Serial};
use crate::semantic::{NodeId, Role, Semantic, Tree};

/// The regions' sizes (docs/design/interaction.md「窗口结构」).
pub const SIDEBAR_WIDTH: Pixels = px(248.0);
pub const TOOLBAR_HEIGHT: Pixels = px(52.0);
pub const INSPECTOR_WIDTH: Pixels = px(300.0);
pub const READING_COLUMN_WIDTH: Pixels = px(760.0);
pub const PANEL_RADIUS: Pixels = px(14.0);
const SEPARATOR: Pixels = px(0.5);
/// The inspector's padding around its segmented control.
const INSPECTOR_PADDING: Pixels = px(14.0);
/// The toolbar's leading inset on a screen without the sidebar, where the
/// window controls sit in the toolbar (docs/design/workspace.md「区域与三层」).
const WINDOW_CONTROLS_INSET: Pixels = px(88.0);
/// The sidebar panel shown when no other replaces it.
const PROJECT_SIDEBAR: &str = "project";
const VIEW: &str = "workspace";

/// A screen's built view, keyed by its route. `view` is None while the
/// screen is not registered or its factory failed.
struct Built {
    route: Route,
    view: Option<AnyView>,
}

/// A frame recording in progress.
struct Recording {
    scenario: Name,
    /// The `After` timer; dropping it cancels it.
    _timer: Option<Task<()>>,
}

/// The root view: one per process, shared by the windows through
/// `host::WindowRoot`.
pub struct Workspace {
    driver: Driver,
    hub: Arc<Hub>,
    deps: Arc<WorkspaceDeps>,
    window: Option<AnyWindowHandle>,
    focus: FocusHandle,
    /// Index 0 is the base screen, then the stack.
    screens: Vec<Built>,
    sidebar: Option<(Name, AnyView)>,
    inspector: Option<(Name, AnyView)>,
    panel: Option<(Name, AnyView)>,
    overlay: Option<(Name, AnyView)>,
    /// 0 = collapsed, 1 = open (ui design「控件」: the panel's slide).
    panel_slide: Transition,
    /// The inspector's segmented control: the slider's position, and the
    /// index it was last drawn on (None before the first draw).
    inspector_slider: Transition,
    inspector_drawn: Option<usize>,
    /// The one-time notice's message key; cleared by the next command.
    notice: Option<lectern_i18n::Key>,
    recording: Option<Recording>,
    tick_scheduled: bool,
    /// Whether the first `ScreenChanged` (`from: None`) went out.
    announced: bool,
}

impl Workspace {
    pub(crate) fn new(
        driver: Driver,
        hub: Arc<Hub>,
        deps: Arc<WorkspaceDeps>,
        cx: &mut Context<Self>,
    ) -> Workspace {
        Workspace {
            driver,
            hub,
            deps,
            window: None,
            focus: cx.focus_handle(),
            screens: Vec::new(),
            sidebar: None,
            inspector: None,
            panel: None,
            overlay: None,
            panel_slide: Transition::new(0.0),
            inspector_slider: Transition::new(0.0),
            inspector_drawn: None,
            notice: None,
            recording: None,
            tick_scheduled: false,
            announced: false,
        }
    }

    pub fn driver(&self) -> &Driver {
        &self.driver
    }

    pub fn hub(&self) -> &Arc<Hub> {
        &self.hub
    }

    /// The notice on screen, by message key name.
    pub fn notice(&self) -> Option<&'static str> {
        self.notice.map(|key| key.name())
    }

    /// Whether a frame recording is in progress.
    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    /// The window the view is shown in changed (`install`, `attach`): the
    /// views built for the old window go, the close is intercepted, the
    /// root takes the focus.
    pub(crate) fn bind_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.window = Some(window.window_handle());
        self.screens.clear();
        self.sidebar = None;
        self.inspector = None;
        self.panel = None;
        self.overlay = None;
        self.tick_scheduled = false;
        let hub = self.hub.clone();
        window.on_window_should_close(cx, move |_, _| {
            hub.dispatch(WorkspaceCommand::CloseWindow);
            false
        });
        window.focus(&self.focus, cx);
        cx.notify();
    }

    // ----- the queue ------------------------------------------------------

    /// One item from the queue (any thread's work reaching the main thread).
    pub(crate) fn handle_queued(&mut self, queued: Queued, cx: &mut Context<Self>) {
        match queued.item {
            Item::RecordFrames { scenario, stop } => {
                self.start_recording(scenario, stop, cx);
                return;
            }
            Item::StopRecording => {
                self.stop_recording(cx);
                return;
            }
            _ => {}
        }
        let announce = matches!(queued.item, Item::Started { .. }) && !self.announced;
        let mut effects = Vec::new();
        self.driver.handle(queued, &mut effects);
        if announce {
            self.announced = true;
            self.hub.emit(WorkspaceEvent::ScreenChanged {
                window: self.driver.nav().window(),
                from: None,
                to: self.driver.nav().nav().current(),
            });
        }
        self.apply(effects, None, cx);
    }

    /// A command from this window's actions and controls: runs on the main
    /// thread at once, in order with the queue.
    fn command(&mut self, command: WorkspaceCommand, window: &mut Window, cx: &mut Context<Self>) {
        self.notice = None;
        self.run(Item::Command(command), window, cx);
    }

    fn run(&mut self, item: Item, window: &mut Window, cx: &mut Context<Self>) {
        let mut effects = Vec::new();
        self.driver
            .handle(Queued { item, guard: None }, &mut effects);
        self.apply(effects, Some(window), cx);
    }

    /// `Item::Tick` from the next-frame callback: retries the deferred
    /// items. The callback's window is passed on: GPUI runs the callback
    /// with the window leased, so reaching it through its handle would fail
    /// and the next tick would never be armed.
    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tick_scheduled = false;
        let mut effects = Vec::new();
        self.driver.handle(
            Queued {
                item: Item::Tick,
                guard: None,
            },
            &mut effects,
        );
        self.apply(effects, Some(window), cx);
    }

    /// Runs the driver's effects; `window` is the current window when the
    /// effects come from an action in it, otherwise the window is reached
    /// through its handle.
    fn apply(
        &mut self,
        effects: Vec<Effect>,
        mut window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        for effect in effects {
            match effect {
                Effect::Ask { .. } => {}
                Effect::Event(event) => {
                    if matches!(event, WorkspaceEvent::ScreenChanged { .. }) {
                        self.notice = None;
                    }
                    self.hub.emit(event);
                }
                Effect::StateChanged(state) => (self.deps.on_state_changed)(state),
                Effect::PromptOpenProject => self.prompt_open_project(cx),
                Effect::CloseWindow => {
                    self.with_window(window.as_deref_mut(), cx, |window, _| {
                        window.remove_window()
                    });
                }
                Effect::Notice(notice) => {
                    self.notice = Some(match notice {
                        Notice::ProjectOpenedWhileAViewRefusedToLeave | Notice::CommandFailed => {
                            keys::workspace::COMMAND_FAILED
                        }
                    });
                }
                Effect::BindKeys(serial) => self.bind_keys(serial, cx),
                Effect::Milestone(milestone) => {
                    let on_milestone = self.deps.on_milestone.clone();
                    self.with_window(window.as_deref_mut(), cx, move |window, _| {
                        window.on_next_frame(move |_, _| {
                            let presented = SystemTime::now();
                            let since_epoch = presented
                                .duration_since(UNIX_EPOCH)
                                .map(|d| d.as_micros())
                                .unwrap_or(0);
                            debug!("milestone"; name = milestone_name(&milestone), presented_us = since_epoch);
                            on_milestone(milestone, presented);
                        });
                    });
                }
            }
        }
        if self.driver.deferred() > 0 && !self.tick_scheduled {
            let this = cx.weak_entity();
            // Armed only when the frame callback was registered; otherwise a
            // later `apply` tries again.
            self.tick_scheduled = self.with_window(window, cx, move |window, _| {
                window.on_next_frame(move |window, cx| {
                    let _ = this.update(cx, |workspace, cx| workspace.tick(window, cx));
                });
            });
        }
        cx.notify();
    }

    /// Runs `f` on the current window, or on the bound window through its
    /// handle. Returns whether `f` ran: not without a window, nor while the
    /// bound window is leased elsewhere (GPUI refuses a nested lease).
    fn with_window(
        &self,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Window, &mut App),
    ) -> bool {
        match window {
            Some(window) => {
                f(window, cx);
                true
            }
            None => match self.window {
                Some(handle) => handle.update(cx, |_, window, cx| f(window, cx)).is_ok(),
                None => false,
            },
        }
    }

    /// ⌘O: the system open panel; the chosen folder comes back as
    /// `OpenProject`. Cancel does nothing.
    fn prompt_open_project(&self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        let hub = self.hub.clone();
        cx.spawn(async move |_, _| {
            if let Ok(Ok(Some(paths))) = receiver.await
                && let Some(path) = paths.into_iter().next()
            {
                hub.dispatch(WorkspaceCommand::OpenProject(path));
            }
        })
        .detach();
    }

    /// A `KeyBindings` contribution applied: its keys join the keymap
    /// (never removed, 「已知限制」).
    fn bind_keys(&self, serial: Serial, cx: &mut App) {
        let bindings = self
            .driver
            .registry()
            .with_serial(serial, |payload| {
                let any: &(dyn std::any::Any + Send) = &*payload.0;
                any.downcast_ref::<KeyBindings>().map(|bindings| {
                    bindings
                        .0
                        .iter()
                        .map(|(keys, action, context)| (*keys, action.boxed_clone(), *context))
                        .collect::<Vec<_>>()
                })
            })
            .flatten()
            .unwrap_or_default();
        let mut loaded = Vec::with_capacity(bindings.len());
        for (keystrokes, action, context) in bindings {
            let predicate = match context {
                Some(context) => match KeyBindingContextPredicate::parse(context) {
                    Ok(predicate) => Some(Rc::new(predicate)),
                    Err(_) => {
                        warn!("key binding rejected"; reason = "context");
                        continue;
                    }
                },
                None => None,
            };
            match KeyBinding::load(
                keystrokes,
                action,
                predicate,
                false,
                None,
                cx.keyboard_mapper().as_ref(),
            ) {
                Ok(binding) => loaded.push(binding),
                Err(_) => warn!("key binding rejected"; reason = "keystrokes"),
            }
        }
        if !loaded.is_empty() {
            cx.bind_keys(loaded);
        }
    }

    // ----- frame recording -----------------------------------------------

    fn start_recording(&mut self, scenario: Name, stop: RecordStop, cx: &mut Context<Self>) {
        self.stop_recording(cx);
        let timer = match stop {
            RecordStop::After(duration) => {
                let this = cx.weak_entity();
                Some(cx.spawn(async move |_, cx| {
                    cx.background_executor().timer(duration).await;
                    let _ = this.update(cx, |workspace, cx| workspace.stop_recording(cx));
                }))
            }
            RecordStop::AnimationsSettled => {
                let this = cx.weak_entity();
                self.with_window(None, cx, move |window, _| settle_check(this, window));
                None
            }
            RecordStop::Manual => None,
        };
        self.recording = Some(Recording {
            scenario,
            _timer: timer,
        });
    }

    /// Ends the recording and hands it to `app`. Until `platform` records
    /// frames the record carries none (「已知限制」).
    fn stop_recording(&mut self, _cx: &mut Context<Self>) {
        if let Some(recording) = self.recording.take() {
            (self.deps.on_frames)(FrameRecord {
                scenario: recording.scenario,
                frames: Vec::new(),
            });
        }
    }

    // ----- views -----------------------------------------------------------

    /// Builds and drops the contributed views to match the navigation state
    /// (「`ui::contrib` 的载荷类型」: built when needed, dropped when not).
    fn sync_views(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let nav = self.driver.nav().nav().clone();
        let registry = self.driver.registry().clone();

        // Screens: the base, then the stack.
        let mut routes = vec![nav.base_route()];
        routes.extend(nav.stack.iter().cloned());
        self.screens.truncate(routes.len());
        for (index, route) in routes.into_iter().enumerate() {
            let stale = self
                .screens
                .get(index)
                .is_none_or(|built| built.route != route);
            if stale {
                self.screens.truncate(index);
                self.screens.push(Built { route, view: None });
            }
            let built = &mut self.screens[index];
            if built.view.is_none() && registry.has(Kind::Screen, &built.route.screen) {
                built.view = build_screen(&registry, &built.route, window, cx);
            }
        }

        // The sidebar panel.
        let sidebar_shown = self.driver.nav().layout(&registry_env(&registry)).sidebar
            == Visibility::Shown
            && nav.project().is_some();
        let panel_id = nav
            .sidebar_panel
            .clone()
            .unwrap_or(Name::Borrowed(PROJECT_SIDEBAR));
        self.sidebar = self.sidebar.take().filter(|(id, _)| {
            sidebar_shown && *id == panel_id && registry.has(Kind::SidebarPanel, id)
        });
        if sidebar_shown && self.sidebar.is_none() && registry.has(Kind::SidebarPanel, &panel_id) {
            let view = payload::<SidebarPanel>(&registry, Kind::SidebarPanel, &panel_id).and_then(
                |panel| {
                    in_scope(&registry, Kind::SidebarPanel, &panel_id, || {
                        (panel.build)(&(), window, cx)
                    })
                },
            );
            if let Some(view) = view {
                self.sidebar = Some((panel_id, view));
            }
        }

        // The inspector's selected tab.
        let inspector_shown = self.inspector_shown();
        let tab_id = inspector_shown.then(|| self.selected_tab()).flatten();
        self.inspector = self
            .inspector
            .take()
            .filter(|(id, _)| tab_id.as_ref() == Some(id) && registry.has(Kind::InspectorTab, id));
        if let Some(tab_id) = tab_id
            && self.inspector.is_none()
            && registry.has(Kind::InspectorTab, &tab_id)
        {
            let view =
                payload::<InspectorTab>(&registry, Kind::InspectorTab, &tab_id).and_then(|tab| {
                    in_scope(&registry, Kind::InspectorTab, &tab_id, || {
                        (tab.build)(&(), window, cx)
                    })
                });
            if let Some(view) = view {
                self.inspector = Some((tab_id, view));
            }
        }

        // The floating panel: kept while it slides out.
        let slide_target = if nav.panel.is_some() { 1.0 } else { 0.0 };
        let motion = if nav.panel.is_some() {
            PANEL_OPEN
        } else {
            PANEL_CLOSE
        };
        self.panel_slide
            .go(slide_target, motion, Instant::now(), cx);
        let keep_panel = nav.panel.is_some() || self.panel_slide.value() > 0.0;
        self.panel = self.panel.take().filter(|(id, _)| {
            keep_panel
                && (nav.panel.as_ref() == Some(id) || nav.panel.is_none())
                && registry.has(Kind::Panel, id)
        });
        if let Some(panel_id) = &nav.panel
            && self.panel.is_none()
            && registry.has(Kind::Panel, panel_id)
        {
            let view = payload::<Panel>(&registry, Kind::Panel, panel_id).and_then(|panel| {
                in_scope(&registry, Kind::Panel, panel_id, || {
                    (panel.build)(&(), window, cx)
                })
            });
            if let Some(view) = view {
                self.panel = Some((panel_id.clone(), view));
            }
        }

        // The overlay.
        self.overlay = self
            .overlay
            .take()
            .filter(|(id, _)| nav.overlay.as_ref() == Some(id) && registry.has(Kind::Overlay, id));
        if let Some(overlay_id) = &nav.overlay
            && self.overlay.is_none()
            && registry.has(Kind::Overlay, overlay_id)
        {
            let view =
                payload::<Overlay>(&registry, Kind::Overlay, overlay_id).and_then(|overlay| {
                    in_scope(&registry, Kind::Overlay, overlay_id, || {
                        (overlay.build)(&(), window, cx)
                    })
                });
            if let Some(view) = view {
                self.overlay = Some((overlay_id.clone(), view));
            }
        }
    }

    fn inspector_shown(&self) -> bool {
        let registry = self.driver.registry();
        let nav = self.driver.nav();
        nav.layout(&registry_env(registry)).inspector && nav.nav().inspector
    }

    /// The inspector tabs by `(order, id)`.
    fn inspector_tabs(&self) -> Vec<Name> {
        self.driver.registry().applied_of(Kind::InspectorTab)
    }

    /// The selected tab: the navigation's choice when registered, else the
    /// first tab.
    fn selected_tab(&self) -> Option<Name> {
        let tabs = self.inspector_tabs();
        match &self.driver.nav().nav().inspector_tab {
            Some(tab) if tabs.contains(tab) => Some(tab.clone()),
            _ => tabs.first().cloned(),
        }
    }

    // ----- the semantic tree -------------------------------------------------

    /// The regions' semantic nodes for this frame (「UI 探针的语义节点」), as
    /// the probe will read them once `ui::semantic` records them.
    pub fn inspect(&self, cx: &App) -> Vec<crate::semantic::Node> {
        let registry = self.driver.registry();
        let nav = self.driver.nav().nav();
        let layout = self.driver.nav().layout(&registry_env(registry));
        let current = nav.current();
        let strings = strings(cx);
        let mut tree = Tree::default();
        let project = nav.project().is_some();

        if project && layout.sidebar == Visibility::Shown {
            let panel_id = nav
                .sidebar_panel
                .clone()
                .unwrap_or(Name::Borrowed(PROJECT_SIDEBAR));
            let sidebar = tree.push(
                None,
                Semantic::new(NodeId::new(VIEW, "sidebar", None), Role::Sidebar)
                    .attr("panel", panel_id.as_ref()),
            );
            if registry.has(Kind::SidebarPanel, &panel_id) {
                tree.push(
                    Some(sidebar),
                    Semantic::new(
                        NodeId::new(VIEW, "sidebar", None).keyed("panel", &panel_id),
                        Role::Group,
                    ),
                );
            }
        }
        if project {
            let toolbar = tree.push(
                None,
                Semantic::new(NodeId::new(VIEW, "toolbar", None), Role::Toolbar)
                    .attr("screen", current.screen.as_ref())
                    .attr("depth", nav.depth().to_string()),
            );
            if layout.back_button {
                let back = Text::key(&strings, keys::workspace::BACK);
                tree.push(
                    Some(toolbar),
                    Semantic::new(
                        NodeId::new(VIEW, "toolbar", None).child("back"),
                        Role::Button,
                    )
                    .label(back.message_id(), back.as_str()),
                );
            }
            if layout.inspector {
                let label = Text::key(&strings, keys::workspace::INSPECTOR_TOGGLE);
                let mut toggle = Semantic::new(
                    NodeId::new(VIEW, "toolbar", None).child("inspector"),
                    Role::Toggle,
                )
                .label(label.message_id(), label.as_str());
                toggle.state.checked = Some(nav.inspector);
                tree.push(Some(toolbar), toggle);
            }
        }
        let content = tree.push(
            None,
            Semantic::new(NodeId::new(VIEW, "content", None), Role::Group)
                .attr("base", if project { "project" } else { "welcome" })
                .attr(
                    "column",
                    if layout.reading_column {
                        "reading"
                    } else {
                        "full"
                    },
                ),
        );
        let mut routes = vec![nav.base_route()];
        routes.extend(nav.stack.iter().cloned());
        for (index, route) in routes.iter().enumerate() {
            if registry.has(Kind::Screen, &route.screen) {
                tree.push(
                    Some(content),
                    Semantic::new(
                        NodeId::new(VIEW, "content", None).keyed("screen", &route.screen),
                        Role::Group,
                    )
                    .attr("index", index.to_string()),
                );
            }
        }
        if !registry.has(Kind::Screen, &current.screen) {
            let missing = Text::key(&strings, keys::workspace::SCREEN_MISSING);
            tree.push(
                Some(content),
                Semantic::new(
                    NodeId::new(VIEW, "content", None).child("missing"),
                    Role::Text,
                )
                .label(missing.message_id(), missing.as_str()),
            );
        }
        if self.inspector_shown() {
            let tabs = self.inspector_tabs();
            let selected = self.selected_tab();
            let inspector = tree.push(
                None,
                Semantic::new(NodeId::new(VIEW, "inspector", None), Role::Panel)
                    .attr("tab", selected.as_deref().unwrap_or("")),
            );
            let bar = tree.push(
                Some(inspector),
                Semantic::new(
                    NodeId::new(VIEW, "inspector", None).child("tabs"),
                    Role::TabBar,
                ),
            );
            for tab in &tabs {
                let label = payload::<InspectorTab>(registry, Kind::InspectorTab, tab)
                    .map(|tab| Text::key(&strings, tab.label));
                let mut node = Semantic::new(
                    NodeId::new(VIEW, "inspector", None).keyed("tab", tab),
                    Role::Tab,
                );
                if let Some(label) = label {
                    node = node.label(label.message_id(), label.as_str());
                }
                node.state.selected = selected.as_ref() == Some(tab);
                tree.push(Some(bar), node);
            }
        }
        if let Some(panel) = &nav.panel {
            tree.push(
                None,
                Semantic::new(NodeId::new(VIEW, "panel", Some(panel)), Role::Panel),
            );
        }
        if let Some(overlay) = &nav.overlay {
            tree.push(
                None,
                Semantic::new(NodeId::new(VIEW, "overlay", Some(overlay)), Role::Panel),
            );
        }
        if project && layout.floating_bar {
            tree.push(
                None,
                Semantic::new(NodeId::new(VIEW, "bar", None), Role::Group),
            );
        }
        if let Some(key) = self.notice {
            let text = Text::key(&strings, key);
            tree.push(
                None,
                Semantic::new(NodeId::new(VIEW, "notice", None), Role::Text)
                    .label(text.message_id(), text.as_str()),
            );
        }
        tree.into_nodes()
    }

    // ----- rendering ---------------------------------------------------------

    fn render_toolbar(
        &self,
        sidebar_shown: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = *colors(window, cx);
        let registry = self.driver.registry();
        let nav = self.driver.nav();
        let layout = nav.layout(&registry_env(registry));
        let current = nav.nav().current();
        let strings = strings(cx);
        let items = payload::<Screen>(registry, Kind::Screen, &current.screen).and_then(|screen| {
            in_scope(registry, Kind::Screen, &current.screen, || {
                (screen.toolbar)(cx)
            })
        });
        let (title, subtitle, controls) = match items {
            Some(items) => (
                Some(items.title),
                items.subtitle,
                items.controls.into_iter().take(3).collect::<Vec<_>>(),
            ),
            None => (None, None, Vec::new()),
        };
        let inspector_label = Text::key(&strings, keys::workspace::INSPECTOR_TOGGLE);
        let back_label = Text::key(&strings, keys::workspace::BACK);
        let inspector_on = nav.nav().inspector;
        let this = cx.weak_entity();
        let toolbar_node = UiNode::new(VIEW, "toolbar", None);
        div()
            .id("workspace-toolbar")
            .flex()
            .flex_row()
            .items_center()
            .h(TOOLBAR_HEIGHT)
            .w_full()
            .px_4()
            .when(!sidebar_shown, |toolbar| toolbar.pl(WINDOW_CONTROLS_INSET))
            .gap_3()
            .border_b(SEPARATOR)
            .border_color(palette.separator)
            .when(layout.back_button, |toolbar| {
                let this = this.clone();
                toolbar.child(
                    Button::icon_only(toolbar_node.child("back"), Icon::Back, back_label).on_click(
                        move |window, cx| {
                            let _ = this.update(cx, |this, cx| {
                                this.command(WorkspaceCommand::PopToBase, window, cx);
                            });
                        },
                    ),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .when_some(title, |column, title| {
                        column.child(
                            div()
                                .text_role(TextRole::Headline, window, cx)
                                .child(title.shared()),
                        )
                    })
                    .when_some(subtitle, |column, subtitle| {
                        column.child(
                            div()
                                .text_role(TextRole::Caption, window, cx)
                                .child(subtitle.shared()),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .children(controls)
                    .when(layout.inspector, |row| {
                        row.child(
                            Button::icon_only(
                                toolbar_node.child("inspector"),
                                Icon::Inspector,
                                inspector_label,
                            )
                            .active(inspector_on)
                            .on_click(move |window, cx| {
                                let _ = this.update(cx, |this, cx| {
                                    this.command(
                                        WorkspaceCommand::SetInspector(!inspector_on),
                                        window,
                                        cx,
                                    );
                                });
                            }),
                        )
                    }),
            )
            .into_any_element()
    }

    fn render_content(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let palette = *colors(window, cx);
        let registry = self.driver.registry();
        let layout = self.driver.nav().layout(&registry_env(registry));
        let strings = strings(cx);
        let screen = self.screens.last().and_then(|built| built.view.clone());
        // Screens that hold grouped lists sit on the grouped page background,
        // so that the cards (`control_background`) show against it.
        let page = if layout.reading_column {
            palette.under_page_background
        } else {
            palette.window_background
        };
        let missing = Text::key(&strings, keys::workspace::SCREEN_MISSING);
        let body = match screen {
            Some(view) => view.into_any_element(),
            None => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_role(TextRole::Secondary, window, cx)
                .child(missing.shared())
                .into_any_element(),
        };
        div()
            .id("workspace-content")
            .key_context(actions::CONTENT)
            .relative()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .bg(page)
            .child(if layout.reading_column {
                div()
                    .size_full()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .h_full()
                            .w_full()
                            .max_w(READING_COLUMN_WIDTH)
                            .child(body),
                    )
                    .into_any_element()
            } else {
                div().size_full().child(body).into_any_element()
            })
            .when(layout.floating_bar, |content| {
                content.child(
                    div()
                        .id("workspace-bar")
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .right_0()
                        .flex()
                        .justify_center(),
                )
            })
            .into_any_element()
    }

    fn render_inspector(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let palette = *colors(window, cx);
        let strings = strings(cx);
        let tabs = self
            .inspector_tabs()
            .into_iter()
            .filter_map(|tab| {
                let label =
                    payload::<InspectorTab>(self.driver.registry(), Kind::InspectorTab, &tab)
                        .map(|payload| Text::key(&strings, payload.label))?;
                Some((tab, label))
            })
            .collect::<Vec<_>>();
        let selected = self.selected_tab();
        let index = selected
            .as_ref()
            .and_then(|selected| tabs.iter().position(|(tab, _)| tab == selected))
            .unwrap_or(0);
        // The slider sits on the first drawn selection and moves on a
        // change (`SEGMENT`; under reduced motion it jumps and fades in).
        let now = Instant::now();
        match self.inspector_drawn.replace(index) {
            None => self.inspector_slider = Transition::new(index as f32),
            Some(drawn) if drawn != index => {
                Segmented::select(&mut self.inspector_slider, index, now, cx);
            }
            Some(_) => {}
        }
        let slider = self.inspector_slider.tick(now, window);
        let page = self.inspector.as_ref().map(|(_, view)| view.clone());
        let this = cx.weak_entity();
        let bar = UiNode::new(VIEW, "inspector", None).child("tabs");
        let names = tabs.iter().map(|(tab, _)| tab.clone()).collect::<Vec<_>>();
        let items = tabs
            .into_iter()
            .map(|(tab, label)| (bar.item(&tab), label))
            .collect::<Vec<_>>();
        div()
            .id("workspace-inspector")
            .key_context(actions::INSPECTOR)
            .flex()
            .flex_col()
            .w(INSPECTOR_WIDTH)
            .h_full()
            .border_l(SEPARATOR)
            .border_color(palette.separator)
            // The sidebar material: GPUI cannot blur behind an element
            // (docs/design/ui.md「与设计的差异」).
            .bg(palette.under_page_background)
            .child(
                div()
                    .id("workspace-inspector-tabs")
                    .p(INSPECTOR_PADDING)
                    .child(Segmented::new(bar, items, index).slider(slider).on_select(
                        move |index, window, cx| {
                            let Some(tab) = names.get(index).cloned() else {
                                return;
                            };
                            let _ = this.update(cx, |this, cx| {
                                this.command(WorkspaceCommand::SelectInspectorTab(tab), window, cx);
                            });
                        },
                    )),
            )
            .child(div().flex_1().min_h_0().children(page))
            .into_any_element()
    }

    fn render_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (id, view) = self.panel.as_ref()?;
        let palette = *colors(window, cx);
        let value = self.panel_slide.tick(Instant::now(), window);
        let (offset, opacity) = match PANEL_OPEN.effective(cx) {
            Effective::Move(_) => ((1.0 - value) * INSPECTOR_WIDTH, 1.0),
            Effective::Fade(_) => (px(0.0), value),
        };
        let _ = id;
        Some(
            div()
                .id("workspace-panel")
                .key_context(actions::PANEL)
                .occlude()
                .absolute()
                .top(TOOLBAR_HEIGHT)
                .bottom_0()
                .right_0()
                .w(INSPECTOR_WIDTH)
                .left_auto()
                .ml_auto()
                .rounded(PANEL_RADIUS)
                .shadow_lg()
                .bg(palette.control_background)
                .opacity(opacity)
                .child(
                    div()
                        .size_full()
                        .relative()
                        .left(offset)
                        .child(view.clone()),
                )
                .into_any_element(),
        )
    }

    fn render_overlay(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (_, view) = self.overlay.as_ref()?;
        let palette = *colors(window, cx);
        Some(
            div()
                .id("workspace-overlay")
                .key_context(actions::OVERLAY)
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .occlude()
                        .rounded(PANEL_RADIUS)
                        .shadow_lg()
                        .bg(palette.control_background)
                        .child(view.clone()),
                )
                .into_any_element(),
        )
    }

    fn render_notice(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let key = self.notice?;
        let palette = *colors(window, cx);
        let text = Text::key(&strings(cx), key);
        Some(
            div()
                .id("workspace-notice")
                .absolute()
                .top_4()
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .shadow_sm()
                        .bg(palette.control_background)
                        .text_role(TextRole::Secondary, window, cx)
                        .child(text.shared()),
                )
                .into_any_element(),
        )
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !lectern_ui::is_ready(cx) {
            // The first frame is `app`'s neutral skeleton; nothing of `ui`
            // mounts before `ready`.
            return div().size_full().into_any_element();
        }
        self.sync_views(window, cx);
        let registry = self.driver.registry();
        let nav = self.driver.nav();
        let layout = nav.layout(&registry_env(registry));
        let project = nav.nav().project().is_some();
        let sidebar_shown = project && layout.sidebar == Visibility::Shown;
        let inspector_shown = self.inspector_shown();
        let palette = *colors(window, cx);
        if window.focused(cx).is_none() {
            // A popped screen took the focus with it: the root takes it
            // back so Esc, ⌘[ and ⌘O keep working.
            window.focus(&self.focus, cx);
        }

        // The root's context, `project` added while a project is open. GPUI's
        // `A > B` wants `B` on the deepest context of the path, which the
        // regions' own contexts occupy, so this crate's bindings use
        // `Workspace` alone and the state machine ignores the inspector
        // commands without an inspector (「与设计的差异」).
        let mut context = KeyContext::new_with_defaults();
        context.add(actions::WORKSPACE);
        if project {
            context.add(actions::PROJECT);
        }

        let mut root = div()
            .id("workspace")
            .key_context(context)
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .on_action(cx.listener(|this, _: &OpenFolder, window, cx| {
                this.command(WorkspaceCommand::PromptOpenProject, window, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseWindow, window, cx| {
                this.command(WorkspaceCommand::CloseWindow, window, cx);
            }))
            .on_action(cx.listener(|this, _: &Back, window, cx| {
                this.command(WorkspaceCommand::PopToBase, window, cx);
            }))
            .on_action(cx.listener(|this, _: &Escape, window, cx| {
                this.command(WorkspaceCommand::Escape, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleInspector, window, cx| {
                let shown = this.driver.nav().nav().inspector;
                this.command(WorkspaceCommand::SetInspector(!shown), window, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowConversation, window, cx| {
                this.command(
                    WorkspaceCommand::SelectInspectorTab(Name::Borrowed(actions::CONVERSATION_TAB)),
                    window,
                    cx,
                );
            }))
            .on_action(cx.listener(|this, action: &Command, window, cx| {
                this.notice = None;
                this.run(Item::Proxy(Name::Owned(action.name.clone())), window, cx);
            }));

        let inspector = inspector_shown.then(|| self.render_inspector(window, cx));
        if !inspector_shown {
            // Shown again later, the slider starts on the then-selected tab.
            self.inspector_drawn = None;
        }
        if project {
            root = root.child(
                div()
                    .flex()
                    .flex_row()
                    .size_full()
                    .when(sidebar_shown, |row| {
                        let panel = self.sidebar.as_ref().map(|(_, view)| view.clone());
                        row.child(
                            div()
                                .id("workspace-sidebar")
                                .key_context(actions::SIDEBAR)
                                .w(SIDEBAR_WIDTH)
                                .h_full()
                                // The top row, as tall as the toolbar, holds
                                // the window controls.
                                .pt(TOOLBAR_HEIGHT)
                                // The sidebar material, opaque: the window's
                                // `Blurred` background does not blur
                                // (docs/design/ui.md「与设计的差异」).
                                .bg(palette.under_page_background)
                                .border_r(SEPARATOR)
                                .border_color(palette.separator)
                                .children(panel),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .bg(palette.window_background)
                            .child(self.render_toolbar(sidebar_shown, window, cx))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .flex_1()
                                    .min_h_0()
                                    .child(self.render_content(window, cx))
                                    .children(inspector),
                            ),
                    ),
            );
        } else {
            root = root.child(self.render_content(window, cx));
        }
        root.children(self.render_panel(window, cx))
            .children(self.render_overlay(window, cx))
            .children(self.render_notice(window, cx))
            .into_any_element()
    }
}

/// The state machine's view of the registry, for `layout`.
fn registry_env(registry: &Registry) -> RegistryEnv<'_> {
    RegistryEnv(registry)
}

struct RegistryEnv<'a>(&'a Registry);

impl crate::nav::Env for RegistryEnv<'_> {
    fn screen_layout(&self, id: &str) -> Option<lectern_services::workspace::ScreenLayout> {
        self.0.screen_layout(id)
    }
    fn has_sidebar_panel(&self, id: &str) -> bool {
        self.0.has(Kind::SidebarPanel, id)
    }
    fn has_panel(&self, id: &str) -> bool {
        self.0.has(Kind::Panel, id)
    }
    fn has_overlay(&self, id: &str) -> bool {
        self.0.has(Kind::Overlay, id)
    }
    fn guards(&self) -> Vec<crate::nav::GuardId> {
        Vec::new()
    }
    fn open_project(&self, _: &std::path::Path) -> Option<lectern_services::project::OpenRequest> {
        None
    }
    fn close_project(&self, _: &lectern_services::project::ProjectKey) {}
    fn set_active_worktree(
        &self,
        _: &lectern_services::project::ProjectKey,
        _: &lectern_services::project::WorktreeKey,
    ) {
    }
}

/// Builds a screen's view through its factory, in the contributor's scope.
fn build_screen(
    registry: &Registry,
    route: &Route,
    window: &mut Window,
    cx: &mut App,
) -> Option<AnyView> {
    let screen = payload::<Screen>(registry, Kind::Screen, &route.screen)?;
    in_scope(registry, Kind::Screen, &route.screen, || {
        (screen.build)(&route.target, window, cx)
    })
}

/// Calls a contributor's closure (kernel design「撤销」): in its activation's
/// admission and log scope, under `contain`; a panic fails the activation
/// while the admission is still held. None when the activation is not live.
fn in_scope<R>(registry: &Registry, kind: Kind, id: &str, f: impl FnOnce() -> R) -> Option<R> {
    let (token, ctx) = registry.token_of(kind, id)?;
    let admission = token.enter()?;
    let _scope = lectern_logging::scope(ctx);
    let result = match contain(f) {
        Ok(result) => Some(result),
        Err(message) => {
            error!("contribution closure panicked"; kind = kind, id = id_text(&Name::Owned(id.to_owned())), activation = token.id());
            token.fail(PluginError::Panicked(message));
            None
        }
    };
    drop(admission);
    result
}

/// Checks each frame whether every transition has settled, then ends the
/// recording (`RecordStop::AnimationsSettled`).
fn settle_check(this: WeakEntity<Workspace>, window: &mut Window) {
    window.on_next_frame(move |window, cx| {
        if motion::active_transitions(cx) == 0 {
            let _ = this.update(cx, |workspace, cx| workspace.stop_recording(cx));
        } else {
            settle_check(this, window);
        }
    });
}
