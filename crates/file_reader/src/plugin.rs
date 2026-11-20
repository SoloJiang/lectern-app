//! The "file-reader" plugin (docs/design/file_reader.md「本 crate 的公开项」):
//! provides `dyn Reader`, requires `dyn Projects`, `dyn CodeIntel` and
//! `dyn Contributions`, activates after the first frame.
//!
//! - `activate` builds the engine (no IO, no grammar, no query compiled),
//!   provides the service, subscribes to `ProjectEvent` and hands the
//!   reader screen, its key bindings and the leave guard to `workspace`
//!   (`own_staged`).
//! - `started` hands the activation's handle to the engine and puts the
//!   plugin's wake-up in the session (a request that arrives while no view
//!   lives makes `workspace` push the reader screen). No task.
//! - Deactivation closes the document (the focus is cleared) and drops the
//!   engine with the session.

use std::sync::{Arc, Mutex, PoisonError, Weak};

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

use crate::edit::{EDIT_REACHABLE, LeaveGuard};
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

/// Where the plugin leaves its `dyn Reader` while it is active, for the
/// measurement build of `app` (docs/design/bench.md「进程级基准」): the
/// driver there opens files as `review-view` does, but is not a plugin and
/// cannot require the service. Holds a `Weak`, so it keeps nothing alive.
#[derive(Clone, Default)]
pub struct ReaderSlot(Arc<Mutex<Option<Weak<dyn Reader>>>>);

impl ReaderSlot {
    /// The reader of the active plugin; None before its activation and after
    /// its deactivation. Any thread.
    pub fn get(&self) -> Option<Arc<dyn Reader>> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .and_then(Weak::upgrade)
    }

    fn set(&self, reader: Weak<dyn Reader>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(reader);
    }
}

/// The plugin (ID "file-reader"), put in the `PluginSet` by `app`.
pub struct FileReaderPlugin {
    deps: FileReaderDeps,
    /// Whether the views can enter edit mode (`edit::EDIT_REACHABLE`).
    edit: bool,
    /// Where the `dyn Reader` is left, when `app` asked for it.
    slot: Option<ReaderSlot>,
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
            edit: EDIT_REACHABLE,
            slot: None,
            #[cfg(test)]
            last: Arc::default(),
        }
    }

    /// The plugin leaves its `dyn Reader` in `slot` when it activates.
    pub fn with_reader_slot(mut self, slot: ReaderSlot) -> FileReaderPlugin {
        self.slot = Some(slot);
        self
    }

    /// The plugin with edit mode reachable or not (tests).
    #[cfg(test)]
    pub(crate) fn with_edit(mut self, edit: bool) -> FileReaderPlugin {
        self.edit = edit;
        self
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
            self.edit,
        );
        let Some(admitted) = contributions.get() else {
            return Err(PluginError::new("the contributions provider is not live"));
        };
        for registration in screen::registrations(&shared) {
            ctx.own_staged(admitted.register(registration, None));
        }
        // One guard for the window: nothing is allocated for it beyond this
        // handle until the user enters edit mode.
        ctx.own_staged(
            admitted.register_leave_guard(Arc::new(LeaveGuard::new(shared.clone())), None),
        );
        drop(admitted);
        ctx.provide::<dyn Reader>(session.clone())?;
        if let Some(slot) = &self.slot {
            let reader: Arc<dyn Reader> = session.clone();
            slot.set(Arc::downgrade(&reader));
        }
        let listener = session.clone();
        let files = shared.clone();
        ctx.on::<ProjectEvent>(move |event, _| match event {
            ProjectEvent::Closed { project } => {
                listener.close_project(project);
                files.files_changed();
            }
            // The Files tree follows the directory table (and the file list
            // and the active worktree it is built from).
            ProjectEvent::Changed { parts, .. } => {
                if parts.dirs || parts.files || parts.active {
                    files.files_changed();
                }
            }
            ProjectEvent::Opened { .. } => files.files_changed(),
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

    fn open_projects(&self) -> Vec<ProjectKey> {
        self.projects
            .get()
            .map(|projects| projects.open_projects())
            .unwrap_or_default()
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
