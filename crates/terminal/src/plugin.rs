//! The "terminal" plugin (docs/design/terminal.md「插件」): provides
//! `dyn Terminals`, requires `dyn Projects` and `dyn SessionSource`, activates
//! after the first frame. Activation builds the empty table and takes the
//! service references: no thread, no IO, no PTY, no login environment.

use std::sync::Arc;

use lectern_executor::Executor;
use lectern_kernel::{
    Activation, ActivationToken, Active, Context, Manifest, Plugin, PluginError, PluginHandle,
    service,
};
use lectern_services::project::{ProjectEvent, Projects};
use lectern_services::session::SessionSource;
use lectern_services::terminal::Terminals;

use crate::diag::CloseReason;
use crate::service::Service;
use crate::slot::Command;
use crate::table::{Config, Shared};

/// The plugin ID.
pub const PLUGIN_ID: &str = "terminal";

/// What `app` hands the plugin.
pub struct TerminalDeps {
    pub executor: Executor,
}

pub struct TerminalPlugin {
    deps: TerminalDeps,
    config: ConfigSeed,
    /// The service of the latest activation, for the tests.
    #[cfg(any(test, feature = "test-support"))]
    provided: Provided,
    /// The token of the latest activation, for the tests (`copy_selection`).
    #[cfg(any(test, feature = "test-support"))]
    token: Arc<std::sync::Mutex<Option<ActivationToken>>>,
}

/// The service and the table an activation provided (tests).
#[cfg(any(test, feature = "test-support"))]
pub type Provided = Arc<std::sync::Mutex<Option<Arc<dyn Terminals>>>>;

/// The overrides, copied into each activation's `Config`.
#[derive(Default)]
struct ConfigSeed {
    shell: Option<std::path::PathBuf>,
    login_env: Option<Vec<(std::ffi::OsString, std::ffi::OsString)>>,
    thread_hook: Option<Arc<dyn Fn(lectern_services::terminal::TerminalId) + Send + Sync>>,
    threads: Arc<std::sync::atomic::AtomicUsize>,
}

impl TerminalPlugin {
    pub fn new(deps: TerminalDeps) -> TerminalPlugin {
        TerminalPlugin {
            deps,
            config: ConfigSeed::default(),
            #[cfg(any(test, feature = "test-support"))]
            provided: Provided::default(),
            #[cfg(any(test, feature = "test-support"))]
            token: Arc::default(),
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl TerminalPlugin {
    /// Tests and benchmarks: `Launch::Shell` starts this program, not
    /// `platform::login_env::user_shell`.
    pub fn with_shell(mut self, program: impl Into<std::path::PathBuf>) -> TerminalPlugin {
        self.config.shell = Some(program.into());
        self
    }

    /// Tests and benchmarks: agents start with this environment, not
    /// `platform::login_env::login_env` (which runs the user's shell).
    /// `Launch::Shell` inherits it too.
    pub fn with_login_env(
        mut self,
        vars: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    ) -> TerminalPlugin {
        self.config.login_env = Some(vars);
        self
    }

    /// Tests: called on a terminal's thread at the start of every round
    /// (a test makes one panic).
    pub fn with_thread_hook(
        mut self,
        hook: impl Fn(lectern_services::terminal::TerminalId) + Send + Sync + 'static,
    ) -> TerminalPlugin {
        self.config.thread_hook = Some(Arc::new(hook));
        self
    }

    /// The number of terminal threads running now.
    pub fn threads(&self) -> Arc<std::sync::atomic::AtomicUsize> {
        self.config.threads.clone()
    }

    /// The slot that holds the service of the latest activation.
    pub fn provided(&self) -> Provided {
        self.provided.clone()
    }

    /// The slot that holds the token of the latest activation, set when it
    /// is started.
    pub fn token(&self) -> Arc<std::sync::Mutex<Option<ActivationToken>>> {
        self.token.clone()
    }
}

impl Plugin for TerminalPlugin {
    /// provides: [dyn Terminals]; requires: [dyn Projects, dyn SessionSource];
    /// activation: [AfterFirstFrame].
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: vec![service::<dyn Terminals>()],
            requires: vec![service::<dyn Projects>(), service::<dyn SessionSource>()],
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        let shared = Arc::new(Shared::new(
            self.deps.executor.clone(),
            ctx.service::<dyn Projects>(),
            ctx.service_set::<dyn SessionSource>(),
            Config {
                shell: self.config.shell.clone(),
                login_env: self.config.login_env.clone(),
                thread_hook: self.config.thread_hook.clone(),
                threads: self.config.threads.clone(),
            },
        ));
        let terminals: Arc<dyn Terminals> = Arc::new(Service {
            shared: shared.clone(),
        });
        #[cfg(any(test, feature = "test-support"))]
        {
            *self
                .provided
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(terminals.clone());
        }
        ctx.provide::<dyn Terminals>(terminals)?;
        let table = shared.clone();
        ctx.on::<ProjectEvent>(move |event, _| {
            if let ProjectEvent::Closed { project } = event {
                for slot in table.remove_project(project) {
                    slot.push(Command::Close(CloseReason::ProjectClosed));
                }
            }
        });
        Ok(Box::new(TerminalActive {
            shared,
            #[cfg(any(test, feature = "test-support"))]
            token: self.token.clone(),
        }))
    }
}

struct TerminalActive {
    shared: Arc<Shared>,
    #[cfg(any(test, feature = "test-support"))]
    token: Arc<std::sync::Mutex<Option<ActivationToken>>>,
}

impl Active for TerminalActive {
    /// Lets the terminals opened since `activate` start their threads.
    fn started(&mut self, handle: PluginHandle) {
        #[cfg(any(test, feature = "test-support"))]
        {
            *self
                .token
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(handle.token());
        }
        self.shared.attach(handle);
    }
}

impl Drop for TerminalActive {
    /// Every terminal is asked to close; the call does not wait.
    fn drop(&mut self) {
        self.shared.stop();
    }
}
