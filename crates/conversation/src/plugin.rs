//! The plugins "conversation", "conversation-live" and "conversation-terminal"
//! and the state they share with the page (docs/design/conversation.md「接口」「插件」).
//! `activate` only takes the services, registers the contributions and
//! subscribes; nothing is read, submitted or drawn before the inspector shows
//! the page (ADR 0017). The kernel has no optional dependencies, so what the
//! page can do without a service is a separate plugin: "conversation-live"
//! brings `dyn Conversations`, "conversation-terminal" brings `dyn Terminals`
//! with the terminal's key bindings and leave guard, and the page works (and
//! says so) without either.

use std::sync::{Arc, Mutex, MutexGuard};

use futures::channel::mpsc;
use gpui::{App, AppContext, Window};
use lectern_kernel::{
    Activation, ActivationToken, Active, Context, Manifest, Name, Plugin, PluginError,
    PluginHandle, ServiceRef, ServiceSet, service,
};
use lectern_logging::{debug, info, warn};
use lectern_services::conversation::{ConversationEvent, Conversations};
use lectern_services::review::ReviewTarget;
use lectern_services::session::{SessionKey, SessionSource, SessionSummary};
use lectern_services::terminal::{RunState, TerminalEvent, Terminals};
use lectern_services::workspace::{
    Contribution, ContributionKind, Contributions, Registration, Route, Target, WindowId,
    WorkspaceCommand, WorkspaceEvent, screens,
};
use lectern_ui::contrib::{InspectorTab, KeyBindings};

use crate::actions;
use crate::diag;
use crate::guard::{AskReceiver, AskSender, LeaveAsk, TerminalGuard};
use crate::memory::Memory;
use crate::view::ConversationTab;

/// The ID of "conversation" in the `PluginSet`.
pub const PLUGIN_ID: &str = "conversation";

/// The ID of "conversation-live" in the `PluginSet`.
pub const LIVE_ID: &str = "conversation-live";

/// The ID of "conversation-terminal" in the `PluginSet`.
pub const TERMINAL_ID: &str = "conversation-terminal";

/// The ID of the inspector's page.
pub const TAB_ID: &str = "conversation";

/// The ID of the terminal's `KeyBindings` contribution.
const TERMINAL_KEY_BINDINGS_ID: &str = "conversation-terminal";

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
    /// "conversation-live" or "conversation-terminal" started or stopped.
    Services,
    /// A terminal event the page cares about: `Listed` (the state of the
    /// terminal that may drive the session is read again) and `Frame` (the
    /// pane of the terminal on the Terminal page is refreshed). `Bell` is not
    /// forwarded.
    Terminal(TerminalEvent),
}

pub(crate) type JobSender = mpsc::UnboundedSender<Job>;
pub(crate) type JobReceiver = mpsc::UnboundedReceiver<Job>;

/// Shared by the plugins and the page. `Mutex`es of options: the plugins fill
/// them when they activate and empty them when they are deactivated.
pub(crate) struct Shared {
    contributions: Mutex<Option<ServiceRef<dyn Contributions>>>,
    sessions: Mutex<Option<ServiceSet<dyn SessionSource>>>,
    conversations: Mutex<Option<ServiceRef<dyn Conversations>>>,
    terminals: Mutex<Option<ServiceRef<dyn Terminals>>>,
    /// The handle of this activation of "conversation-terminal":
    /// the terminal grid's view needs its token (the copy of a selection is
    /// handed to the terminal with it).
    terminal_handle: Mutex<Option<PluginHandle>>,
    /// Where the leave guard sends its question: the foreground task that
    /// the page started in its window. None until a page is built.
    asker: Mutex<Option<AskSender>>,
    /// The review on the screen on top.
    current: Mutex<Current>,
    /// `Conversations::waiting()` as of the last `WaitingChanged`: the sessions
    /// that newly wait are found by comparing with it.
    waiting: Mutex<Arc<[SessionKey]>>,
    /// The drafts and the pages the sessions were last on.
    memory: Mutex<Memory>,
    /// The living pages' channels. The inspector has one page, so there is
    /// usually one element.
    tabs: Mutex<Vec<JobSender>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    pub(crate) fn new() -> Shared {
        Shared {
            contributions: Mutex::new(None),
            sessions: Mutex::new(None),
            conversations: Mutex::new(None),
            terminals: Mutex::new(None),
            terminal_handle: Mutex::new(None),
            asker: Mutex::new(None),
            current: Mutex::new(Current::None),
            waiting: Mutex::new(Arc::from([])),
            memory: Mutex::new(Memory::new()),
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

    /// The terminal service, if its plugin is active: a copy of the
    /// reference (for the terminal grid's view, which keeps it).
    pub(crate) fn terminals(&self) -> Option<ServiceRef<dyn Terminals>> {
        let terminals = lock(&self.terminals).clone()?;
        terminals.get().is_some().then_some(terminals)
    }

    /// The token of the activation of "conversation-terminal" that is live.
    pub(crate) fn terminal_token(&self) -> Option<ActivationToken> {
        lock(&self.terminal_handle)
            .as_ref()
            .filter(|handle| handle.is_active())
            .map(PluginHandle::token)
    }

    /// Runs `f` with the terminal service.
    pub(crate) fn with_terminals<R>(&self, f: impl FnOnce(&dyn Terminals) -> R) -> Option<R> {
        let terminals = lock(&self.terminals).clone()?;
        let admitted = terminals.get()?;
        Some(f(&*admitted))
    }

    /// Starts the task that shows the leave guard's confirmations in the
    /// window of the page that was just built, and replaces the one of an
    /// earlier page (its sender is dropped, so it ends). Main thread.
    pub(crate) fn serve_leave_asks(&self, window: &mut Window, cx: &mut App) {
        let (sender, receiver): (AskSender, AskReceiver) = mpsc::unbounded();
        *lock(&self.asker) = Some(sender);
        window
            .spawn(cx, async move |cx| crate::guard::serve(receiver, cx).await)
            .detach();
    }

    /// Hands a question to the page's task. Any thread. Gives the question
    /// back when there is no task, or its window is gone.
    pub(crate) fn ask_leave(&self, ask: LeaveAsk) -> Result<(), LeaveAsk> {
        match lock(&self.asker).as_ref() {
            Some(asker) => asker.unbounded_send(ask).map_err(|e| e.into_inner()),
            None => Err(ask),
        }
    }

    /// The state of the terminal that holds the session, if there is one
    /// (any state: the caller decides which ones drive the session). None
    /// without `dyn Terminals` or without a terminal for the session.
    pub(crate) fn terminal_state(&self, session: &SessionKey) -> Option<RunState> {
        let terminals = lock(&self.terminals).clone()?;
        let admitted = terminals.get()?;
        let id = admitted.for_session(session)?;
        admitted.info(id).map(|info| info.state)
    }

    /// Whether a terminal drives the session now: it starts or runs.
    pub(crate) fn terminal_drives(&self, session: &SessionKey) -> bool {
        crate::drive::drives(self.terminal_state(session).as_ref())
    }

    /// Runs `f` on the drafts.
    pub(crate) fn with_memory<R>(&self, f: impl FnOnce(&mut Memory) -> R) -> R {
        f(&mut lock(&self.memory))
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
            // The window the confirmations are shown in is gone; its task
            // ends with the sender. A page built in the window that a Dock
            // reopen makes starts a new one.
            WorkspaceEvent::WindowClosed { window } if *window == WindowId(0) => {
                *lock(&self.asker) = None;
            }
            _ => {}
        }
    }

    /// Runs on the thread that emitted the event: forwards, and on
    /// `WaitingChanged` looks for the session on the screen.
    fn on_conversation(&self, event: &ConversationEvent) {
        match event {
            ConversationEvent::Changed {
                session,
                generation,
            } => {
                self.broadcast(Job::Conversation {
                    session: session.clone(),
                    generation: *generation,
                });
            }
            ConversationEvent::WaitingChanged => self.on_waiting(),
        }
    }

    /// The set of sessions that wait for the user changed. A session that
    /// newly waits and is the one under review brings the Conversation page
    /// up (a command to `workspace`; the focus does not move). The page
    /// need not be alive for this: the inspector may be hidden, and the
    /// command shows it. Runs on the provider's thread: reads two small
    /// values and sends one command.
    fn on_waiting(&self) {
        let Some(now) = self.with_conversations(|conversations| conversations.waiting()) else {
            return;
        };
        let before = std::mem::replace(&mut *lock(&self.waiting), now.clone());
        let new: Vec<&SessionKey> = now.iter().filter(|key| !before.contains(key)).collect();
        debug!(
            "waiting changed";
            sessions = diag::count(now.len()),
            new = diag::count(new.len())
        );
        let Current::Session(shown) = self.current() else {
            return;
        };
        if !new.contains(&&shown) {
            return;
        }
        let pending = self
            .with_conversations(|conversations| conversations.snapshot(&shown))
            .flatten()
            .map_or(0, |snapshot| snapshot.permissions.len());
        let _scope = lectern_logging::scope(diag::session_ctx(&shown.id));
        info!("conversation tab selected for a request"; pending = diag::count(pending));
        self.with_contributions(|contributions| {
            contributions.dispatch(WorkspaceCommand::SelectInspectorTab(Name::Borrowed(TAB_ID)));
        });
    }

    /// Runs on the terminal's thread: only forwards what changes the state
    /// of a terminal or its picture.
    fn on_terminal(&self, event: &TerminalEvent) {
        if matches!(
            event,
            TerminalEvent::Listed { .. } | TerminalEvent::Frame { .. }
        ) {
            self.broadcast(Job::Terminal(event.clone()));
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

/// The plugin "conversation-terminal": requires `dyn Terminals` and
/// `dyn Contributions`. It brings the terminal service into [`Shared`] (the
/// Terminal page and the reply box use it), registers the terminal's key
/// bindings and the leave guard, and forwards the terminal's events.
pub struct TerminalPagePlugin {
    shared: Arc<Shared>,
}

/// The plugins, to be put in the `PluginSet` by `app`: "conversation",
/// "conversation-live" and "conversation-terminal". They share one
/// [`Shared`]. Does no IO.
pub fn plugins() -> (ConversationPlugin, LivePlugin, TerminalPagePlugin) {
    let shared = Arc::new(Shared::new());
    (
        ConversationPlugin {
            shared: shared.clone(),
        },
        LivePlugin {
            shared: shared.clone(),
        },
        TerminalPagePlugin { shared },
    )
}

impl ConversationPlugin {
    /// The state the plugins share (tests read the token that the Terminal
    /// page hands to the pane).
    #[cfg(test)]
    pub(crate) fn shared(&self) -> Arc<Shared> {
        self.shared.clone()
    }
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
        build: Arc::new(move |_, window: &mut Window, cx: &mut App| {
            let shared = build_shared.clone();
            cx.new(|cx| ConversationTab::new(shared, window, cx)).into()
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
    pub(crate) fn with_contributions<R>(
        &self,
        f: impl FnOnce(&dyn Contributions) -> R,
    ) -> Option<R> {
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
            // The sessions that wait already are not "new" when the first
            // change comes.
            if let Some(admitted) = conversations.get() {
                *lock(&self.shared.waiting) = admitted.waiting();
            }
            *lock(&self.shared.conversations) = Some(conversations);
            self.shared.broadcast(Job::Services);
        }
    }
}

impl Drop for LiveActive {
    fn drop(&mut self) {
        let had = lock(&self.shared.conversations).take().is_some();
        *lock(&self.shared.waiting) = Arc::from([]);
        if had {
            self.shared.broadcast(Job::Services);
        }
    }
}

impl Plugin for TerminalPagePlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: Vec::new(),
            requires: vec![service::<dyn Terminals>(), service::<dyn Contributions>()],
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        let terminals = ctx.service::<dyn Terminals>();
        let contributions = ctx.service::<dyn Contributions>();
        let Some(admitted) = contributions.get() else {
            return Err(PluginError::DependencyUnavailable);
        };
        // The key bindings and the guard come and go with the terminal: no
        // `dyn Terminals`, no key to bind and nothing to ask about.
        ctx.own_staged(admitted.register(
            Contribution {
                kind: ContributionKind::KeyBindings,
                id: Name::Borrowed(TERMINAL_KEY_BINDINGS_ID),
                order: 0,
                payload: KeyBindings(lectern_terminal_view::key_bindings()).into_payload(),
            },
            None,
        ));
        let guard = TerminalGuard::new(terminals.clone(), self.shared.clone());
        ctx.own_staged(admitted.register_leave_guard(Arc::new(guard), None));
        let shared = self.shared.clone();
        ctx.on::<TerminalEvent>(move |event, _| shared.on_terminal(event));
        Ok(Box::new(TerminalActive {
            shared: self.shared.clone(),
            terminals: Some(terminals),
        }))
    }
}

struct TerminalActive {
    shared: Arc<Shared>,
    terminals: Option<ServiceRef<dyn Terminals>>,
}

impl Active for TerminalActive {
    /// The service and the handle are written in `started`: `activate` has
    /// no side effects.
    fn started(&mut self, handle: PluginHandle) {
        if let Some(terminals) = self.terminals.take() {
            *lock(&self.shared.terminal_handle) = Some(handle);
            *lock(&self.shared.terminals) = Some(terminals);
            self.shared.broadcast(Job::Services);
        }
    }
}

impl Drop for TerminalActive {
    fn drop(&mut self) {
        let had = lock(&self.shared.terminals).take().is_some();
        *lock(&self.shared.terminal_handle) = None;
        // The task that shows the guard's confirmations stays (it serves the
        // window until the window or a newer page ends it); the guard itself
        // goes with the registration.
        if had {
            self.shared.broadcast(Job::Services);
        }
    }
}
