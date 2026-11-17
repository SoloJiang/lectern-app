//! What the reading view and the plugin share (docs/design/file_reader.md
//! 「本 crate 的公开项」「界面」): the window's [`Session`], the services the
//! view talks to and the view's hold on the session's wake-up.
//!
//! The session has one wake-up slot. While no view lives, the plugin's own
//! wake-up is in it: a request that arrives then makes `workspace` push the
//! reader screen, whose view installs its own wake-up and takes the
//! request. When the view goes, the plugin's wake-up comes back.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use lectern_kernel::{Name, ServiceRef};
use lectern_services::code_intel::{CodeIntel, Query};
use lectern_services::workspace::{
    Contributions, Milestone, Route, Target, WorkspaceCommand, screens,
};

use crate::session::{Session, Wake};

pub(crate) struct Shared {
    pub(crate) session: Arc<Session>,
    contributions: ServiceRef<dyn Contributions>,
    code_intel: ServiceRef<dyn CodeIntel>,
    /// The wake-up of no view: asks `workspace` for the reader screen when
    /// a request is waiting.
    push: Wake,
    /// The views built so far, and the number of the latest one: only the
    /// latest one's end puts the plugin's wake-up back.
    built: AtomicU64,
    latest: Mutex<u64>,
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
                contributions,
                code_intel,
                push,
                built: AtomicU64::new(0),
                latest: Mutex::new(0),
            }
        })
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
