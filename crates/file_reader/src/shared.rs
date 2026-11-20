//! What the reading view and the plugin share (docs/design/file_reader.md
//! 「本 crate 的公开项」「界面」): the window's [`Session`], the services the
//! view talks to and the view's hold on the session's wake-up.
//!
//! The session has one wake-up slot. While no view lives, the plugin's own
//! wake-up is in it: a request that arrives then makes `workspace` push the
//! reader screen, whose view installs its own wake-up and takes the
//! request. When the view goes, the plugin's wake-up comes back.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use lectern_kernel::{Name, ServiceRef};
use lectern_services::code_intel::{CodeIntel, Query};
use lectern_services::project::ProjectKey;
use lectern_services::workspace::{
    Contributions, Milestone, Route, Target, WorkspaceCommand, screens,
};

use crate::session::{Session, Wake};

/// What the Files tree keeps between its views. `workspace` drops the
/// sidebar's view whenever the sidebar is hidden (the reader hides it), so
/// the expansion and the selection live here, by path.
#[derive(Default)]
pub(crate) struct FilesMemory {
    /// The project they belong to: another project starts afresh.
    pub(crate) project: Option<ProjectKey>,
    /// The directories that were expanded, by path.
    pub(crate) expanded: HashSet<Box<str>>,
    /// The row that was selected, by path.
    pub(crate) selected: Option<Box<str>>,
}

pub(crate) struct Shared {
    pub(crate) session: Arc<Session>,
    /// Whether the views can enter edit mode (`edit::EDIT_REACHABLE`; the
    /// tests turn it on).
    edit: bool,
    contributions: ServiceRef<dyn Contributions>,
    code_intel: ServiceRef<dyn CodeIntel>,
    /// The wake-up of no view: asks `workspace` for the reader screen when
    /// a request is waiting.
    push: Wake,
    /// The views built so far, and the number of the latest one: only the
    /// latest one's end puts the plugin's wake-up back.
    built: AtomicU64,
    latest: Mutex<u64>,
    /// The Files tree's wake-up (`ProjectEvent::Changed`) and the number of
    /// the view that installed it.
    files_wake: Mutex<Option<(u64, Wake)>>,
    files_built: AtomicU64,
    files_memory: Mutex<FilesMemory>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// The route of the reader screen: one value, so that several requests
/// before the screen exists push it once (`workspace` ignores a push of the
/// screen that is on top).
fn route() -> Route {
    Route {
        screen: Name::Borrowed(screens::READER),
        target: Target::None,
    }
}

impl Shared {
    pub(crate) fn new(
        session: Arc<Session>,
        contributions: ServiceRef<dyn Contributions>,
        code_intel: ServiceRef<dyn CodeIntel>,
        edit: bool,
    ) -> Arc<Shared> {
        Arc::new_cyclic(|this: &std::sync::Weak<Shared>| {
            let weak = this.clone();
            let push: Wake = Arc::new(move || {
                let Some(shared) = weak.upgrade() else {
                    return;
                };
                if shared.session.pending() > 0 {
                    shared.dispatch(WorkspaceCommand::Push(route()));
                }
            });
            Shared {
                session,
                edit,
                contributions,
                code_intel,
                push,
                built: AtomicU64::new(0),
                latest: Mutex::new(0),
                files_wake: Mutex::new(None),
                files_built: AtomicU64::new(0),
                files_memory: Mutex::new(FilesMemory::default()),
            }
        })
    }

    /// Whether edit mode can be entered: the Edit… button, ⌘E and the Edit
    /// action work only when it can.
    pub(crate) fn edit_reachable(&self) -> bool {
        self.edit
    }

    /// The plugin's wake-up goes in the session's slot (`started`, and
    /// when a view ends).
    pub(crate) fn install_push(&self) {
        self.session.set_waker(Some(self.push.clone()));
    }

    /// A view is built: its number. Its wake-up replaces the plugin's.
    pub(crate) fn begin_view(&self, wake: Wake) -> u64 {
        let id = self.built.fetch_add(1, Ordering::SeqCst) + 1;
        *lock(&self.latest) = id;
        self.session.set_waker(Some(wake));
        id
    }

    /// The view `id` ends: if it is the latest, the document goes with its
    /// focus and the plugin's wake-up comes back.
    pub(crate) fn end_view(&self, id: u64) {
        let latest = *lock(&self.latest);
        if latest != id {
            return;
        }
        self.session.close();
        self.install_push();
    }

    /// A Files view is built: its number. Its wake-up replaces the
    /// previous one's.
    pub(crate) fn attach_files(&self, wake: Wake) -> u64 {
        let id = self.files_built.fetch_add(1, Ordering::SeqCst) + 1;
        *lock(&self.files_wake) = Some((id, wake));
        id
    }

    /// The Files view `id` ends: its wake-up goes unless a newer view
    /// installed its own.
    pub(crate) fn detach_files(&self, id: u64) {
        let mut slot = lock(&self.files_wake);
        if slot.as_ref().is_some_and(|(held, _)| *held == id) {
            *slot = None;
        }
    }

    /// The project's directory table, file list or active worktree changed
    /// (a `ProjectEvent`, any thread): the Files tree reads the snapshot
    /// again.
    pub(crate) fn files_changed(&self) {
        let wake = lock(&self.files_wake)
            .as_ref()
            .map(|(_, wake)| wake.clone());
        if let Some(wake) = wake {
            wake();
        }
    }

    /// What the last Files view kept, for a view of `project`.
    pub(crate) fn take_files_memory(&self, project: Option<&ProjectKey>) -> FilesMemory {
        let kept = std::mem::take(&mut *lock(&self.files_memory));
        if kept.project.as_ref() == project {
            kept
        } else {
            FilesMemory {
                project: project.cloned(),
                ..FilesMemory::default()
            }
        }
    }

    pub(crate) fn keep_files_memory(&self, memory: FilesMemory) {
        *lock(&self.files_memory) = memory;
    }

    pub(crate) fn dispatch(&self, command: WorkspaceCommand) {
        if let Some(contributions) = self.contributions.get() {
            contributions.dispatch(command);
        }
    }

    pub(crate) fn report_milestone(&self, milestone: Milestone) {
        if let Some(contributions) = self.contributions.get() {
            contributions.report_milestone(milestone);
        }
    }

    /// `CodeIntel::prefetch`; false when the service is gone.
    pub(crate) fn prefetch(&self, query: Query) -> bool {
        match self.code_intel.get() {
            Some(code_intel) => {
                code_intel.prefetch(query);
                true
            }
            None => false,
        }
    }
}
