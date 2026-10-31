//! The plugins "conversation" and "conversation-live" and the state they
//! share with the page (docs/design/conversation.md「接口」「插件」).
//! `activate` only takes the services, registers the contributions and
//! subscribes; nothing is read, submitted or drawn before the inspector shows
//! the page (ADR 0017). The kernel has no optional dependencies, so what the
//! page can do without a service is a separate plugin: "conversation-live"
//! brings `dyn Conversations`, and the page works (and says so) without it.

use std::sync::{Arc, Mutex, MutexGuard};

use futures::channel::mpsc;
use gpui::{App, AppContext};
use lectern_kernel::{
    Activation, Active, Context, Manifest, Name, Plugin, PluginError, PluginHandle, ServiceRef,
    ServiceSet, service,
};
use lectern_logging::{debug, warn};
use lectern_services::conversation::{ConversationEvent, Conversations};
use lectern_services::review::ReviewTarget;
use lectern_services::session::{SessionKey, SessionSource, SessionSummary};
use lectern_services::workspace::{
    Contribution, ContributionKind, Contributions, Registration, Route, Target, WindowId,
    WorkspaceEvent, screens,
};
use lectern_ui::contrib::{InspectorTab, KeyBindings};

use crate::actions;
use crate::view::ConversationTab;

/// The ID of "conversation" in the `PluginSet`.
pub const PLUGIN_ID: &str = "conversation";

/// The ID of "conversation-live" in the `PluginSet`.
pub const LIVE_ID: &str = "conversation-live";

/// The ID of the inspector's page.
pub const TAB_ID: &str = "conversation";

/// The page comes after the Files page (whose `order` is 0).
const TAB_ORDER: u16 = 10;

/// What the screen on top shows, as far as this page is concerned.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Current {
    /// No review screen (the project home, Welcome, the reader).
    None,
    /// A review, but of no session (`Outside`, `Range`).
    NotSession,
    Session(SessionKey),
}

impl Current {
    /// The discriminant, for the `route changed` record.
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Current::None => "none",
            Current::NotSession => "not_session",
            Current::Session(_) => "session",
        }
    }

    /// The review of a session is the only route this page follows.
    pub(crate) fn of(route: &Route) -> Current {
        if route.screen.as_ref() != screens::REVIEW {
            return Current::None;
        }
        match &route.target {
            Target::Review(ReviewTarget::Session(key)) => Current::Session(key.clone()),
            Target::Review(_) => Current::NotSession,
            _ => Current::None,
        }
    }
}

/// What listeners on other threads send to the page's foreground task.
#[derive(Clone, Debug)]
pub(crate) enum Job {
    Route(Current),
    /// `ConversationEvent::Changed`.
    Conversation {
        session: SessionKey,
        generation: u64,
    },
    /// "conversation-live" started or stopped.
    Services,
}

pub(crate) type JobSender = mpsc::UnboundedSender<Job>;
pub(crate) type JobReceiver = mpsc::UnboundedReceiver<Job>;

/// Shared by the plugins and the page. `Mutex`es of options: the plugins fill
/// them when they activate and empty them when they are deactivated.
pub(crate) struct Shared {
    contributions: Mutex<Option<ServiceRef<dyn Contributions>>>,
    sessions: Mutex<Option<ServiceSet<dyn SessionSource>>>,
    conversations: Mutex<Option<ServiceRef<dyn Conversations>>>,
    /// The review on the screen on top.
    current: Mutex<Current>,
    /// The living pages' channels. The inspector has one page, so there is
    /// usually one element.
    tabs: Mutex<Vec<JobSender>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    fn new() -> Shared {
        Shared {
            contributions: Mutex::new(None),
            sessions: Mutex::new(None),
            conversations: Mutex::new(None),
            current: Mutex::new(Current::None),
            tabs: Mutex::new(Vec::new()),
        }
    }

    /// The service now, if its plugin is active. A copy of the reference:
    /// the lock is not held while the service is called.
    pub(crate) fn with_conversations<R>(
        &self,
        f: impl FnOnce(&dyn Conversations) -> R,
    ) -> Option<R> {
        let conversations = lock(&self.conversations).clone()?;
        let admitted = conversations.get()?;
        Some(f(&*admitted))
    }

    pub(crate) fn has_conversations(&self) -> bool {
        lock(&self.conversations)
            .as_ref()
            .is_some_and(|conversations| conversations.get().is_some())
    }

    /// The summary of a session in the sessions list, None if no source
    /// knows it (or the sessions provider is gone).
    pub(crate) fn session_summary(&self, key: &SessionKey) -> Option<SessionSummary> {
        let sessions = lock(&self.sessions).clone()?;
        let mut found = None;
        sessions.for_each(|source| {
            if found.is_none()
                && let Some(summary) = source.sessions().get(key)
            {
                found = Some(summary.clone());
            }
        });
        found
    }

    pub(crate) fn current(&self) -> Current {
        lock(&self.current).clone()
    }

    /// Records the review on the screen on top and tells the pages when it
    /// changed. Any thread.
    fn set_current(&self, current: Current) {
        {
            let mut shown = lock(&self.current);
            if *shown == current {
                return;
            }
            *shown = current.clone();
        }
        debug!("route changed"; current = current.name());
        self.broadcast(Job::Route(current));
    }

    /// Registers a page's channel. Called before the page reads `current`, so
    /// that a change between the two is not lost: the page applies the job
    /// again and finds the same value.
    pub(crate) fn register_tab(&self) -> JobReceiver {
        let (tx, rx) = mpsc::unbounded();
        lock(&self.tabs).push(tx);
        rx
    }

    /// Sends to every living page; a page whose receiver is gone leaves the
    /// list.
    fn broadcast(&self, job: Job) {
        lock(&self.tabs).retain(|tab| tab.unbounded_send(job.clone()).is_ok());
    }

    /// Runs on the thread that emitted the event: only records and forwards.
    fn on_workspace(&self, event: &WorkspaceEvent) {
        match event {
            WorkspaceEvent::ScreenChanged { window, to, .. } if *window == WindowId(0) => {
                self.set_current(Current::of(to));
            }
            WorkspaceEvent::WindowClosed { window } if *window == WindowId(0) => {
                self.set_current(Current::None);
            }
            _ => {}
        }
    }

    /// Runs on the thread that emitted the event: only forwards.
    fn on_conversation(&self, event: &ConversationEvent) {
        if let ConversationEvent::Changed {
            session,
            generation,
        } = event
        {
            self.broadcast(Job::Conversation {
                session: session.clone(),
                generation: *generation,
            });
        }
    }

    fn clear_main(&self) {
        *lock(&self.contributions) = None;
        *lock(&self.sessions) = None;
        *lock(&self.current) = Current::None;
    }
}

/// The plugin "conversation": requires `dyn Contributions` and
/// `dyn SessionSource`, activates after the first frame. `app` builds the
/// plugins with [`plugins`] and puts them in the `PluginSet`.
pub struct ConversationPlugin {
    shared: Arc<Shared>,
}

/// The plugin "conversation-live": requires `dyn Conversations`.
pub struct LivePlugin {
    shared: Arc<Shared>,
}

/// The plugins, to be put in the `PluginSet` by `app`: "conversation" and
/// "conversation-live" (the third, "conversation-terminal", comes with the
/// Terminal page). They share one [`Shared`]. Does no IO.
pub fn plugins() -> (ConversationPlugin, LivePlugin) {
    let shared = Arc::new(Shared::new());
    (
        ConversationPlugin {
            shared: shared.clone(),
        },
        LivePlugin { shared },
    )
}

impl Plugin for ConversationPlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: Vec::new(),
            requires: vec![
                service::<dyn Contributions>(),
                service::<dyn SessionSource>(),
            ],
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        let contributions = ctx.service::<dyn Contributions>();
        let sessions = ctx.service_set::<dyn SessionSource>();
        *lock(&self.shared.contributions) = Some(contributions.clone());
        *lock(&self.shared.sessions) = Some(sessions);
        // From here on a failure must not leave the services in `Shared`: the
        // guard clears them unless the activation goes through.
        let active = ConversationActive {
            shared: self.shared.clone(),
        };
        let Some(admitted) = contributions.get() else {
            return Err(PluginError::DependencyUnavailable);
        };
        for registration in register(&*admitted, &self.shared) {
            ctx.own_staged(registration);
        }
        let shared = self.shared.clone();
        ctx.on::<WorkspaceEvent>(move |event, _| shared.on_workspace(event));
        Ok(Box::new(active))
    }
}

/// The contributions (docs/design/conversation.md「向 workspace 登记的贡献」):
/// no token yet, so the guards go to `ctx.own_staged` and take effect at the
/// activation's commit.
fn register(contributions: &dyn Contributions, shared: &Arc<Shared>) -> Vec<Registration> {
    let build_shared = shared.clone();
    let tab = InspectorTab {
        label: lectern_i18n::keys::conversation::TAB,
        build: Arc::new(move |_, _, cx: &mut App| {
            let shared = build_shared.clone();
            cx.new(|cx| ConversationTab::new(shared, cx)).into()
        }),
        // Not `Some(review)`: a page scoped to a screen would hide the pages
        // of `None`, the Files page among them (docs/design/workspace.md「检查器页的范围」).
        screen: None,
    };
    vec![
        contributions.register(
            Contribution {
                kind: ContributionKind::InspectorTab,
                id: Name::Borrowed(TAB_ID),
                order: TAB_ORDER,
                payload: tab.into_payload(),
            },
            None,
        ),
        contributions.register(
            Contribution {
                kind: ContributionKind::KeyBindings,
                id: Name::Borrowed(actions::KEY_BINDINGS_ID),
                order: 0,
                payload: KeyBindings(actions::key_bindings()).into_payload(),
            },
            None,
        ),
    ]
}

struct ConversationActive {
    shared: Arc<Shared>,
}

impl Active for ConversationActive {
    /// Reads the screen that is up already: a `ScreenChanged` before `Live`
    /// is not delivered (docs/design/kernel.md「激活时的上下文」).
    fn started(&mut self, _: PluginHandle) {
        let route = self
            .shared
            .with_contributions(|contributions| contributions.current_route(WindowId(0)))
            .flatten();
        match route {
            Some(route) => self.shared.set_current(Current::of(&route)),
            None => warn!("route unavailable"),
        }
    }
}

impl Drop for ConversationActive {
    /// Deactivation: `workspace` drops the registered page with the
    /// registrations; the services leave `Shared` so that a late call finds
    /// none.
    fn drop(&mut self) {
        self.shared.clear_main();
    }
}

impl Shared {
    fn with_contributions<R>(&self, f: impl FnOnce(&dyn Contributions) -> R) -> Option<R> {
        let contributions = lock(&self.contributions).clone()?;
        let admitted = contributions.get()?;
        Some(f(&*admitted))
    }
}

impl Plugin for LivePlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: Vec::new(),
            requires: vec![service::<dyn Conversations>()],
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        let conversations = ctx.service::<dyn Conversations>();
        let shared = self.shared.clone();
        ctx.on::<ConversationEvent>(move |event, _| shared.on_conversation(event));
        Ok(Box::new(LiveActive {
            shared: self.shared.clone(),
            conversations: Some(conversations),
        }))
    }
}

struct LiveActive {
    shared: Arc<Shared>,
    conversations: Option<ServiceRef<dyn Conversations>>,
}

impl Active for LiveActive {
    /// The service is written in `started`: `activate` has no side effects.
    /// The living page watches the session it shows.
    fn started(&mut self, _: PluginHandle) {
        if let Some(conversations) = self.conversations.take() {
            *lock(&self.shared.conversations) = Some(conversations);
            self.shared.broadcast(Job::Services);
        }
    }
}

impl Drop for LiveActive {
    fn drop(&mut self) {
        let had = lock(&self.shared.conversations).take().is_some();
        if had {
            self.shared.broadcast(Job::Services);
        }
    }
}
