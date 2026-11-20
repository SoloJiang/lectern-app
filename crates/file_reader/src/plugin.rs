//! The "file-reader" plugin (docs/design/file_reader.md「本 crate 的公开项」):
//! provides `dyn Reader`, requires `dyn Projects`, `dyn CodeIntel` and
//! `dyn Contributions`, activates after the first frame.
//!
//! - `activate` builds the engine (no IO, no grammar, no query compiled),
//!   provides the service, subscribes to `ProjectEvent` and hands the
//!   reader screen and its key bindings to `workspace` (`own_staged`).
//! - `started` hands the activation's handle to the engine and puts the
//!   plugin's wake-up in the session (a request that arrives while no view
//!   lives makes `workspace` push the reader screen). No task.
//! - Deactivation closes the document (the focus is cleared) and drops the
//!   engine with the session.

#[cfg(test)]
use std::sync::Weak;
use std::sync::{Arc, Mutex, PoisonError};

use lectern_cache::Cache;
use lectern_executor::Executor;
use lectern_fs_watch::Watcher;
use lectern_kernel::{
    Activation, Active, Context, Manifest, Plugin, PluginError, PluginHandle, ServiceRef, service,
};
use lectern_services::code_intel::{CodeIntel, Focus};
use lectern_services::project::{ProjectEvent, ProjectKey, ProjectSnapshot, Projects, WorktreeKey};
use lectern_services::reader::{Reader, ReaderEvent};
use lectern_services::workspace::Contributions;
use lectern_syntax::{Engine, EngineDeps, Languages};

use crate::screen;
use crate::session::{Host, Session};
use crate::shared::Shared;

/// The ID of "file-reader" in the `PluginSet`.
pub const PLUGIN_ID: &str = "file-reader";

/// What `app` creates once and hands to the plugin (ADR 0025).
#[derive(Clone)]
pub struct FileReaderDeps {
    pub executor: Executor,
    pub cache: Cache,
    pub watcher: Watcher,
}

/// The plugin (ID "file-reader"), put in the `PluginSet` by `app`.
pub struct FileReaderPlugin {
    deps: FileReaderDeps,
    /// The latest activation's session (tests). Weak: it never keeps a
    /// deactivated activation alive.
    #[cfg(test)]
    last: Arc<Mutex<Weak<Session>>>,
}

impl FileReaderPlugin {
    /// No IO.
    pub fn new(deps: FileReaderDeps) -> FileReaderPlugin {
        FileReaderPlugin {
            deps,
            #[cfg(test)]
            last: Arc::default(),
        }
    }

    /// Where every activation of this plugin leaves its session, kept after
    /// the plugin moves into a kernel (tests).
    #[cfg(test)]
    pub(crate) fn sessions(&self) -> Arc<Mutex<Weak<Session>>> {
        self.last.clone()
    }
}

impl Plugin for FileReaderPlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: vec![service::<dyn Reader>()],
            requires: vec![
                service::<dyn Projects>(),
                service::<dyn CodeIntel>(),
                service::<dyn Contributions>(),
            ],
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    /// The engine, the session, the service and the `ProjectEvent`
    /// listener. No IO, no grammar, no task.
    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        let host = Arc::new(KernelHost {
            projects: ctx.service::<dyn Projects>(),
            code_intel: ctx.service::<dyn CodeIntel>(),
            handle: Mutex::new(None),
        });
        let engine = Engine::new(EngineDeps {
            executor: self.deps.executor.clone(),
            cache: self.deps.cache.clone(),
            watcher: self.deps.watcher.clone(),
            languages: Languages::core(),
        });
        let session = Arc::new(Session::new(engine, host.clone()));
        let contributions = ctx.service::<dyn Contributions>();
        let shared = Shared::new(
            session.clone(),
            contributions.clone(),
            ctx.service::<dyn CodeIntel>(),
        );
        let Some(admitted) = contributions.get() else {
            return Err(PluginError::new("the contributions provider is not live"));
        };
        for registration in screen::registrations(&shared) {
            ctx.own_staged(admitted.register(registration, None));
        }
        drop(admitted);
        ctx.provide::<dyn Reader>(session.clone())?;
        let listener = session.clone();
        ctx.on::<ProjectEvent>(move |event, _| {
            if let ProjectEvent::Closed { project } = event {
                listener.close_project(project);
            }
        });
        #[cfg(test)]
        {
            *self.last.lock().unwrap_or_else(PoisonError::into_inner) = Arc::downgrade(&session);
        }
        Ok(Box::new(FileReaderActive {
            session,
            host,
            shared,
        }))
    }
}

/// One activation.
struct FileReaderActive {
    session: Arc<Session>,
    host: Arc<KernelHost>,
    shared: Arc<Shared>,
}

impl Active for FileReaderActive {
    fn started(&mut self, handle: PluginHandle) {
        *self
            .host
            .handle
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(handle.clone());
        self.session.start(handle);
        self.shared.install_push();
    }
}

impl Drop for FileReaderActive {
    /// Deactivation: the document goes and its focus is cleared while
    /// `code_intel` (deactivated after this plugin) still answers.
    fn drop(&mut self) {
        self.session.set_waker(None);
        self.session.close();
    }
}

/// The session's surroundings through the kernel: every call takes the
/// provider's admission and answers as with nothing there once it is gone.
struct KernelHost {
    projects: ServiceRef<dyn Projects>,
    code_intel: ServiceRef<dyn CodeIntel>,
    /// Set by `started`; the events go through it.
    handle: Mutex<Option<PluginHandle>>,
}

impl Host for KernelHost {
    fn snapshot(&self, project: &ProjectKey) -> Option<Arc<ProjectSnapshot>> {
        self.projects.get()?.snapshot(project)
    }

    fn set_focus(&self, project: &ProjectKey, worktree: Option<&WorktreeKey>, focus: Focus) {
        if let Some(code_intel) = self.code_intel.get() {
            code_intel.set_focus(project, worktree, focus);
        }
    }

    fn emit(&self, event: ReaderEvent) {
        let handle = self
            .handle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(handle) = handle {
            handle.emitter().emit(event);
        }
    }
}
