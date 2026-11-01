//! The cell "project-home" and "project-home-waiting" share (docs/design/
//! project_home.md「本 crate 的公开项」): `dyn Conversations` may be absent, so
//! the sessions that wait for the user are read through this cell, and a change
//! only wakes the views.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lectern_kernel::ServiceRef;
use lectern_services::conversation::Conversations;
use lectern_services::session::SessionKey;

/// The callback "project-home" registers: wakes the views. Any thread, only
/// sends on channels; it never recomposes the snapshot.
type OnChange = Arc<dyn Fn() + Send + Sync>;

/// Shared by "project-home" and "project-home-waiting". `app` builds one and
/// hands a clone to each.
///
/// The two plugins activate in any order. Nothing here depends on it: the
/// views read [`waiting`](WaitingSlot::waiting) when they are woken, and the
/// slot wakes them whenever the service comes, goes or announces a change.
#[derive(Clone, Default)]
pub struct WaitingSlot {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    conversations: Option<ServiceRef<dyn Conversations>>,
    on_change: Option<OnChange>,
}

impl WaitingSlot {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// "project-home-waiting" `started`: records the service and wakes the
    /// views, since sessions may wait already.
    pub fn set_conversations(&self, conversations: ServiceRef<dyn Conversations>) {
        self.lock().conversations = Some(conversations);
        self.changed();
    }

    /// "project-home-waiting" is deactivated: the marks go away.
    pub fn clear_conversations(&self) {
        self.lock().conversations = None;
        self.changed();
    }

    /// "project-home" `started`.
    pub fn set_on_change(&self, on_change: OnChange) {
        self.lock().on_change = Some(on_change);
    }

    /// The listener of `ConversationEvent::WaitingChanged`, and the other
    /// writers: only calls `on_change`.
    pub fn changed(&self) {
        let on_change = self.lock().on_change.clone();
        if let Some(on_change) = on_change {
            on_change();
        }
    }

    /// The sessions with a permission request that waits for the user; empty
    /// when there is no `dyn Conversations` or its provider is no longer live.
    /// Takes a lock and copies an `Arc` (`Conversations::waiting`), so the main
    /// thread may call it. The provider's admission is held for this one call.
    pub fn waiting(&self) -> Arc<[SessionKey]> {
        let service = self.lock().conversations.clone();
        service
            .and_then(|service| service.get().map(|conversations| conversations.waiting()))
            .unwrap_or_else(|| Arc::from(Vec::new()))
    }
}
