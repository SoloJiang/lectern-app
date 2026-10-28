//! The two plugins (docs/design/welcome.md「接口」「依赖」): "welcome" keeps the
//! recent projects and registers the screen; "welcome-discovery" computes the
//! discovered folders. They are two because the kernel has no optional
//! dependency: without a session source the screen still exists, only the
//! discovered list stays empty.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{AppContext, SharedString};
use lectern_executor::Executor;
use lectern_kernel::{
    Activation, Active, Context, Manifest, Name, Plugin, PluginError, PluginHandle, ServiceRef,
    service,
};
use lectern_services::project::{ProjectEvent, Projects};
use lectern_services::session::{SessionSource, SessionsChanged};
use lectern_services::workspace::{
    Contribution, ContributionKind, Contributions, ScreenLayout, Visibility, screens,
};
use lectern_ui::Text;
use lectern_ui::contrib::{Screen, ToolbarItems};

use crate::actions;
use crate::model::WelcomeModel;
use crate::prefs::{LanguagePrefs, PlatformPrefs};
use crate::view::{ViewDeps, WelcomeView};

/// The ID of the plugin with the screen and the recent projects.
pub const WELCOME_ID: &str = "welcome";
/// The ID of the plugin with the discovered folders.
pub const DISCOVERY_ID: &str = "welcome-discovery";

/// What `app` hands over.
pub struct WelcomeDeps {
    /// Created by `app` and passed in.
    pub executor: Executor,
    /// Where `recent.json` lives: `app` passes `platform::dirs::app_data()`
    /// joined with `welcome/`. It is created by the first write, after the
    /// first frame.
    pub data_dir: PathBuf,
    /// The app's version as "major.minor", for the subtitle under the name;
    /// `app` cuts it from the package version. With `None` the subtitle has no
    /// version and the rest of the layout stays.
    pub version: Option<SharedString>,
}

/// The two plugins, sharing one model. `app` puts them in the `PluginSet`
/// one by one. Builds an empty model; no IO.
pub fn plugins(deps: WelcomeDeps) -> [(Name, WelcomePlugin); 2] {
    let model = WelcomeModel::new(deps.executor, deps.data_dir);
    from_model(model, Arc::new(PlatformPrefs), home_dir(), deps.version)
}

pub(crate) fn from_model(
    model: Arc<WelcomeModel>,
    prefs: Arc<dyn LanguagePrefs>,
    home: Option<PathBuf>,
    version: Option<SharedString>,
) -> [(Name, WelcomePlugin); 2] {
    [
        (
            Name::Borrowed(WELCOME_ID),
            WelcomePlugin {
                role: Role::Screen,
                model: model.clone(),
                prefs: prefs.clone(),
                home: home.clone(),
                version: version.clone(),
            },
        ),
        (
            Name::Borrowed(DISCOVERY_ID),
            WelcomePlugin {
                role: Role::Discovery,
                model,
                prefs,
                home,
                version,
            },
        ),
    ]
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    Screen,
    Discovery,
}

pub struct WelcomePlugin {
    role: Role,
    model: Arc<WelcomeModel>,
    /// The language preference the screen's view reads and writes.
    prefs: Arc<dyn LanguagePrefs>,
    /// The home directory, shown as `~` in the paths.
    home: Option<PathBuf>,
    /// The "major.minor" of the subtitle.
    version: Option<SharedString>,
}

impl WelcomePlugin {
    /// The model both plugins share.
    pub fn model(&self) -> Arc<WelcomeModel> {
        self.model.clone()
    }
}

/// The layout of the screen: no sidebar, back button, reading column,
/// inspector or floating bar.
pub(crate) const LAYOUT: ScreenLayout = ScreenLayout {
    sidebar: Visibility::Hidden,
    back_button: false,
    reading_column: false,
    inspector: false,
    floating_bar: false,
};

/// The home directory the paths are shortened with; an environment read,
/// not file IO.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

impl Plugin for WelcomePlugin {
    fn manifest(&self) -> Manifest {
        let requires = match self.role {
            Role::Screen => vec![service::<dyn Contributions>(), service::<dyn Projects>()],
            Role::Discovery => vec![service::<dyn Projects>(), service::<dyn SessionSource>()],
        };
        Manifest {
            provides: Vec::new(),
            requires,
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    /// Takes the services and subscribes; no IO, no task.
    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        match self.role {
            Role::Screen => self.activate_screen(ctx),
            Role::Discovery => self.activate_discovery(ctx),
        }
    }
}

impl WelcomePlugin {
    fn activate_screen(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        let contributions = ctx.service::<dyn Contributions>();
        let projects = ctx.service::<dyn Projects>();
        let screen = self.screen(contributions.clone());
        let registry = contributions
            .get()
            .ok_or(PluginError::DependencyUnavailable)?;
        let registration = registry.register(
            Contribution {
                kind: ContributionKind::Screen(LAYOUT),
                id: Name::Borrowed(screens::WELCOME),
                order: 0,
                payload: screen.into_payload(),
            },
            None,
        );
        ctx.own_staged(registration);
        let bindings = registry.register(
            Contribution {
                kind: ContributionKind::KeyBindings,
                id: Name::Borrowed(screens::WELCOME),
                order: 0,
                payload: actions::key_bindings().into_payload(),
            },
            None,
        );
        ctx.own_staged(bindings);
        let model = self.model.clone();
        ctx.on::<ProjectEvent>(move |event, handle| model.welcome_project_event(event, handle));
        Ok(Box::new(ScreenActive {
            model: self.model.clone(),
            projects: Some(projects),
        }))
    }

    /// The screen's payload: a factory that builds the view each time
    /// `workspace` needs the screen, and an empty toolbar (the layout has
    /// none). The factory runs on the main thread.
    fn screen(&self, contributions: ServiceRef<dyn Contributions>) -> Screen {
        let model = self.model.clone();
        let prefs = self.prefs.clone();
        let home = self.home.clone();
        let version = self.version.clone();
        Screen {
            build: Arc::new(move |_, window, cx| {
                let deps = ViewDeps {
                    model: model.clone(),
                    contributions: contributions.clone(),
                    prefs: prefs.clone(),
                    home: home.clone(),
                    version: version.clone(),
                };
                let view = cx.new(|cx| WelcomeView::new(deps, cx));
                let focus = view.read(cx).focus_handle().clone();
                window.focus(&focus, cx);
                view.into()
            }),
            toolbar: Arc::new(|_| ToolbarItems {
                title: Text::verbatim(String::new()),
                subtitle: None,
                controls: Vec::new(),
            }),
        }
    }

    fn activate_discovery(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        let projects = ctx.service::<dyn Projects>();
        let sources = ctx.service_set::<dyn SessionSource>();
        let model = self.model.clone();
        ctx.on::<SessionsChanged>(move |_, _| model.request_rebuild());
        let model = self.model.clone();
        ctx.on::<ProjectEvent>(move |event, _| {
            if !matches!(event, ProjectEvent::Changed { .. }) {
                model.request_rebuild();
            }
        });
        Ok(Box::new(DiscoveryActive {
            model: self.model.clone(),
            services: Some((projects, sources)),
        }))
    }
}

struct ScreenActive {
    model: Arc<WelcomeModel>,
    projects: Option<ServiceRef<dyn Projects>>,
}

impl Active for ScreenActive {
    /// Submits the task that reads the file.
    fn started(&mut self, handle: PluginHandle) {
        if let Some(projects) = self.projects.take() {
            self.model.welcome_started(handle, projects);
        }
    }
}

impl Drop for ScreenActive {
    fn drop(&mut self) {
        self.model.welcome_stopped();
    }
}

struct DiscoveryActive {
    model: Arc<WelcomeModel>,
    services: Option<(
        ServiceRef<dyn Projects>,
        lectern_kernel::ServiceSet<dyn SessionSource>,
    )>,
}

impl Active for DiscoveryActive {
    /// The list is pending; submits the first rebuild.
    fn started(&mut self, handle: PluginHandle) {
        if let Some((projects, sources)) = self.services.take() {
            self.model.discovery_started(handle, projects, sources);
        }
    }
}

impl Drop for DiscoveryActive {
    fn drop(&mut self) {
        self.model.discovery_stopped();
    }
}
