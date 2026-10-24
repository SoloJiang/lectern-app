//! The navigation state machine (docs/design/workspace.md「数据结构」
//! 「导航状态机」) and the leave-guard sequencing around it.
//!
//! [`Nav`] is the state of one window; [`Navigator`] applies commands,
//! project events and guard answers to it. Everything here is a pure
//! function of its inputs and the [`Env`] it is given: what screens and
//! panels are registered, which guards exist, and the `Projects` calls
//! that a command ends in. The results come back as [`Effect`]s; the driver
//! runs them on the main thread, the tests read them.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use lectern_kernel::Name;
use lectern_logging::{debug, info, warn};
use lectern_services::project::{OpenRequest, ProjectEvent, ProjectKey, WorktreeKey};
use lectern_services::workspace::{
    Layer, LeaveScope, Route, ScreenLayout, Target, Visibility, WindowId, WorkspaceCommand,
    WorkspaceEvent, screens,
};

use crate::diag::{command_name, id_text, open_error_kind};
use crate::effect::{Effect, Notice};
use crate::state::WindowState;

/// The base screen: Welcome, or the open project's home.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Base {
    Welcome,
    Project(ProjectKey),
}

/// The navigation state of one window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Nav {
    pub base: Base,
    /// Screens pushed above the base; the base screen shows when empty.
    pub stack: Vec<Route>,
    /// None: the project sidebar.
    pub sidebar_panel: Option<Name>,
    pub inspector: bool,
    pub inspector_tab: Option<Name>,
    pub panel: Option<Name>,
    pub overlay: Option<Name>,
}

impl Nav {
    /// Welcome, nothing open.
    pub fn new() -> Nav {
        Nav {
            base: Base::Welcome,
            stack: Vec::new(),
            sidebar_panel: None,
            inspector: false,
            inspector_tab: None,
            panel: None,
            overlay: None,
        }
    }

    /// The route of the base screen.
    pub fn base_route(&self) -> Route {
        match &self.base {
            Base::Welcome => Route {
                screen: Name::Borrowed(screens::WELCOME),
                target: Target::None,
            },
            Base::Project(key) => Route {
                screen: Name::Borrowed(screens::PROJECT_HOME),
                target: Target::Project(key.clone()),
            },
        }
    }

    /// The current screen: the top of the stack, or the base.
    pub fn current(&self) -> Route {
        self.stack
            .last()
            .cloned()
            .unwrap_or_else(|| self.base_route())
    }

    /// The stack depth; 0 on the base screen.
    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// The open project, if any.
    pub fn project(&self) -> Option<&ProjectKey> {
        match &self.base {
            Base::Welcome => None,
            Base::Project(key) => Some(key),
        }
    }
}

impl Default for Nav {
    fn default() -> Self {
        Nav::new()
    }
}

/// The layout of a screen that is not registered: every region off.
pub const MISSING_LAYOUT: ScreenLayout = ScreenLayout {
    sidebar: Visibility::Hidden,
    back_button: false,
    reading_column: false,
    inspector: false,
    floating_bar: false,
};

/// What the state machine reads and calls: the registry's tables and the
/// `Projects` service. The driver implements it over both; the tests fake
/// it.
pub trait Env {
    /// The layout of a registered screen.
    fn screen_layout(&self, id: &str) -> Option<ScreenLayout>;
    fn has_sidebar_panel(&self, id: &str) -> bool;
    fn has_panel(&self, id: &str) -> bool;
    fn has_overlay(&self, id: &str) -> bool;
    /// The registered leave guards, in registration order.
    fn guards(&self) -> Vec<GuardId>;
    /// `Projects::open`; None when the service is not available (the
    /// command is then ignored).
    fn open_project(&self, path: &Path) -> Option<OpenRequest>;
    fn close_project(&self, project: &ProjectKey);
    fn set_active_worktree(&self, project: &ProjectKey, worktree: &WorktreeKey);
}

/// A registered leave guard, by its registration serial.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GuardId(pub u64);

/// One round of asking the guards; answers for another round are stale.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AskId(pub u64);

/// A command or event that arrived while the guards were being asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    Command(WorkspaceCommand),
    Project(ProjectEvent),
    /// `WorkspaceHost::ask_leave`: a round whose only outcome is the answer
    /// to the report numbered `report`.
    AskLeave {
        scope: LeaveScope,
        report: u64,
    },
}

/// `WorkspaceHost::ask_leave`'s `done`.
pub type Report = Box<dyn FnOnce(bool) + Send>;

/// What runs once every guard of a round allowed it.
#[derive(Debug)]
enum Then {
    OpenProject(PathBuf),
    CloseProject,
    SetActiveWorktree(WorktreeKey),
    Pop,
    PopToBase,
    CloseWindow,
    SwitchProject {
        key: ProjectKey,
        path: Option<PathBuf>,
    },
    /// Answer the report (`ask_leave`); nothing else changes.
    Report(u64),
}

impl Then {
    fn name(&self) -> &'static str {
        match self {
            Then::OpenProject(_) => "open_project",
            Then::CloseProject => "close_project",
            Then::SetActiveWorktree(_) => "set_active_worktree",
            Then::Pop => "pop",
            Then::PopToBase => "pop_to_base",
            Then::CloseWindow => "close_window",
            Then::SwitchProject { .. } => "switch_project",
            Then::Report(_) => "ask_leave",
        }
    }
}

struct Asking {
    ask: AskId,
    scope: LeaveScope,
    guards: Vec<GuardId>,
    /// The index of the next guard to ask.
    next: usize,
    then: Then,
}

/// The most in-flight opens remembered for `WindowState::project`.
const OPENS_KEPT: usize = 8;

/// The state machine of one window with its guard rounds and the inputs
/// queued behind them.
pub struct Navigator {
    window: WindowId,
    nav: Nav,
    /// The paths of in-flight `Projects::open` calls, by request.
    opens: Vec<(OpenRequest, PathBuf)>,
    /// The path of the open project, for `WindowState`.
    project_path: Option<PathBuf>,
    asking: Option<Asking>,
    /// Inputs that arrived during a guard round, run in order after it.
    queued: VecDeque<Input>,
    next_ask: u64,
    /// The `done`s of `ask_leave` rounds not yet answered, by report number.
    reports: HashMap<u64, Report>,
    next_report: u64,
}

impl Navigator {
    pub fn new(window: WindowId) -> Navigator {
        Navigator {
            window,
            nav: Nav::new(),
            opens: Vec::new(),
            project_path: None,
            asking: None,
            queued: VecDeque::new(),
            next_ask: 1,
            reports: HashMap::new(),
            next_report: 1,
        }
    }

    pub fn window(&self) -> WindowId {
        self.window
    }

    pub fn nav(&self) -> &Nav {
        &self.nav
    }

    /// Whether a guard round is in progress; inputs queue up until it ends.
    pub fn asking(&self) -> bool {
        self.asking.is_some()
    }

    /// The inputs waiting behind the guard round.
    pub fn queued(&self) -> usize {
        self.queued.len()
    }

    /// The window state as `app` would store it now.
    pub fn state(&self) -> WindowState {
        WindowState {
            version: WindowState::VERSION,
            project: self.project_path.clone(),
            inspector: self.nav.inspector,
        }
    }

    /// `WorkspaceHost::restore`'s inspector flag: set directly, before any
    /// screen with an inspector shows, without an event.
    pub fn restore_inspector(&mut self, shown: bool) {
        self.nav.inspector = shown;
    }

    /// The layout of the current screen; [`MISSING_LAYOUT`] when it is not
    /// registered.
    pub fn layout(&self, env: &dyn Env) -> ScreenLayout {
        env.screen_layout(&self.nav.current().screen)
            .unwrap_or(MISSING_LAYOUT)
    }

    /// A command from `Contributions::dispatch` or the window's actions.
    pub fn dispatch(&mut self, command: WorkspaceCommand, env: &dyn Env, out: &mut Vec<Effect>) {
        self.input(Input::Command(command), env, out);
    }

    /// A `ProjectEvent` from the `project` plugin.
    pub fn project_event(&mut self, event: &ProjectEvent, env: &dyn Env, out: &mut Vec<Effect>) {
        if matches!(event, ProjectEvent::Changed { .. }) {
            return;
        }
        self.input(Input::Project(event.clone()), env, out);
    }

    /// `WorkspaceHost::ask_leave`: asks the guards for `scope` and answers
    /// `done` with the verdict; `done(true)` at once when there is no guard.
    /// Queues behind a round in progress like any other input.
    pub fn ask_leave(
        &mut self,
        scope: LeaveScope,
        done: Report,
        env: &dyn Env,
        out: &mut Vec<Effect>,
    ) {
        let report = self.next_report;
        self.next_report += 1;
        self.reports.insert(report, done);
        self.input(Input::AskLeave { scope, report }, env, out);
    }

    fn input(&mut self, input: Input, env: &dyn Env, out: &mut Vec<Effect>) {
        if self.asking.is_some() {
            self.queued.push_back(input);
            return;
        }
        self.run(input, env, out);
    }

    fn run(&mut self, input: Input, env: &dyn Env, out: &mut Vec<Effect>) {
        match input {
            Input::Command(command) => self.command(command, env, out),
            Input::Project(event) => self.project(&event, env, out),
            Input::AskLeave { scope, report } => self.ask(scope, Then::Report(report), env, out),
        }
    }

    /// Runs the inputs queued behind a guard round, until the next round
    /// starts or the queue is empty.
    fn drain(&mut self, env: &dyn Env, out: &mut Vec<Effect>) {
        while self.asking.is_none() {
            let Some(input) = self.queued.pop_front() else {
                return;
            };
            self.run(input, env, out);
        }
    }

    /// A guard's answer. Answers for a round that ended are ignored.
    pub fn guard_answered(
        &mut self,
        ask: AskId,
        allowed: bool,
        env: &dyn Env,
        out: &mut Vec<Effect>,
    ) {
        let Some(asking) = self.asking.as_mut() else {
            debug!("stale guard answer"; window = self.window.0, ask = ask.0);
            return;
        };
        if asking.ask != ask {
            debug!("stale guard answer"; window = self.window.0, ask = ask.0);
            return;
        }
        if !allowed {
            let asking = self.asking.take().expect("a round is in progress");
            debug!("command"; name = asking.then.name(), guarded = true, result = "refused");
            match asking.then {
                Then::SwitchProject { .. } => {
                    warn!("project opened while a view refused to leave"; window = self.window.0);
                    out.push(Effect::Notice(
                        Notice::ProjectOpenedWhileAViewRefusedToLeave,
                    ));
                }
                Then::Report(report) => self.report(report, false),
                _ => {}
            }
            self.drain(env, out);
            return;
        }
        if asking.next < asking.guards.len() {
            let guard = asking.guards[asking.next];
            asking.next += 1;
            out.push(Effect::Ask {
                ask: asking.ask,
                guard,
                scope: asking.scope.clone(),
            });
            return;
        }
        let asking = self.asking.take().expect("a round is in progress");
        debug!("command"; name = asking.then.name(), guarded = true, result = "done");
        self.finish(asking.then, env, out);
        self.drain(env, out);
    }

    /// Starts a guard round for `scope`; with no guards, `then` runs at once.
    fn ask(&mut self, scope: LeaveScope, then: Then, env: &dyn Env, out: &mut Vec<Effect>) {
        let guards = env.guards();
        if guards.is_empty() {
            debug!("command"; name = then.name(), guarded = false, result = "done");
            self.finish(then, env, out);
            return;
        }
        let ask = AskId(self.next_ask);
        self.next_ask += 1;
        out.push(Effect::Ask {
            ask,
            guard: guards[0],
            scope: scope.clone(),
        });
        self.asking = Some(Asking {
            ask,
            scope,
            guards,
            next: 1,
            then,
        });
    }

    fn finish(&mut self, then: Then, env: &dyn Env, out: &mut Vec<Effect>) {
        match then {
            Then::OpenProject(path) => self.open(path, env),
            Then::CloseProject => {
                if let Some(key) = self.nav.project() {
                    env.close_project(key);
                }
            }
            Then::SetActiveWorktree(worktree) => {
                if let Some(key) = self.nav.project() {
                    env.set_active_worktree(key, &worktree);
                }
            }
            Then::Pop => self.pop(env, out),
            Then::PopToBase => {
                self.pop(env, out);
                if !self.nav.stack.is_empty() {
                    self.ask(LeaveScope::View, Then::PopToBase, env, out);
                }
            }
            Then::CloseWindow => {
                out.push(Effect::CloseWindow);
                out.push(Effect::Event(WorkspaceEvent::WindowClosed {
                    window: self.window,
                }));
            }
            Then::SwitchProject { key, path } => self.switch_project(key, path, env, out),
            Then::Report(report) => self.report(report, true),
        }
    }

    /// Answers an `ask_leave` round. The `done` is `app`'s, not a
    /// contributor's, so it runs as it is.
    fn report(&mut self, report: u64, allowed: bool) {
        if let Some(done) = self.reports.remove(&report) {
            done(allowed);
        }
    }

    fn ignored(&self, name: &'static str) {
        debug!("command"; name = name, guarded = false, result = "ignored");
    }

    fn done(&self, name: &'static str) {
        debug!("command"; name = name, guarded = false, result = "done");
    }

    fn command(&mut self, command: WorkspaceCommand, env: &dyn Env, out: &mut Vec<Effect>) {
        let name = command_name(&command);
        match command {
            WorkspaceCommand::OpenProject(path) => match self.nav.project() {
                None => {
                    self.done(name);
                    self.open(path, env);
                }
                Some(key) => {
                    let scope = LeaveScope::Project(key.clone());
                    self.ask(scope, Then::OpenProject(path), env, out);
                }
            },
            WorkspaceCommand::PromptOpenProject => {
                self.done(name);
                out.push(Effect::PromptOpenProject);
            }
            WorkspaceCommand::CloseProject => match self.nav.project() {
                None => self.ignored(name),
                Some(key) => {
                    let scope = LeaveScope::Project(key.clone());
                    self.ask(scope, Then::CloseProject, env, out);
                }
            },
            WorkspaceCommand::SetActiveWorktree(worktree) => match self.nav.project() {
                None => self.ignored(name),
                Some(key) => {
                    let scope = LeaveScope::Worktree(key.clone());
                    self.ask(scope, Then::SetActiveWorktree(worktree), env, out);
                }
            },
            WorkspaceCommand::Push(route) => {
                if self.nav.project().is_none() {
                    self.ignored(name);
                    return;
                }
                if env.screen_layout(&route.screen).is_none() {
                    warn!("screen not registered"; window = self.window.0, id = id_text(&route.screen));
                    self.ignored(name);
                    return;
                }
                self.done(name);
                let from = self.nav.current();
                self.close_panel(out);
                self.close_overlay(out);
                self.nav.stack.push(route);
                self.screen_changed(from, env, out);
            }
            WorkspaceCommand::Pop => {
                if self.nav.project().is_none() || self.nav.stack.is_empty() {
                    self.ignored(name);
                    return;
                }
                self.ask(LeaveScope::View, Then::Pop, env, out);
            }
            WorkspaceCommand::PopToBase => {
                if self.nav.project().is_none() || self.nav.stack.is_empty() {
                    self.ignored(name);
                    return;
                }
                self.ask(LeaveScope::View, Then::PopToBase, env, out);
            }
            WorkspaceCommand::ShowSidebarPanel(panel) => {
                if let Some(id) = &panel
                    && !env.has_sidebar_panel(id)
                {
                    self.ignored(name);
                    return;
                }
                self.done(name);
                self.set_sidebar_panel(panel, out);
            }
            WorkspaceCommand::SetInspector(shown) => {
                if !self.layout(env).inspector {
                    self.ignored(name);
                    return;
                }
                self.done(name);
                self.set_inspector(shown, out);
            }
            WorkspaceCommand::SelectInspectorTab(tab) => {
                if !self.layout(env).inspector {
                    self.ignored(name);
                    return;
                }
                self.done(name);
                self.nav.inspector_tab = Some(tab);
                self.set_inspector(true, out);
            }
            WorkspaceCommand::OpenPanel(panel) => {
                if !env.has_panel(&panel) {
                    self.ignored(name);
                    return;
                }
                self.done(name);
                if self.nav.panel.as_ref() == Some(&panel) {
                    return;
                }
                self.close_panel(out);
                self.layer_changed(Layer::Panel(panel.clone()), true, out);
                self.nav.panel = Some(panel);
            }
            WorkspaceCommand::ClosePanel => {
                self.done(name);
                self.close_panel(out);
            }
            WorkspaceCommand::OpenOverlay(overlay) => {
                if !env.has_overlay(&overlay) {
                    self.ignored(name);
                    return;
                }
                self.done(name);
                if self.nav.overlay.as_ref() == Some(&overlay) {
                    return;
                }
                self.close_overlay(out);
                self.layer_changed(Layer::Overlay(overlay.clone()), true, out);
                self.nav.overlay = Some(overlay);
            }
            WorkspaceCommand::CloseOverlay => {
                self.done(name);
                self.close_overlay(out);
            }
            WorkspaceCommand::Escape => {
                if self.nav.overlay.is_some() {
                    self.done(name);
                    self.close_overlay(out);
                } else if self.nav.panel.is_some() {
                    self.done(name);
                    self.close_panel(out);
                } else if self.nav.sidebar_panel.is_some() {
                    self.done(name);
                    self.set_sidebar_panel(None, out);
                } else if self.nav.project().is_some() && !self.nav.stack.is_empty() {
                    self.ask(LeaveScope::View, Then::Pop, env, out);
                } else {
                    self.ignored(name);
                }
            }
            WorkspaceCommand::CloseWindow => {
                self.ask(LeaveScope::Window, Then::CloseWindow, env, out);
            }
        }
    }

    fn project(&mut self, event: &ProjectEvent, env: &dyn Env, out: &mut Vec<Effect>) {
        match event {
            ProjectEvent::Opened {
                request,
                result: Ok(key),
            } => {
                let path = self.take_open(*request);
                if self.nav.project() == Some(key) {
                    // The same project again: a worktree switch or a refresh.
                    if path.is_some() && path != self.project_path {
                        self.project_path = path;
                        out.push(Effect::StateChanged(self.state()));
                    }
                    return;
                }
                let then = Then::SwitchProject {
                    key: key.clone(),
                    path,
                };
                if self.nav.stack.is_empty() {
                    self.finish(then, env, out);
                } else {
                    self.ask(LeaveScope::View, then, env, out);
                }
            }
            ProjectEvent::Opened {
                request,
                result: Err(error),
            } => {
                self.take_open(*request);
                warn!("project open failed"; window = self.window.0, error = open_error_kind(error));
            }
            ProjectEvent::Closed { project } => {
                if self.nav.project() != Some(project) {
                    return;
                }
                let from = self.nav.current();
                self.nav.base = Base::Welcome;
                self.nav.stack.clear();
                self.project_path = None;
                self.close_overlay(out);
                self.close_panel(out);
                self.set_sidebar_panel(None, out);
                self.set_inspector(false, out);
                self.screen_changed(from, env, out);
                out.push(Effect::StateChanged(self.state()));
            }
            ProjectEvent::Changed { .. } => {}
        }
    }

    /// A screen's contribution was removed: the routes showing it leave the
    /// stack without asking a guard; a missing base screen shows nothing.
    pub fn screen_removed(&mut self, id: &str, env: &dyn Env, out: &mut Vec<Effect>) {
        let from = self.nav.current();
        let before = self.nav.stack.len();
        self.nav.stack.retain(|route| route.screen != id);
        if self.nav.stack.len() != before {
            self.close_panel(out);
            self.close_overlay(out);
            self.screen_changed(from, env, out);
        } else if self.nav.stack.is_empty() && from.screen == id {
            warn!("base screen missing"; window = self.window.0, id = id_text(&from.screen));
        }
    }

    fn open(&mut self, path: PathBuf, env: &dyn Env) {
        match env.open_project(&path) {
            Some(request) => {
                if self.opens.len() >= OPENS_KEPT {
                    self.opens.remove(0);
                }
                self.opens.push((request, path));
            }
            None => {
                debug!("command"; name = "open_project", guarded = false, result = "ignored");
            }
        }
    }

    fn take_open(&mut self, request: OpenRequest) -> Option<PathBuf> {
        let index = self.opens.iter().position(|(r, _)| *r == request)?;
        Some(self.opens.remove(index).1)
    }

    fn switch_project(
        &mut self,
        key: ProjectKey,
        path: Option<PathBuf>,
        env: &dyn Env,
        out: &mut Vec<Effect>,
    ) {
        let from = self.nav.current();
        let old = std::mem::replace(&mut self.nav.base, Base::Project(key));
        self.nav.stack.clear();
        self.project_path = path;
        self.close_overlay(out);
        self.close_panel(out);
        self.set_sidebar_panel(None, out);
        self.screen_changed(from, env, out);
        out.push(Effect::StateChanged(self.state()));
        if let Base::Project(old) = old {
            // One window shows one project at a time; the old one would
            // otherwise keep its watches, tasks and snapshot.
            env.close_project(&old);
        }
    }

    fn pop(&mut self, env: &dyn Env, out: &mut Vec<Effect>) {
        let from = self.nav.current();
        if self.nav.stack.pop().is_none() {
            return;
        }
        self.close_panel(out);
        self.close_overlay(out);
        self.screen_changed(from, env, out);
    }

    fn screen_changed(&mut self, from: Route, env: &dyn Env, out: &mut Vec<Effect>) {
        let to = self.nav.current();
        info!(
            "screen changed";
            window = self.window.0,
            from = id_text(&from.screen),
            to = id_text(&to.screen),
            depth = self.nav.depth()
        );
        if self.nav.stack.is_empty() && env.screen_layout(&to.screen).is_none() {
            warn!("base screen missing"; window = self.window.0, id = id_text(&to.screen));
        }
        out.push(Effect::Event(WorkspaceEvent::ScreenChanged {
            window: self.window,
            from: Some(from),
            to,
        }));
    }

    fn layer_changed(&self, layer: Layer, open: bool, out: &mut Vec<Effect>) {
        debug!("layer changed"; window = self.window.0, layer = layer_name(&layer), open = open);
        out.push(Effect::Event(WorkspaceEvent::LayerChanged {
            window: self.window,
            layer,
            open,
        }));
    }

    fn set_sidebar_panel(&mut self, panel: Option<Name>, out: &mut Vec<Effect>) {
        if self.nav.sidebar_panel == panel {
            return;
        }
        if let Some(old) = self.nav.sidebar_panel.take() {
            self.layer_changed(Layer::SidebarPanel(old), false, out);
        }
        if let Some(new) = panel {
            self.layer_changed(Layer::SidebarPanel(new.clone()), true, out);
            self.nav.sidebar_panel = Some(new);
        }
    }

    fn set_inspector(&mut self, shown: bool, out: &mut Vec<Effect>) {
        if self.nav.inspector == shown {
            return;
        }
        self.nav.inspector = shown;
        self.layer_changed(Layer::Inspector, shown, out);
        out.push(Effect::StateChanged(self.state()));
    }

    fn close_panel(&mut self, out: &mut Vec<Effect>) {
        if let Some(panel) = self.nav.panel.take() {
            self.layer_changed(Layer::Panel(panel), false, out);
        }
    }

    fn close_overlay(&mut self, out: &mut Vec<Effect>) {
        if let Some(overlay) = self.nav.overlay.take() {
            self.layer_changed(Layer::Overlay(overlay), false, out);
        }
    }
}

fn layer_name(layer: &Layer) -> &'static str {
    match layer {
        Layer::SidebarPanel(_) => "sidebar_panel",
        Layer::Inspector => "inspector",
        Layer::Panel(_) => "panel",
        Layer::Overlay(_) => "overlay",
    }
}
