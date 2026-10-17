//! Plugin, manifest, activation state and errors.

use std::borrow::Cow;
use std::fmt;
use std::time::Duration;

use crate::handle::{Context, PluginHandle};
use crate::service::ServiceKey;
use crate::token::ActivationToken;

/// A name: `&'static str` for built-in plugins, a `String` read from a manifest
/// file for process plugins.
pub type Name = Cow<'static, str>;

/// What a plugin provides and requires, and when it activates.
///
/// The plugin ID is not part of the manifest: it is given when the plugin is
/// added to a [`PluginSet`](crate::PluginSet), so the kernel still knows which
/// plugin failed when `manifest()` panics.
#[derive(Clone, Debug, Default)]
pub struct Manifest {
    /// Every one of these must be provided when activation succeeds.
    pub provides: Vec<ServiceKey>,
    pub requires: Vec<ServiceKey>,
    /// Any one condition activates the plugin. Must not be empty; a plugin that
    /// should only be activated by hand uses [`Activation::Manual`].
    pub activation: Vec<Activation>,
}

/// An activation condition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Activation {
    /// As soon as the startup barrier is released. For plugins the review loop
    /// needs.
    AfterFirstFrame,
    /// When the open project has a file matching one of the patterns, such as
    /// `["**/*.java", "pom.xml", "build.gradle*"]`.
    Workspace(Vec<Name>),
    /// The first time the user triggers this command, such as `"terminal.open"`.
    Command(Name),
    /// Only through `Kernel::activate(id)`.
    Manual,
}

/// A plugin linked into the app.
///
/// The kernel drops plugin objects on its own thread under `catch_unwind` at
/// shutdown; their `Drop` must not panic.
pub trait Plugin: Send + Sync + 'static {
    /// In-memory only, no IO. A panic marks this plugin `Failed`; the others
    /// are checked and activated as usual.
    fn manifest(&self) -> Manifest;

    /// First phase: register only, with no side effects (no tasks, no events,
    /// no file access). Runs on the kernel thread and counts against this
    /// plugin's activation budget. Returns the state of this activation.
    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError>;
}

/// The state of one activation, owned by the kernel. It receives `started`
/// after the commit and is dropped last when the plugin is deactivated.
pub trait Active: Send + 'static {
    /// Second phase, called once on the kernel thread after the commit. Side
    /// effects start here. Events emitted and work submitted through `handle`
    /// are held until this activation is live.
    fn started(&mut self, handle: PluginHandle) {
        let _ = handle;
    }
}

impl Active for () {}

/// A guard that takes effect at commit, such as a `dyn Contributions`
/// registration. `commit` runs on the kernel thread and should only enqueue.
pub trait Staged: Send + 'static {
    /// `token` becomes live only after every `commit` and `started` succeed;
    /// work queued to other threads checks it before running.
    fn commit(&mut self, token: ActivationToken);
}

/// Why a plugin failed, or why a request could not be served.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PluginError {
    /// Reported by the plugin itself.
    Failed(String),
    /// Plugin code panicked.
    Panicked(String),
    /// `activate` returned `Ok` without providing a service its manifest
    /// declares.
    MissingService(&'static str),
    /// `provide` was called for a service the manifest does not declare.
    UndeclaredService(&'static str),
    /// The same service was provided twice in one activation.
    DuplicateService(&'static str),
    /// The dependency graph check at startup rejected this plugin.
    InvalidGraph(String),
    /// A service the plugin requires has no provider that can be activated.
    DependencyUnavailable,
    /// The plugin is disabled.
    Disabled,
    /// The kernel is shutting down.
    ShuttingDown,
    /// No plugin declares this command.
    UnknownCommand(Name),
}

impl PluginError {
    /// An error reported by plugin code.
    pub fn new(message: impl Into<String>) -> Self {
        PluginError::Failed(message.into())
    }
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PluginError::Failed(message) => f.write_str(message),
            PluginError::Panicked(message) => write!(f, "panicked: {message}"),
            PluginError::MissingService(id) => write!(f, "did not provide declared service {id}"),
            PluginError::UndeclaredService(id) => {
                write!(
                    f,
                    "provided service {id}, which the manifest does not declare"
                )
            }
            PluginError::DuplicateService(id) => write!(f, "provided service {id} twice"),
            PluginError::InvalidGraph(reason) => write!(f, "invalid dependency graph: {reason}"),
            PluginError::DependencyUnavailable => f.write_str("a required service is unavailable"),
            PluginError::Disabled => f.write_str("the plugin is disabled"),
            PluginError::ShuttingDown => f.write_str("the kernel is shutting down"),
            PluginError::UnknownCommand(name) => write!(f, "no plugin declares command {name}"),
        }
    }
}

impl std::error::Error for PluginError {}

/// Lifecycle state of a plugin, as shown by [`Kernel::status`](crate::Kernel::status).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginState {
    /// No activation condition is met.
    Inactive,
    /// An activation condition is met but these required services have no
    /// active provider.
    Waiting {
        missing: Vec<ServiceKey>,
    },
    Active,
    /// Disabled in settings.
    Disabled,
    /// Failed in this run; re-enabling the plugin retries once.
    Failed(PluginError),
}

/// Status and timings of one plugin, read by the settings page and `bench`.
#[derive(Clone, Debug)]
pub struct PluginStatus {
    pub id: Name,
    pub state: PluginState,
    /// Time spent in `manifest()` (called once): at startup, or in
    /// `PluginSet::prepare` / `PreparedSet::with` for `Kernel::start_prepared`.
    pub manifest_time: Option<Duration>,
    /// Time spent in the latest `activate`.
    pub activate_time: Option<Duration>,
    /// Time spent in the latest commit.
    pub commit_time: Option<Duration>,
    /// Time spent in the latest `started`, including held events and work run
    /// once the activation went live and the `ServiceAdded` events it caused.
    pub started_time: Option<Duration>,
}

/// A problem found by [`PluginSet::validate`](crate::PluginSet::validate).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphError {
    DuplicatePlugin(Name),
    DuplicateCommand {
        command: Name,
        plugins: Vec<Name>,
    },
    /// One stable ID used for two service types, or with two `MULTIPLE` values.
    ConflictingServiceId(&'static str),
    MissingProvider {
        plugin: Name,
        service: &'static str,
    },
    /// A service with `MULTIPLE = false` has more than one provider.
    MultipleProviders {
        service: &'static str,
        plugins: Vec<Name>,
    },
    EmptyActivation(Name),
    /// Plugins whose service dependencies form a cycle.
    Cycle(Vec<Name>),
    ManifestPanicked {
        plugin: Name,
        message: String,
    },
    /// [`PreparedSet::dependency_closure`](crate::PreparedSet::dependency_closure)
    /// was asked about a plugin that is not in the set.
    UnknownPlugin(Name),
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GraphError::DuplicatePlugin(id) => write!(f, "plugin ID {id} is used twice"),
            GraphError::DuplicateCommand { command, plugins } => {
                write!(f, "command {command} is declared by {}", plugins.join(", "))
            }
            GraphError::ConflictingServiceId(id) => {
                write!(f, "service ID {id} names more than one service")
            }
            GraphError::MissingProvider { plugin, service } => {
                write!(f, "{plugin} requires {service}, which no plugin provides")
            }
            GraphError::MultipleProviders { service, plugins } => {
                write!(
                    f,
                    "single-provider service {service} is provided by {}",
                    plugins.join(", ")
                )
            }
            GraphError::EmptyActivation(id) => write!(f, "{id} has no activation condition"),
            GraphError::Cycle(ids) => write!(f, "dependency cycle: {}", ids.join(" -> ")),
            GraphError::ManifestPanicked { plugin, message } => {
                write!(f, "manifest of {plugin} panicked: {message}")
            }
            GraphError::UnknownPlugin(id) => write!(f, "no plugin {id} in the set"),
        }
    }
}

impl std::error::Error for GraphError {}
