//! Test doubles and fixtures (docs/design/conversation.md「测试」).
//!
//! The services are fakes of the `services` traits that hand out what a test
//! constructs and record what the plugins do. `read`, `turns`, `load` and the
//! other calls this crate must not make are `unreachable!`. The other plugins'
//! crates are not dependencies, not even as dev-dependencies (architecture
//! rule 7), so nothing here comes from their `test-support`. Every name, path
//! and text is synthetic.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use lectern_kernel::{
    Activation, ActivationToken, Active, Context, Kernel, Manifest, Name, Plugin, PluginError,
    PluginSet, Service, service,
};
use lectern_services::conversation::{
    CallKind, CallState, ConversationCommand, ConversationEvent, ConversationInterest,
    ConversationSnapshot, Conversations, Entry, EntryBody, EntryKey, Link, Load, Passage,
    PromptOrigin, ToolEntry,
};
use lectern_services::project::ProjectKey;
use lectern_services::review::ReviewTarget;
use lectern_services::session::{
    Agent, Batch, Content, ContentRef, Cursor, Format, ResumeCommand, SessionError, SessionKey,
    SessionList, SessionSource, SessionSummary, SourceId, Timestamp, TurnIndex,
};
use lectern_services::terminal::{
    Frame, FrameInterest, GridSize, Input, Launch, LaunchError, LaunchKind, OpenSpec, RunState,
    TerminalEvent, TerminalId, TerminalInfo, TerminalList, Terminals, Theme,
};
use lectern_services::workspace::{
    CommandHandler, Contribution, ContributionKind, Contributions, LeaveGuard, Milestone, Payload,
    Registration, Route, SidebarEntry, Target, WindowId, WorkspaceCommand, WorkspaceEvent, screens,
};

use crate::plugin::{
    ConversationPlugin, LIVE_ID, LivePlugin, PLUGIN_ID, Shared, TERMINAL_ID, TerminalPagePlugin,
    plugins,
};

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ----------------------------------------------------------------- fixtures

/// The ID of the fake session source.
pub(crate) const SOURCE: &str = "fake";

/// A session of the fake source (a UUID, as the real ones are).
pub(crate) fn session(n: u32) -> SessionKey {
    SessionKey {
        source: SourceId(Arc::from(SOURCE)),
        id: Arc::from(format!("00000000-0000-4000-8000-{n:012}")),
    }
}

/// The folder the fake sessions ran in.
pub(crate) const CWD: &str = "/work/app";

/// A summary in the sessions list.
pub(crate) fn summary(key: &SessionKey) -> SessionSummary {
    SessionSummary {
        key: key.clone(),
        agent: Agent::ClaudeCode,
        parent: None,
        file: Arc::from(Path::new("/logs/session.jsonl")),
        cwd: Arc::from(Path::new(CWD)),
        place: lectern_services::session::Place::Pending,
        started_at: Timestamp(1_000),
        last_activity: Timestamp(2_000),
        title: Some(Arc::from("A synthetic session")),
        agent_version: None,
        logged_branch: None,
        open_turn: false,
        tail_pending: false,
        archived: false,
        inheritance_unknown: false,
        format: Format::Known,
    }
}

pub(crate) fn passage(text: &str) -> Passage {
    Passage {
        text: Arc::from(text),
        truncated: false,
    }
}

/// An empty, ready snapshot with no connection.
pub(crate) fn snapshot(key: &SessionKey, generation: u64) -> ConversationSnapshot {
    ConversationSnapshot {
        session: key.clone(),
        generation,
        agent: Agent::ClaudeCode,
        load: Load::Ready,
        link: Link::None,
        can_prompt: Ok(()),
        context: None,
        earlier: 0,
        entries: Arc::from([]),
        plan: None,
        permissions: Arc::from([]),
    }
}

/// `snapshot` with entries.
pub(crate) fn with_entries(
    key: &SessionKey,
    generation: u64,
    entries: Vec<Entry>,
) -> ConversationSnapshot {
    ConversationSnapshot {
        entries: Arc::from(entries),
        ..snapshot(key, generation)
    }
}

pub(crate) fn entry_key(text: &str) -> EntryKey {
    EntryKey(Arc::from(text))
}

pub(crate) fn prompt(key: &str, text: &str) -> Entry {
    Entry {
        key: entry_key(key),
        body: EntryBody::Prompt {
            text: passage(text),
            origin: PromptOrigin::User,
        },
    }
}

pub(crate) fn agent_text(key: &str, text: &str, streaming: bool) -> Entry {
    Entry {
        key: entry_key(key),
        body: EntryBody::AgentText {
            text: passage(text),
            streaming,
        },
    }
}

pub(crate) fn tool(key: &str, kind: CallKind, summary: &str, state: CallState) -> Entry {
    Entry {
        key: entry_key(key),
        body: EntryBody::Tool(ToolEntry {
            kind,
            state,
            summary: Arc::from(summary),
        }),
    }
}

// -------------------------------------------------------------------- fakes

/// One source of sessions: the sessions list, nothing else.
pub(crate) struct FakeSessions {
    list: Mutex<Arc<SessionList>>,
}

impl FakeSessions {
    pub fn new(sessions: Vec<SessionSummary>) -> FakeSessions {
        FakeSessions {
            list: Mutex::new(Arc::new(SessionList {
                generation: 1,
                sessions: Arc::from(sessions),
                diagnostics: Default::default(),
            })),
        }
    }
}

impl SessionSource for FakeSessions {
    fn id(&self) -> SourceId {
        SourceId(Arc::from(SOURCE))
    }
    fn agent(&self) -> Agent {
        Agent::ClaudeCode
    }
    fn sessions(&self) -> Arc<SessionList> {
        lock(&self.list).clone()
    }
    fn read(&self, _: &SessionKey, _: &Cursor, _: u64) -> Result<Batch, SessionError> {
        unreachable!("conversation never reads a session")
    }
    fn turns(&self, _: &SessionKey) -> Result<Arc<TurnIndex>, SessionError> {
        unreachable!("conversation never reads turns")
    }
    fn turns_unretained(&self, _: &SessionKey) -> Result<Arc<TurnIndex>, SessionError> {
        unreachable!("conversation never reads turns")
    }
    fn load(&self, _: &SessionKey, _: &ContentRef) -> Result<Content, SessionError> {
        unreachable!("conversation never loads content")
    }
    fn resume(&self, _: &SessionKey) -> Option<ResumeCommand> {
        unreachable!("conversation never resumes a session")
    }
    fn complete(&self, _: &[SessionKey]) {
        unreachable!("conversation never completes a session")
    }
}

/// What a fake provider does when it gets a command: it may swap the
/// snapshot before returning, as the real one does.
pub(crate) type Responder =
    Box<dyn Fn(&FakeConversations, &SessionKey, &ConversationCommand) + Send>;

/// The provider of `dyn Conversations`: keeps the snapshots a test sets,
/// counts the interests, records the commands.
#[derive(Default)]
pub(crate) struct FakeConversations {
    snapshots: Mutex<HashMap<SessionKey, Arc<ConversationSnapshot>>>,
    watched: AtomicUsize,
    released: Arc<AtomicUsize>,
    commands: Mutex<Vec<(SessionKey, ConversationCommand)>>,
    responder: Mutex<Option<Responder>>,
    waiting: Mutex<Arc<[SessionKey]>>,
}

impl FakeConversations {
    /// Replaces the session's snapshot (no event: the test sends it).
    pub fn set(&self, snapshot: ConversationSnapshot) {
        lock(&self.snapshots).insert(snapshot.session.clone(), Arc::new(snapshot));
    }

    pub fn get(&self, session: &SessionKey) -> Option<Arc<ConversationSnapshot>> {
        lock(&self.snapshots).get(session).cloned()
    }

    /// How many `watch` calls were made.
    pub fn watched(&self) -> usize {
        self.watched.load(Ordering::SeqCst)
    }

    /// How many interests were dropped.
    pub fn released(&self) -> usize {
        self.released.load(Ordering::SeqCst)
    }

    /// The interests that are alive.
    pub fn alive(&self) -> usize {
        self.watched().saturating_sub(self.released())
    }

    pub fn commands(&self) -> Vec<(SessionKey, ConversationCommand)> {
        lock(&self.commands).clone()
    }

    /// What `waiting()` answers from now on (no event: the test sends it).
    pub fn set_waiting(&self, sessions: Vec<SessionKey>) {
        *lock(&self.waiting) = Arc::from(sessions);
    }

    pub fn respond(
        &self,
        responder: impl Fn(&FakeConversations, &SessionKey, &ConversationCommand) + Send + 'static,
    ) {
        *lock(&self.responder) = Some(Box::new(responder));
    }
}

impl Conversations for FakeConversations {
    fn watch(&self, _: &SessionKey) -> ConversationInterest {
        self.watched.fetch_add(1, Ordering::SeqCst);
        let released = self.released.clone();
        ConversationInterest::new(move || {
            released.fetch_add(1, Ordering::SeqCst);
        })
    }
    fn snapshot(&self, session: &SessionKey) -> Option<Arc<ConversationSnapshot>> {
        self.get(session)
    }
    fn command(&self, session: &SessionKey, command: ConversationCommand) {
        lock(&self.commands).push((session.clone(), command.clone()));
        if let Some(responder) = lock(&self.responder).as_ref() {
            responder(self, session, &command);
        }
    }
    fn waiting(&self) -> Arc<[SessionKey]> {
        lock(&self.waiting).clone()
    }
}

/// What a test sees of one `Terminals::open` call.
#[derive(Clone, Debug)]
pub(crate) struct OpenCall {
    pub launch: Launch,
    pub size: Option<GridSize>,
    pub theme: Theme,
}

#[derive(Default)]
struct TerminalsState {
    terminals: Vec<TerminalInfo>,
    next: u64,
    opens: Vec<OpenCall>,
    refuse: Option<LaunchError>,
    frames: HashMap<TerminalId, Arc<Frame>>,
    frame_reads: HashMap<TerminalId, usize>,
    inputs: Vec<(TerminalId, Input)>,
    closed: Vec<TerminalId>,
    watches: usize,
    /// The activation numbers of the tokens `copy_selection` was given.
    copy_tokens: Vec<u64>,
}

/// The provider of `dyn Terminals`: the terminals a test sets, by session,
/// and the ones `open` makes (registered at once, `Starting`, as the real
/// service does). It records `open`, `watch`, `frame`, `input` and `close`.
/// A terminal's state changes only when a test says so (no event: the test
/// sends `Listed`).
#[derive(Default)]
pub(crate) struct FakeTerminals {
    state: Mutex<TerminalsState>,
    /// The interests alive.
    alive: Arc<AtomicUsize>,
}

impl FakeTerminals {
    /// Gives the session a terminal in `state` (in the project of `CWD`), or
    /// changes the state of the one it has.
    pub fn set(&self, session: &SessionKey, state: RunState) {
        self.set_in(session, folder(CWD), state);
    }

    /// The same, in a given project.
    pub fn set_in(&self, session: &SessionKey, project: ProjectKey, state: RunState) {
        let mut inner = lock(&self.state);
        let launch = LaunchKind::Resume(session.clone());
        if let Some(info) = inner
            .terminals
            .iter_mut()
            .find(|info| info.launch == launch)
        {
            info.state = state;
            return;
        }
        Self::push(&mut inner, launch, project, state);
    }

    fn push(
        inner: &mut TerminalsState,
        launch: LaunchKind,
        project: ProjectKey,
        state: RunState,
    ) -> TerminalId {
        inner.next += 1;
        let id = TerminalId(inner.next);
        inner.terminals.push(TerminalInfo {
            id,
            launch,
            project,
            cwd: Arc::from(Path::new(CWD)),
            state,
            title: None,
            opened_at: Timestamp(1_000),
            bells: 0,
            dropped_inputs: 0,
        });
        id
    }

    /// Gives a terminal that is not the resume of a session (a shell).
    pub fn set_shell(&self, project: ProjectKey, state: RunState) -> TerminalId {
        Self::push(&mut lock(&self.state), LaunchKind::Shell, project, state)
    }

    /// The title the program set (it is the program's output: a canary
    /// carrier).
    pub fn set_title(&self, id: TerminalId, title: &str) {
        if let Some(info) = lock(&self.state)
            .terminals
            .iter_mut()
            .find(|info| info.id == id)
        {
            info.title = Some(Arc::from(title));
        }
    }

    /// The next `open` answers with this error.
    pub fn refuse_next(&self, error: LaunchError) {
        lock(&self.state).refuse = Some(error);
    }

    pub fn set_frame(&self, id: TerminalId, frame: Arc<Frame>) {
        lock(&self.state).frames.insert(id, frame);
    }

    /// Every `open`, in order.
    pub fn opens(&self) -> Vec<OpenCall> {
        lock(&self.state).opens.clone()
    }

    /// The terminals closed, in order.
    pub fn closed(&self) -> Vec<TerminalId> {
        lock(&self.state).closed.clone()
    }

    /// How many times `frame(id)` was read.
    pub fn frame_reads(&self, id: TerminalId) -> usize {
        lock(&self.state).frame_reads.get(&id).copied().unwrap_or(0)
    }

    /// The activation numbers of the tokens that `copy_selection` got.
    pub fn copy_tokens(&self) -> Vec<u64> {
        lock(&self.state).copy_tokens.clone()
    }

    /// How many times `watch` was called.
    pub fn watches(&self) -> usize {
        lock(&self.state).watches
    }

    /// The interests that are alive.
    pub fn alive(&self) -> usize {
        self.alive.load(Ordering::SeqCst)
    }

    /// The ID of the terminal of a session, if it has one.
    pub fn id_of(&self, session: &SessionKey) -> Option<TerminalId> {
        self.for_session(session)
    }
}

/// A folder project at `root`.
pub(crate) fn folder(root: &str) -> ProjectKey {
    ProjectKey::Folder {
        root: Arc::from(Path::new(root)),
    }
}

impl Terminals for FakeTerminals {
    fn open(&self, spec: OpenSpec) -> Result<TerminalId, LaunchError> {
        let mut inner = lock(&self.state);
        inner.opens.push(OpenCall {
            launch: spec.launch.clone(),
            size: spec.size,
            theme: spec.theme,
        });
        if let Some(error) = inner.refuse.take() {
            return Err(error);
        }
        let Launch::Resume(session) = spec.launch else {
            unreachable!("the Terminal page only resumes sessions");
        };
        let launch = LaunchKind::Resume(session);
        if let Some(at) = inner
            .terminals
            .iter()
            .position(|info| info.launch == launch)
        {
            let info = &inner.terminals[at];
            if matches!(info.state, RunState::Starting | RunState::Running { .. }) {
                return Err(LaunchError::AlreadyRunning(info.id));
            }
            // An ended terminal is replaced.
            let old = inner.terminals.remove(at);
            inner.closed.push(old.id);
        }
        Ok(Self::push(
            &mut inner,
            launch,
            folder(CWD),
            RunState::Starting,
        ))
    }
    fn list(&self) -> Arc<TerminalList> {
        Arc::new(TerminalList {
            generation: 1,
            terminals: Arc::from(lock(&self.state).terminals.clone()),
        })
    }
    fn info(&self, id: TerminalId) -> Option<TerminalInfo> {
        lock(&self.state)
            .terminals
            .iter()
            .find(|info| info.id == id)
            .cloned()
    }
    fn for_session(&self, session: &SessionKey) -> Option<TerminalId> {
        let launch = LaunchKind::Resume(session.clone());
        lock(&self.state)
            .terminals
            .iter()
            .find(|info| info.launch == launch)
            .map(|info| info.id)
    }
    fn frame(&self, id: TerminalId) -> Option<Arc<Frame>> {
        let mut inner = lock(&self.state);
        *inner.frame_reads.entry(id).or_insert(0) += 1;
        inner.frames.get(&id).cloned()
    }
    fn watch(&self, id: TerminalId) -> Option<FrameInterest> {
        let mut inner = lock(&self.state);
        if !inner.terminals.iter().any(|info| info.id == id) {
            return None;
        }
        inner.watches += 1;
        self.alive.fetch_add(1, Ordering::SeqCst);
        let alive = self.alive.clone();
        Some(FrameInterest::new(move || {
            alive.fetch_sub(1, Ordering::SeqCst);
        }))
    }
    fn input(&self, id: TerminalId, input: Input) {
        lock(&self.state).inputs.push((id, input));
    }
    fn copy_selection(
        &self,
        _: TerminalId,
        token: lectern_kernel::ActivationToken,
        reply: Box<dyn FnOnce(Option<String>) + Send>,
    ) {
        lock(&self.state).copy_tokens.push(token.id());
        reply(None);
    }
    fn close(&self, id: TerminalId) {
        let mut inner = lock(&self.state);
        inner.terminals.retain(|info| info.id != id);
        inner.closed.push(id);
    }
}

/// The window's side of `dyn Contributions`: keeps what was registered,
/// counts what was unregistered and answers `current_route` with what a test
/// set.
#[derive(Default)]
pub(crate) struct FakeContributions {
    contributions: Mutex<Vec<(ContributionKind, String, u16, Payload)>>,
    released: Arc<AtomicUsize>,
    tokens_given: Mutex<Vec<bool>>,
    route: Mutex<Option<Route>>,
    dispatched: Mutex<Vec<WorkspaceCommand>>,
    guards: Mutex<Vec<Arc<dyn LeaveGuard>>>,
    guards_released: Arc<AtomicUsize>,
}

impl FakeContributions {
    /// The route `current_route` answers with from now on.
    pub fn set_route(&self, route: Option<Route>) {
        *lock(&self.route) = route;
    }

    /// The commands the plugins dispatched, in order.
    pub fn dispatched(&self) -> Vec<WorkspaceCommand> {
        lock(&self.dispatched).clone()
    }

    /// The kinds and IDs of the contributions, in registration order.
    pub fn kinds(&self) -> Vec<(ContributionKind, String)> {
        lock(&self.contributions)
            .iter()
            .map(|(kind, id, _, _)| (*kind, id.clone()))
            .collect()
    }

    /// The leave guards registered, in order.
    pub fn guards(&self) -> Vec<Arc<dyn LeaveGuard>> {
        lock(&self.guards).clone()
    }

    /// How many leave guards were unregistered.
    pub fn guards_released(&self) -> usize {
        self.guards_released.load(Ordering::SeqCst)
    }

    /// How many registrations were dropped.
    pub fn released(&self) -> usize {
        self.released.load(Ordering::SeqCst)
    }

    /// Whether each registration came with the activation's token.
    pub fn tokens_given(&self) -> Vec<bool> {
        lock(&self.tokens_given).clone()
    }

    /// The order of the contribution registered under `id`.
    pub fn order(&self, id: &str) -> Option<u16> {
        lock(&self.contributions)
            .iter()
            .find(|(_, registered, _, _)| registered == id)
            .map(|(_, _, order, _)| *order)
    }

    /// A clone of the payload registered under `id`, as its `ui::contrib` type.
    pub fn payload<T: Clone + 'static>(&self, id: &str) -> Option<T> {
        lock(&self.contributions)
            .iter()
            .filter(|(_, registered, _, _)| registered == id)
            .find_map(|(_, _, _, payload)| {
                let any: &(dyn std::any::Any + Send) = &*payload.0;
                any.downcast_ref::<T>().cloned()
            })
    }

    /// The keystrokes, action names and contexts of the `KeyBindings`
    /// contribution registered under `id`.
    #[allow(
        clippy::type_complexity,
        reason = "the tuple is what a test compares; a named type would be used once"
    )]
    pub fn binding_names(
        &self,
        id: &str,
    ) -> Option<Vec<(&'static str, &'static str, Option<&'static str>)>> {
        lock(&self.contributions)
            .iter()
            .filter(|(_, registered, _, _)| registered == id)
            .find_map(|(_, _, _, payload)| {
                let any: &(dyn std::any::Any + Send) = &*payload.0;
                any.downcast_ref::<lectern_ui::contrib::KeyBindings>()
                    .map(|bindings| {
                        bindings
                            .0
                            .iter()
                            .map(|(keys, action, context)| (*keys, action.name(), *context))
                            .collect()
                    })
            })
    }

    /// The key bindings registered, loaded the way `workspace` loads them.
    pub fn key_bindings(&self, cx: &gpui::App) -> Vec<gpui::KeyBinding> {
        lock(&self.contributions)
            .iter()
            .filter(|(kind, _, _, _)| matches!(kind, ContributionKind::KeyBindings))
            .flat_map(|(_, _, _, payload)| {
                let any: &(dyn std::any::Any + Send) = &*payload.0;
                let bindings = any.downcast_ref::<lectern_ui::contrib::KeyBindings>();
                bindings
                    .into_iter()
                    .flat_map(|bindings| bindings.0.iter())
                    .map(|(keys, action, context)| {
                        let predicate = context.map(|context| {
                            std::rc::Rc::new(
                                gpui::KeyBindingContextPredicate::parse(context)
                                    .expect("the binding's context parses"),
                            )
                        });
                        gpui::KeyBinding::load(
                            keys,
                            action.boxed_clone(),
                            predicate,
                            false,
                            None,
                            cx.keyboard_mapper().as_ref(),
                        )
                        .expect("the binding's keystrokes parse")
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }
}

impl Contributions for FakeContributions {
    fn register(&self, contribution: Contribution, token: Option<ActivationToken>) -> Registration {
        let Contribution {
            kind,
            id,
            order,
            payload,
        } = contribution;
        lock(&self.tokens_given).push(token.is_some());
        lock(&self.contributions).push((kind, id.to_string(), order, payload));
        let released = self.released.clone();
        Registration::new(
            |_| {},
            move || {
                released.fetch_add(1, Ordering::SeqCst);
            },
        )
    }
    fn handle_command(
        &self,
        _: Name,
        _: CommandHandler,
        _: Option<ActivationToken>,
    ) -> Registration {
        unreachable!("conversation handles no command")
    }
    fn register_leave_guard(
        &self,
        guard: Arc<dyn LeaveGuard>,
        token: Option<ActivationToken>,
    ) -> Registration {
        lock(&self.tokens_given).push(token.is_some());
        lock(&self.guards).push(guard);
        let released = self.guards_released.clone();
        Registration::new(
            |_| {},
            move || {
                released.fetch_add(1, Ordering::SeqCst);
            },
        )
    }
    fn dispatch(&self, command: WorkspaceCommand) {
        lock(&self.dispatched).push(command);
    }
    fn sidebar_entries(&self) -> Arc<[SidebarEntry]> {
        unreachable!("conversation never lists the sidebar entries")
    }
    fn current_route(&self, _window: WindowId) -> Option<Route> {
        lock(&self.route).clone()
    }
    fn report_milestone(&self, _: Milestone) {
        unreachable!("conversation reports no milestone")
    }
}

// ---------------------------------------------------------------------- rig

/// Provides a service from a plain plugin: the kernel of the tests holds the
/// plugins under test and these.
struct Provide<S: ?Sized>(Arc<S>);

impl<S> Plugin for Provide<S>
where
    S: Service + ?Sized + 'static,
    Arc<S>: Send + Sync,
{
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: vec![service::<S>()],
            requires: Vec::new(),
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        ctx.provide::<S>(self.0.clone())?;
        Ok(Box::new(()))
    }
}

/// A plugin that activates only through `Kernel::activate`, so that a test
/// decides the order.
struct Manual<P>(P);

impl<P: Plugin> Plugin for Manual<P> {
    fn manifest(&self) -> Manifest {
        let mut manifest = self.0.manifest();
        manifest.activation = vec![Activation::Manual];
        manifest
    }

    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        self.0.activate(ctx)
    }
}

/// A kernel with fake services and the three plugins under test (not
/// activated yet).
pub(crate) struct Rig {
    pub kernel: Arc<Kernel>,
    pub conversations: Arc<FakeConversations>,
    pub contributions: Arc<FakeContributions>,
    pub terminals: Arc<FakeTerminals>,
    /// The state the plugins share.
    pub shared: Arc<Shared>,
}

impl Rig {
    /// The sessions list holds `session(1)` to `session(3)`.
    pub fn new() -> Rig {
        Rig::with_sessions((1..=3).map(|n| summary(&session(n))).collect())
    }

    /// The sessions list holds `sessions`.
    pub fn with_sessions(sessions: Vec<SessionSummary>) -> Rig {
        let sessions = Arc::new(FakeSessions::new(sessions));
        let conversations = Arc::new(FakeConversations::default());
        let contributions = Arc::new(FakeContributions::default());
        let terminals = Arc::new(FakeTerminals::default());
        let (main, live, terminal): (ConversationPlugin, LivePlugin, TerminalPagePlugin) =
            plugins();
        let shared = main.shared();
        let set = PluginSet::new()
            .with(
                "mock-sessions",
                Provide::<dyn SessionSource>(sessions.clone()),
            )
            .with(
                "mock-conversations",
                Provide::<dyn Conversations>(conversations.clone()),
            )
            .with(
                "mock-contributions",
                Provide::<dyn Contributions>(contributions.clone()),
            )
            .with(
                "mock-terminals",
                Provide::<dyn Terminals>(terminals.clone()),
            )
            .with(PLUGIN_ID, Manual(main))
            .with(LIVE_ID, Manual(live))
            .with(TERMINAL_ID, Manual(terminal));
        let kernel = Kernel::start(set);
        kernel.barrier();
        Rig {
            kernel,
            conversations,
            contributions,
            terminals,
            shared,
        }
    }

    pub fn activate_main(&self) {
        self.kernel.activate(PLUGIN_ID);
        self.kernel.barrier();
    }

    pub fn activate_live(&self) {
        self.kernel.activate(LIVE_ID);
        self.kernel.barrier();
    }

    pub fn activate_terminal(&self) {
        self.kernel.activate(TERMINAL_ID);
        self.kernel.barrier();
    }

    pub fn deactivate_terminal(&self) {
        self.kernel.set_enabled(TERMINAL_ID, false);
        self.kernel.barrier();
    }

    pub fn deactivate_main(&self) {
        self.kernel.set_enabled(PLUGIN_ID, false);
        self.kernel.barrier();
    }

    pub fn deactivate_live(&self) {
        self.kernel.set_enabled(LIVE_ID, false);
        self.kernel.barrier();
    }

    /// The route of a session's review.
    pub fn review_route(key: &SessionKey) -> Route {
        Route {
            screen: Name::Borrowed(screens::REVIEW),
            target: Target::Review(ReviewTarget::Session(key.clone())),
        }
    }

    /// The window shows `route`, as `workspace` announces it.
    pub fn screen(&self, route: Route) {
        self.kernel.emitter().emit(WorkspaceEvent::ScreenChanged {
            window: WindowId(0),
            from: None,
            to: route,
        });
        self.kernel.barrier();
    }

    /// The window shows the review of `key`.
    pub fn review(&self, key: &SessionKey) {
        self.screen(Rig::review_route(key));
    }

    /// The window shows the project home.
    pub fn home(&self) {
        self.screen(Route {
            screen: Name::Borrowed(screens::PROJECT_HOME),
            target: Target::None,
        });
    }

    /// `WaitingChanged`, as the provider emits it.
    pub fn waiting_changed(&self) {
        self.kernel
            .emitter()
            .emit(ConversationEvent::WaitingChanged);
        self.kernel.barrier();
    }

    /// `Listed`, as the terminal service emits it.
    pub fn terminals_listed(&self) {
        self.kernel
            .emitter()
            .emit(TerminalEvent::Listed { generation: 1 });
        self.kernel.barrier();
    }

    /// `Frame` for a terminal, as the terminal service emits it.
    pub fn terminal_frame(&self, id: TerminalId, generation: u64) {
        self.kernel
            .emitter()
            .emit(TerminalEvent::Frame { id, generation });
        self.kernel.barrier();
    }

    /// `Changed` for `session`, as the provider emits it.
    pub fn changed(&self, session: &SessionKey, generation: u64) {
        self.kernel.emitter().emit(ConversationEvent::Changed {
            session: session.clone(),
            generation,
        });
        self.kernel.barrier();
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let (done, wait) = std::sync::mpsc::channel();
        self.kernel.shutdown(move || {
            let _ = done.send(());
        });
        let _ = wait.recv_timeout(std::time::Duration::new(10, 0));
    }
}
