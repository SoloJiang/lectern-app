//! The rig of the GPUI tests: `lectern_ui::init_fixed`, a real kernel with
//! this crate's plugin, the fake services, a test window, and the screen
//! built through the factory the plugin registered.

use std::any::Any;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::ProgressTab;
use gpui::{
    AnyView, AnyWindowHandle, App, Context, Entity, InteractiveElement, IntoElement, KeyBinding,
    KeyBindingContextPredicate, ParentElement, Render, Styled, TestAppContext, VisualTestContext,
    Window, WindowHandle, actions, div,
};
use lectern_executor::Executor;
use lectern_i18n::{Locale, Strings};
use lectern_kernel::testing::TestKernel;
use lectern_kernel::{ActivationToken, Name};
use lectern_services::code_intel::CodeIntel;
use lectern_services::reader::Reader;
use lectern_services::review::{Review, ReviewSnapshot, ReviewTarget, RowsKey};
use lectern_services::session::{
    Batch, Content, ContentRef, Cursor, ResumeCommand, SessionError, SessionKey, SessionList,
    SessionSource, SessionSummary, SourceId, TurnIndex,
};
use lectern_services::usage::Usage;
use lectern_services::workspace::{
    CommandHandler, Contribution, ContributionKind, Contributions, LeaveGuard, Milestone,
    Registration, SidebarEntry, Target, WorkspaceCommand,
};
use lectern_ui::contrib::{InspectorTab, KeyBindings, Screen};
use lectern_ui::semantic::{Record, Recorder};
use lectern_ui::{Appearance, init_fixed};

use crate::testing::{self, FIRST_ID, FakeReview};
use crate::tests::fakes::{FakeCodeIntel, FakeReader, FakeUsage};
use crate::{ReviewModel, ReviewScreen, plugins};

/// `dyn Contributions` that remembers what it was given.
#[derive(Default)]
pub struct FakeContributions {
    registered: Mutex<Vec<String>>,
    screen: Mutex<Option<Screen>>,
    tab: Mutex<Option<InspectorTab>>,
    keys: Mutex<Vec<(String, String, Option<String>)>>,
    dispatched: Mutex<Vec<WorkspaceCommand>>,
    milestones: Mutex<Vec<Milestone>>,
    released: Arc<AtomicUsize>,
    bindings: Mutex<Option<KeyBindings>>,
}

impl FakeContributions {
    pub fn registered(&self) -> Vec<String> {
        self.registered.lock().unwrap().clone()
    }

    pub fn released(&self) -> usize {
        self.released.load(Ordering::SeqCst)
    }

    pub fn screen(&self) -> Screen {
        self.screen
            .lock()
            .unwrap()
            .clone()
            .expect("a screen was registered")
    }

    pub fn tab(&self) -> InspectorTab {
        self.tab
            .lock()
            .unwrap()
            .clone()
            .expect("a tab was registered")
    }

    pub fn dispatched(&self) -> Vec<WorkspaceCommand> {
        self.dispatched.lock().unwrap().clone()
    }

    pub fn clear_dispatched(&self) {
        self.dispatched.lock().unwrap().clear();
    }

    pub fn milestones(&self) -> Vec<Milestone> {
        self.milestones.lock().unwrap().clone()
    }

    /// The registered bindings: keystrokes, action name, context.
    pub fn keys(&self) -> Vec<(String, String, Option<String>)> {
        self.keys.lock().unwrap().clone()
    }

    /// Binds the registered keys in the app, as `workspace` does.
    pub fn bind(&self, cx: &mut App) {
        let Some(bindings) = self.bindings.lock().unwrap().take() else {
            return;
        };
        let mut loaded = Vec::new();
        for (keystrokes, action, context) in bindings.0 {
            let predicate = context.map(|context| {
                Rc::new(KeyBindingContextPredicate::parse(context).expect("a valid predicate"))
            });
            loaded.push(
                KeyBinding::load(
                    keystrokes,
                    action,
                    predicate,
                    false,
                    None,
                    cx.keyboard_mapper().as_ref(),
                )
                .expect("a valid binding"),
            );
        }
        cx.bind_keys(loaded);
    }
}

impl Contributions for FakeContributions {
    fn register(&self, contribution: Contribution, _: Option<ActivationToken>) -> Registration {
        self.registered.lock().unwrap().push(format!(
            "{}:{}",
            match contribution.kind {
                ContributionKind::Screen(_) => "screen",
                ContributionKind::InspectorTab => "inspector-tab",
                ContributionKind::KeyBindings => "key-bindings",
                _ => "other",
            },
            contribution.id
        ));
        let payload: Box<dyn Any + Send> = contribution.payload.0;
        let payload = match payload.downcast::<Screen>() {
            Ok(screen) => {
                *self.screen.lock().unwrap() = Some(*screen);
                return self.registration();
            }
            Err(payload) => payload,
        };
        let payload = match payload.downcast::<InspectorTab>() {
            Ok(tab) => {
                *self.tab.lock().unwrap() = Some(*tab);
                return self.registration();
            }
            Err(payload) => payload,
        };
        if let Ok(bindings) = payload.downcast::<KeyBindings>() {
            let mut keys = self.keys.lock().unwrap();
            for (keystrokes, action, context) in &bindings.0 {
                keys.push((
                    (*keystrokes).to_owned(),
                    action.name().to_owned(),
                    context.map(str::to_owned),
                ));
            }
            drop(keys);
            *self.bindings.lock().unwrap() = Some(*bindings);
        }
        self.registration()
    }

    fn handle_command(
        &self,
        _: Name,
        _: CommandHandler,
        _: Option<ActivationToken>,
    ) -> Registration {
        self.registration()
    }

    fn register_leave_guard(
        &self,
        _: Arc<dyn LeaveGuard>,
        _: Option<ActivationToken>,
    ) -> Registration {
        self.registration()
    }

    fn dispatch(&self, command: WorkspaceCommand) {
        self.dispatched.lock().unwrap().push(command);
    }

    fn sidebar_entries(&self) -> Arc<[SidebarEntry]> {
        Arc::from(Vec::new())
    }

    fn report_milestone(&self, milestone: Milestone) {
        self.milestones.lock().unwrap().push(milestone);
    }
}

impl FakeContributions {
    fn registration(&self) -> Registration {
        let released = self.released.clone();
        Registration::new(
            |_| {},
            move || {
                released.fetch_add(1, Ordering::SeqCst);
            },
        )
    }
}

/// One session source with a list of summaries.
pub struct FakeSessions {
    list: Mutex<Arc<SessionList>>,
}

impl FakeSessions {
    pub fn new() -> Arc<FakeSessions> {
        Arc::new(FakeSessions {
            list: Mutex::new(Arc::new(SessionList::default())),
        })
    }

    pub fn set(&self, sessions: Vec<SessionSummary>) {
        *self.list.lock().unwrap() = Arc::new(SessionList {
            generation: 1,
            sessions: Arc::from(sessions),
            diagnostics: Default::default(),
        });
    }
}

impl SessionSource for FakeSessions {
    fn id(&self) -> SourceId {
        SourceId(Arc::from("fake"))
    }

    fn agent(&self) -> lectern_services::session::Agent {
        lectern_services::session::Agent::ClaudeCode
    }

    fn sessions(&self) -> Arc<SessionList> {
        self.list.lock().unwrap().clone()
    }

    fn read(&self, _: &SessionKey, _: &Cursor, _: u64) -> Result<Batch, SessionError> {
        Err(SessionError::NotFound)
    }

    fn turns(&self, _: &SessionKey) -> Result<Arc<TurnIndex>, SessionError> {
        Err(SessionError::NotFound)
    }

    fn turns_unretained(&self, _: &SessionKey) -> Result<Arc<TurnIndex>, SessionError> {
        Err(SessionError::NotFound)
    }

    fn load(&self, _: &SessionKey, _: &ContentRef) -> Result<Content, SessionError> {
        Err(SessionError::NotFound)
    }

    fn resume(&self, _: &SessionKey) -> Option<ResumeCommand> {
        None
    }

    fn complete(&self, _: &[SessionKey]) {}
}

/// A summary with a title, for the toolbar.
pub fn summary(key: SessionKey, title: &str) -> SessionSummary {
    use lectern_services::session::{Format, Place, Timestamp};
    SessionSummary {
        key,
        agent: lectern_services::session::Agent::ClaudeCode,
        parent: None,
        file: Arc::from(std::path::Path::new("/fake/session.jsonl")),
        cwd: Arc::from(std::path::Path::new("/fake")),
        place: Place::Pending,
        started_at: Timestamp(0),
        last_activity: Timestamp(0),
        title: Some(Arc::from(title)),
        agent_version: None,
        logged_branch: None,
        open_turn: false,
        tail_pending: false,
        archived: false,
        inheritance_unknown: false,
        format: Format::Known,
    }
}

/// The window's root: the content region `workspace` would give the
/// screen, with the `Content` key context the bindings name.
pub struct Host {
    child: Option<AnyView>,
    context: &'static str,
    /// How many times the outer `ProbeEscape` was handled.
    pub probe: Arc<AtomicUsize>,
}

actions!(review_test, [ProbeEscape]);

impl Host {
    pub fn new(context: &'static str) -> Host {
        Host {
            child: None,
            context,
            probe: Arc::default(),
        }
    }

    pub fn default_for_tests() -> Host {
        Host::new("Content")
    }

    pub fn set_child(&mut self, child: AnyView) {
        self.child = Some(child);
    }

    pub fn clear_child(&mut self) {
        self.child = None;
    }
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let probe = self.probe.clone();
        div()
            .id("content")
            .key_context(self.context)
            .size_full()
            .on_action(cx.listener(move |_, _: &ProbeEscape, _, _| {
                probe.fetch_add(1, Ordering::SeqCst);
            }))
            .children(self.child.clone())
    }
}

pub struct Rig {
    /// "review-view".
    pub kernel: TestKernel,
    /// "review-view-handoff", a kernel of its own (a `TestKernel` holds one
    /// plugin); not active until `enable_handoff`.
    pub handoff: TestKernel,
    /// The highlight tasks run when the test says so (`run_tasks`).
    pub executor: Executor,
    pub review: Arc<FakeReview>,
    pub contributions: Arc<FakeContributions>,
    pub sessions: Arc<FakeSessions>,
    pub reader: Arc<FakeReader>,
    pub code_intel: Arc<FakeCodeIntel>,
    pub usage: Arc<FakeUsage>,
}

/// A screen in a test window.
pub struct Opened {
    pub window: WindowHandle<Host>,
    pub model: Entity<ReviewModel>,
}

impl Rig {
    /// The kernel with the plugin and the fakes, the plugin activated.
    pub fn new(cx: &mut TestAppContext) -> Rig {
        Rig::with_motion(cx, false)
    }

    /// The same, with the system's reduce-motion setting as given.
    pub fn with_motion(cx: &mut TestAppContext, reduced: bool) -> Rig {
        let rig = Rig::inactive_with(cx, reduced);
        rig.kernel.activate().expect("the plugin activates");
        rig.settle(cx);
        rig
    }

    /// The same, the plugin not activated yet.
    pub fn inactive(cx: &mut TestAppContext) -> Rig {
        Rig::inactive_with(cx, false)
    }

    fn inactive_with(cx: &mut TestAppContext, reduced: bool) -> Rig {
        // The kernel thread wakes the foreground task; the test scheduler
        // accepts a wake from another thread only with parking allowed.
        cx.executor().allow_parking();
        cx.update(|cx| init_fixed(cx, Strings::of(Locale::En), Appearance::Light, reduced));
        let review = FakeReview::new();
        let contributions = Arc::new(FakeContributions::default());
        let sessions = FakeSessions::new();
        let review_service: Arc<dyn Review> = review.clone();
        let contributions_service: Arc<dyn Contributions> = contributions.clone();
        let sessions_service: Arc<dyn SessionSource> = sessions.clone();
        let executor = Executor::manual();
        let (review_view, handoff_plugin) = plugins(executor.clone());
        let kernel = TestKernel::new(review_view)
            .mock::<dyn Contributions>(contributions_service)
            .mock::<dyn Review>(review_service)
            .mock::<dyn SessionSource>(sessions_service);
        let reader = Arc::new(FakeReader::default());
        let code_intel = Arc::new(FakeCodeIntel::default());
        let usage = Arc::new(FakeUsage::default());
        let reader_service: Arc<dyn Reader> = reader.clone();
        let code_intel_service: Arc<dyn CodeIntel> = code_intel.clone();
        let usage_service: Arc<dyn Usage> = usage.clone();
        let handoff = TestKernel::new(handoff_plugin)
            .mock::<dyn Reader>(reader_service)
            .mock::<dyn CodeIntel>(code_intel_service)
            .mock::<dyn Usage>(usage_service);
        Rig {
            kernel,
            handoff,
            executor,
            review,
            contributions,
            sessions,
            reader,
            code_intel,
            usage,
        }
    }

    /// Activates "review-view-handoff".
    pub fn enable_handoff(&self, cx: &mut TestAppContext) {
        self.handoff
            .activate()
            .expect("the handoff plugin activates");
        self.settle(cx);
    }

    /// Runs the highlight tasks that were submitted and delivers their
    /// results to the screens.
    pub fn run_tasks(&self, cx: &mut TestAppContext) -> usize {
        let steps = self.executor.run_until_idle();
        self.settle(cx);
        steps
    }

    /// Hands the kernel's emitter to the fake review service, so that its
    /// events go out the way the real service's do.
    pub fn connect(&self) {
        let kernel = self.kernel.inner().expect("the kernel runs");
        self.review.set_emitter(kernel.emitter());
    }

    /// Lets the kernel thread and the main-thread tasks finish.
    pub fn settle(&self, cx: &mut TestAppContext) {
        if let Some(kernel) = self.kernel.inner() {
            kernel.barrier();
        }
        if let Some(kernel) = self.handoff.inner() {
            kernel.barrier();
        }
        cx.run_until_parked();
    }

    /// Opens a review screen for `target` in a fresh window. The fake review
    /// gives it `snapshot` as its first (so the screen starts ready) unless
    /// `snapshot` is None (it starts in the loading state).
    pub fn open(
        &self,
        cx: &mut TestAppContext,
        target: ReviewTarget,
        snapshot: Option<ReviewSnapshot>,
    ) -> Opened {
        self.connect();
        cx.update(|cx| self.contributions.bind(cx));
        if let Some(mut snapshot) = snapshot {
            snapshot.id = FIRST_ID;
            snapshot.target = target.clone();
            self.review.set_snapshot(FIRST_ID, snapshot);
        }
        let screen = self.contributions.screen();
        let window = cx.add_window(|_, _| Host::new("Content"));
        let view = window
            .update(cx, |host, window, cx| {
                let view = (screen.build)(&Target::Review(target), window, cx);
                host.child = Some(view.clone());
                cx.notify();
                view
            })
            .expect("the window is open");
        let screen = view.downcast::<ReviewScreen>().expect("a review screen");
        let model = screen.read_with(cx, |screen, _| screen.model().clone());
        let _ = screen;
        let opened = Opened { window, model };
        self.settle(cx);
        opened
    }
}

/// The Progress page in a window of its own, under an `Inspector` region.
pub struct TabOpened {
    pub window: WindowHandle<Host>,
    pub tab: Entity<ProgressTab>,
}

impl Rig {
    /// Builds the inspector page through the factory the plugin registered.
    pub fn open_tab(&self, cx: &mut TestAppContext) -> TabOpened {
        cx.update(|cx| self.contributions.bind(cx));
        let tab = self.contributions.tab();
        let window = cx.add_window(|_, _| Host::new("Inspector"));
        let view = window
            .update(cx, |host, window, cx| {
                let view = (tab.build)(&(), window, cx);
                host.child = Some(view.clone());
                cx.notify();
                view
            })
            .expect("the window is open");
        let tab = view.downcast::<ProgressTab>().expect("the Progress page");
        self.settle(cx);
        TabOpened { window, tab }
    }
}

impl TabOpened {
    pub fn visual(&self, cx: &mut TestAppContext) -> VisualTestContext {
        VisualTestContext::from_window(AnyWindowHandle::from(self.window), cx)
    }

    pub fn draw(&self, cx: &mut TestAppContext) {
        let mut visual = self.visual(cx);
        for _ in 0..2 {
            visual.update(|window, cx| window.draw(cx).clear(cx));
            visual.run_until_parked();
        }
    }

    pub fn record(&self, cx: &mut TestAppContext) -> Vec<Record> {
        self.draw(cx);
        let mut visual = self.visual(cx);
        let id = visual.update(|window, _| window.window_handle().window_id());
        Recorder::begin(id);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        Recorder::finish()
    }
}

impl Opened {
    pub fn visual(&self, cx: &mut TestAppContext) -> VisualTestContext {
        VisualTestContext::from_window(AnyWindowHandle::from(self.window), cx)
    }

    /// Draws one frame (twice: the list measures what it lays out, and the
    /// view reads that on its next render).
    pub fn draw(&self, cx: &mut TestAppContext) {
        let mut visual = self.visual(cx);
        for _ in 0..2 {
            visual.update(|window, cx| window.draw(cx).clear(cx));
            visual.run_until_parked();
        }
    }

    /// One frame with the recorder armed: the semantic records.
    pub fn record(&self, cx: &mut TestAppContext) -> Vec<Record> {
        self.draw(cx);
        let mut visual = self.visual(cx);
        let id = visual.update(|window, _| window.window_handle().window_id());
        Recorder::begin(id);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        Recorder::finish()
    }

    pub fn read<R>(&self, cx: &mut TestAppContext, f: impl FnOnce(&ReviewModel) -> R) -> R {
        self.model.read_with(cx, |model, _| f(model))
    }

    pub fn update<R>(
        &self,
        cx: &mut TestAppContext,
        f: impl FnOnce(&mut ReviewModel, &mut Context<ReviewModel>) -> R,
    ) -> R {
        self.model.update(cx, |model, cx| f(model, cx))
    }

    /// Lets time pass for the test executor's timers.
    pub fn advance(&self, cx: &mut TestAppContext, by: Duration) {
        cx.executor().advance_clock(by);
        cx.run_until_parked();
    }
}

/// A rig with the fixture review open and every rows entry set; the screen
/// is drawn. `auto` makes `Prepare` answer with `Prepared`.
pub fn ready(
    cx: &mut TestAppContext,
    files: u32,
    turns: u32,
    rows: u32,
) -> (
    Rig,
    Opened,
    Vec<(RowsKey, lectern_services::review::FileRows)>,
) {
    let rig = Rig::new(cx);
    let (snapshot, all_rows) = testing::snapshot_fixture(files, turns, rows);
    rig.review.set_auto_prepare(true);
    for (key, rows) in all_rows.clone() {
        rig.review.set_rows(FIRST_ID, key, rows);
    }
    for turn in 0..turns {
        rig.review
            .set_text(FIRST_ID, testing::prompt_key(turn), "Make the change.");
        rig.review.set_text(
            FIRST_ID,
            testing::agent_key(turn),
            "Done: the files are changed.",
        );
    }
    let target = snapshot.target.clone();
    let opened = rig.open(cx, target, Some(snapshot));
    opened.draw(cx);
    rig.settle(cx);
    opened.draw(cx);
    (rig, opened, all_rows)
}
