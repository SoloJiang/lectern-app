//! The "review-view" plugin and the state it shares with the screens
//! (docs/design/review_view.md「接口」「插件」). `activate` only takes the
//! services, registers the three contributions and subscribes; nothing is
//! read, submitted or drawn before a screen is pushed (ADR 0017).

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use futures::channel::mpsc;
use gpui::{App, AppContext, Window};
use lectern_executor::{Executor, Priority};
use lectern_kernel::{
    Activation, Active, Context, Manifest, Name, Plugin, PluginError, PluginHandle, ServiceRef,
    ServiceSet, service,
};
use lectern_logging::{debug, warn};
use lectern_services::code_intel::CodeIntel;
use lectern_services::reader::Reader;
use lectern_services::review::{Review, ReviewEvent, ReviewId};
use lectern_services::session::{SessionKey, SessionSource, SessionSummary};
use lectern_services::usage::{Usage, UsageEvent, UsageQuery};
use lectern_services::workspace::{
    Contribution, ContributionKind, Contributions, Layer, Milestone, ScreenLayout, Target,
    Visibility, WorkspaceEvent, screens,
};
use lectern_syntax::WindowSpans;
use lectern_ui::contrib::{InspectorTab, KeyBindings, Screen};

use crate::actions;
use crate::diag;
use crate::highlight::SpanKey;
use crate::items::DiffMode;
use crate::progress_tab::ProgressTab;
use crate::screen;

/// The ID of "review-view" in the `PluginSet`.
pub const PLUGIN_ID: &str = "review-view";

/// The ID of "review-view-handoff" in the `PluginSet`.
pub const HANDOFF_ID: &str = "review-view-handoff";

/// The ID of "review-view-usage" in the `PluginSet`.
pub const USAGE_ID: &str = "review-view-usage";

/// The ID of the inspector page of the Progress list.
pub const FILES_TAB: &str = "files";

/// The review screen's layout (docs/design/review_view.md「界面」).
pub const LAYOUT: ScreenLayout = ScreenLayout {
    sidebar: Visibility::Hidden,
    back_button: true,
    reading_column: true,
    inspector: true,
    floating_bar: true,
};

/// What listeners on other threads send to a screen's foreground task.
#[derive(Clone, Debug)]
pub(crate) enum Job {
    Review(ReviewEvent),
    /// A panel or overlay opened or closed.
    Layer {
        open: bool,
    },
    /// `UsageEvent::Changed` of a query: the screen that watches it reads the
    /// snapshot again.
    Usage {
        query: UsageQuery,
    },
    /// The handoff services (the reader and the code navigation) arrived or
    /// left.
    Handoff,
    /// The usage service arrived or left.
    UsageService,
    /// A highlight task finished: the spans of a window.
    Spans {
        key: SpanKey,
        spans: Arc<WindowSpans>,
        took_us: u64,
    },
    /// A highlight task gave no spans: the window stays plain text.
    SpansUnavailable {
        key: SpanKey,
        /// The language's grammar or query is unusable (as opposed to a
        /// timeout or a cancel).
        unavailable: bool,
        language: Arc<str>,
        bytes: u64,
        took_us: u64,
        /// The task's cancel flag: names the task, so that the answer of a
        /// task that was cancelled does not end a newer one for the window.
        task: Arc<std::sync::atomic::AtomicBool>,
    },
}

pub(crate) type JobSender = mpsc::UnboundedSender<Job>;
pub(crate) type JobReceiver = mpsc::UnboundedReceiver<Job>;

/// The two services of "review-view-handoff". Taken from `Shared` by copy (a
/// clone of the references); `ServiceRef::get` is None once a provider is
/// deactivated.
#[derive(Clone)]
pub(crate) struct Handoff {
    pub reader: ServiceRef<dyn Reader>,
    pub code_intel: ServiceRef<dyn CodeIntel>,
}

/// Shared by the plugins and every screen. `Mutex`es of options: the plugins
/// fill them when they activate and empty them when they are deactivated.
pub(crate) struct Shared {
    contributions: Mutex<Option<ServiceRef<dyn Contributions>>>,
    review: Mutex<Option<ServiceRef<dyn Review>>>,
    sessions: Mutex<Option<ServiceSet<dyn SessionSource>>>,
    /// Filled by "review-view-handoff"; the screens work without it.
    handoff: Mutex<Option<Handoff>>,
    /// Filled by "review-view-usage"; the screens work without it, and
    /// without the handoff the other way round.
    usage: Mutex<Option<ServiceRef<dyn Usage>>>,
    /// The handle of this activation of "review-view": the highlight tasks
    /// are submitted through it.
    handle: Mutex<Option<PluginHandle>>,
    executor: Executor,
    /// The languages whose unavailability was logged already (once per run).
    warned: Mutex<HashSet<String>>,
    /// The diff mode the user chose: the screens opened later start in it
    /// (not kept across runs, docs/design/review_view.md「未决问题」1).
    mode: Mutex<DiffMode>,
    /// The living screens' channels; the listeners deliver by `ReviewId`.
    screens: Mutex<Vec<(ReviewId, u64, JobSender)>>,
    /// The next registration's token.
    next_token: AtomicU64,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    fn new(executor: Executor) -> Shared {
        Shared {
            contributions: Mutex::new(None),
            review: Mutex::new(None),
            sessions: Mutex::new(None),
            handoff: Mutex::new(None),
            usage: Mutex::new(None),
            handle: Mutex::new(None),
            executor,
            warned: Mutex::new(HashSet::new()),
            mode: Mutex::new(DiffMode::Unified),
            screens: Mutex::new(Vec::new()),
            next_token: AtomicU64::new(0),
        }
    }

    /// Whether the handoff services are there (their plugin is active).
    pub(crate) fn has_handoff(&self) -> bool {
        lock(&self.handoff).is_some()
    }

    /// Whether the usage service is there (its plugin is active).
    pub(crate) fn has_usage(&self) -> bool {
        lock(&self.usage).is_some()
    }

    pub(crate) fn with_reader<R>(&self, f: impl FnOnce(&dyn Reader) -> R) -> Option<R> {
        let reader = lock(&self.handoff).as_ref()?.reader.clone();
        let admitted = reader.get()?;
        Some(f(&*admitted))
    }

    pub(crate) fn with_code_intel<R>(&self, f: impl FnOnce(&dyn CodeIntel) -> R) -> Option<R> {
        let code_intel = lock(&self.handoff).as_ref()?.code_intel.clone();
        let admitted = code_intel.get()?;
        Some(f(&*admitted))
    }

    pub(crate) fn with_usage<R>(&self, f: impl FnOnce(&dyn Usage) -> R) -> Option<R> {
        let usage = lock(&self.usage).as_ref()?.clone();
        let admitted = usage.get()?;
        Some(f(&*admitted))
    }

    /// Runs a highlight task on the executor, as `Priority::Interactive`,
    /// holding a `BusyGuard` until it is done. False when the plugin is not
    /// started or is gone: nothing runs.
    pub(crate) fn submit(&self, work: impl FnOnce() + Send + 'static) -> bool {
        let Some(handle) = lock(&self.handle).clone() else {
            return false;
        };
        let Some(guard) = handle.busy() else {
            return false;
        };
        let executor = self.executor.clone();
        handle.own_with(move |gate| {
            executor.run_gated(&gate, Priority::Interactive, move || {
                let _guard = guard;
                work();
            })
        })
    }

    pub(crate) fn mode(&self) -> DiffMode {
        *lock(&self.mode)
    }

    pub(crate) fn set_mode(&self, mode: DiffMode) {
        *lock(&self.mode) = mode;
    }

    /// True the first time it is asked about `language` in this run.
    pub(crate) fn first_warning(&self, language: &str) -> bool {
        lock(&self.warned).insert(language.to_owned())
    }

    /// The summary of a session in the sessions list, None if no source
    /// knows it.
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

    pub(crate) fn with_review<R>(&self, f: impl FnOnce(&dyn Review) -> R) -> Option<R> {
        let review = lock(&self.review).clone()?;
        let admitted = review.get()?;
        Some(f(&*admitted))
    }

    pub(crate) fn with_contributions<R>(
        &self,
        f: impl FnOnce(&dyn Contributions) -> R,
    ) -> Option<R> {
        let contributions = lock(&self.contributions).clone()?;
        let admitted = contributions.get()?;
        Some(f(&*admitted))
    }

    /// The title of a session in the sessions list, None if no source knows it.
    pub(crate) fn session_title(&self, key: &SessionKey) -> Option<Arc<str>> {
        self.session_summary(key)?.title
    }

    pub(crate) fn report_milestone(&self, milestone: Milestone) {
        self.with_contributions(|contributions| contributions.report_milestone(milestone));
    }

    pub(crate) fn dispatch(&self, command: lectern_services::workspace::WorkspaceCommand) {
        self.with_contributions(|contributions| contributions.dispatch(command));
    }

    /// Registers a screen's channel. Called before the screen reads its first
    /// snapshot, so an event sent before the read is not lost: the snapshot
    /// read afterwards is at least that generation.
    pub(crate) fn register_screen(&self, id: ReviewId) -> (u64, JobSender, JobReceiver) {
        let (tx, rx) = mpsc::unbounded();
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        lock(&self.screens).push((id, token, tx.clone()));
        (token, tx, rx)
    }

    /// Two screens may show the same review (`open` gives the same ID): the
    /// token says which channel goes.
    pub(crate) fn unregister_screen(&self, id: ReviewId, token: u64) {
        let mut screens = lock(&self.screens);
        if let Some(at) = screens
            .iter()
            .position(|(screen, t, _)| *screen == id && *t == token)
        {
            screens.remove(at);
        }
    }

    /// Runs on the thread that emitted the event: only forwards.
    fn on_review(&self, event: &ReviewEvent) {
        let id = match event {
            ReviewEvent::Changed { id, .. } | ReviewEvent::Prepared { id, .. } => *id,
            ReviewEvent::Overview { .. } | ReviewEvent::Worktrees { .. } => return,
        };
        let screens = lock(&self.screens);
        for (screen, _, tx) in screens.iter() {
            if *screen == id {
                // The receiver is gone only with the screen: nothing to do.
                let _ = tx.unbounded_send(Job::Review(event.clone()));
            }
        }
    }

    fn on_workspace(&self, event: &WorkspaceEvent) {
        let WorkspaceEvent::LayerChanged { layer, open, .. } = event else {
            return;
        };
        if !matches!(layer, Layer::Panel(_) | Layer::Overlay(_)) {
            return;
        }
        let screens = lock(&self.screens);
        for (_, _, tx) in screens.iter() {
            let _ = tx.unbounded_send(Job::Layer { open: *open });
        }
    }

    /// Runs on the thread that emitted the event: only forwards. Every
    /// screen gets it; the one that watches the query reads the snapshot.
    fn on_usage(&self, event: &UsageEvent) {
        let UsageEvent::Changed { query, .. } = event else {
            return;
        };
        let screens = lock(&self.screens);
        for (_, _, tx) in screens.iter() {
            let _ = tx.unbounded_send(Job::Usage {
                query: query.clone(),
            });
        }
    }

    fn handoff_changed(&self) {
        let screens = lock(&self.screens);
        for (_, _, tx) in screens.iter() {
            let _ = tx.unbounded_send(Job::Handoff);
        }
    }

    fn usage_changed(&self) {
        let screens = lock(&self.screens);
        for (_, _, tx) in screens.iter() {
            let _ = tx.unbounded_send(Job::UsageService);
        }
    }

    fn clear(&self) {
        *lock(&self.contributions) = None;
        *lock(&self.review) = None;
        *lock(&self.sessions) = None;
        *lock(&self.handle) = None;
    }
}

/// The plugin "review-view": requires `dyn Contributions`, `dyn Review` and
/// `dyn SessionSource`, activates after the first frame. `app` builds it with
/// [`plugin`] and puts it in the `PluginSet`.
pub struct ReviewViewPlugin {
    shared: Arc<Shared>,
}

/// The three plugins, to be put in the `PluginSet` by `app`: "review-view",
/// "review-view-handoff" and "review-view-usage". They share one [`Shared`].
/// `executor` is the process's, for the highlight tasks. Does no IO.
pub fn plugins(executor: Executor) -> (ReviewViewPlugin, HandoffPlugin, ReviewViewUsagePlugin) {
    let shared = Arc::new(Shared::new(executor));
    (
        ReviewViewPlugin {
            shared: shared.clone(),
        },
        HandoffPlugin {
            shared: shared.clone(),
        },
        ReviewViewUsagePlugin { shared },
    )
}

impl Plugin for ReviewViewPlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: Vec::new(),
            requires: vec![
                service::<dyn Contributions>(),
                service::<dyn Review>(),
                service::<dyn SessionSource>(),
            ],
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        let contributions = ctx.service::<dyn Contributions>();
        let review = ctx.service::<dyn Review>();
        let sessions = ctx.service_set::<dyn SessionSource>();
        *lock(&self.shared.contributions) = Some(contributions.clone());
        *lock(&self.shared.review) = Some(review);
        *lock(&self.shared.sessions) = Some(sessions);
        // From here on a failure must not leave the services in `Shared`:
        // the guard clears them unless the activation goes through.
        let active = ReviewViewActive {
            shared: self.shared.clone(),
        };
        let Some(admitted) = contributions.get() else {
            return Err(PluginError::new("the contributions provider is not live"));
        };
        for registration in registrations(&self.shared) {
            ctx.own_staged(admitted.register(registration, None));
        }
        let shared = self.shared.clone();
        ctx.on::<ReviewEvent>(move |event, _| shared.on_review(event));
        let shared = self.shared.clone();
        ctx.on::<WorkspaceEvent>(move |event, _| shared.on_workspace(event));
        Ok(Box::new(active))
    }
}

/// The three contributions (docs/design/review_view.md「向 workspace 登记的贡献」).
fn registrations(shared: &Arc<Shared>) -> Vec<Contribution> {
    let build_shared = shared.clone();
    let screen_payload = Screen {
        build: Arc::new(move |target: &Target, window: &mut Window, cx: &mut App| {
            screen::build(&build_shared, target, window, cx)
        }),
        toolbar: Arc::new(screen::toolbar),
    };
    let tab_payload = InspectorTab {
        label: lectern_i18n::keys::review::PROGRESS_TITLE,
        build: Arc::new(|_, _, cx: &mut App| cx.new(ProgressTab::new).into()),
        screen: None,
    };
    vec![
        Contribution {
            kind: ContributionKind::Screen(LAYOUT),
            id: Name::Borrowed(screens::REVIEW),
            order: 0,
            payload: screen_payload.into_payload(),
        },
        Contribution {
            kind: ContributionKind::InspectorTab,
            id: Name::Borrowed(FILES_TAB),
            order: 0,
            payload: tab_payload.into_payload(),
        },
        Contribution {
            kind: ContributionKind::KeyBindings,
            id: Name::Borrowed(actions::KEY_BINDINGS_ID),
            order: 0,
            payload: KeyBindings(actions::key_bindings()).into_payload(),
        },
    ]
}

struct ReviewViewActive {
    shared: Arc<Shared>,
}

impl Active for ReviewViewActive {
    fn started(&mut self, handle: PluginHandle) {
        *lock(&self.shared.handle) = Some(handle);
        debug!("review view started"; screens = 0u32);
    }
}

impl Drop for ReviewViewActive {
    /// Deactivation: the screens still showing are popped by `workspace`;
    /// the services leave `Shared` so that a late call finds none.
    fn drop(&mut self) {
        let open = lock(&self.shared.screens).len();
        if open > 0 {
            warn!("review view deactivated with screens open"; screens = diag::count(open));
        }
        self.shared.clear();
    }
}

/// The plugin "review-view-handoff": requires `dyn Reader` and
/// `dyn CodeIntel`, activates after the first frame. The review screens work
/// without it; with it they hand off to the reader and the code navigation
/// (docs/design/review_view.md「接口」「插件」).
pub struct HandoffPlugin {
    shared: Arc<Shared>,
}

impl Plugin for HandoffPlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: Vec::new(),
            requires: vec![service::<dyn Reader>(), service::<dyn CodeIntel>()],
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        let handoff = Handoff {
            reader: ctx.service::<dyn Reader>(),
            code_intel: ctx.service::<dyn CodeIntel>(),
        };
        Ok(Box::new(HandoffActive {
            shared: self.shared.clone(),
            handoff: Some(handoff),
        }))
    }
}

struct HandoffActive {
    shared: Arc<Shared>,
    handoff: Option<Handoff>,
}

impl Active for HandoffActive {
    fn started(&mut self, _: PluginHandle) {
        if let Some(handoff) = self.handoff.take() {
            *lock(&self.shared.handoff) = Some(handoff);
            self.shared.handoff_changed();
        }
    }
}

impl Drop for HandoffActive {
    fn drop(&mut self) {
        let had = lock(&self.shared.handoff).take().is_some();
        if had {
            self.shared.handoff_changed();
        }
    }
}

/// The plugin "review-view-usage": requires `dyn Usage` only, activates after
/// the first frame. The review screens work without it; with it they show the
/// session's usage in the brief and in the turn titles. It does not depend
/// on "review-view-handoff", nor the other way round (docs/design/review_view.md
/// 「接口」「插件」), the way `ProjectHomeUsagePlugin` and `WorktreesUsagePlugin`
/// stand apart from their screens.
pub struct ReviewViewUsagePlugin {
    shared: Arc<Shared>,
}

impl Plugin for ReviewViewUsagePlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: Vec::new(),
            requires: vec![service::<dyn Usage>()],
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    /// Takes the service reference and subscribes. `Shared` is written in
    /// `started`: `activate` has no side effects.
    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        let usage = ctx.service::<dyn Usage>();
        let shared = self.shared.clone();
        ctx.on::<UsageEvent>(move |event, _| shared.on_usage(event));
        Ok(Box::new(UsageActive {
            shared: self.shared.clone(),
            usage: Some(usage),
        }))
    }
}

struct UsageActive {
    shared: Arc<Shared>,
    usage: Option<ServiceRef<dyn Usage>>,
}

impl Active for UsageActive {
    fn started(&mut self, _: PluginHandle) {
        if let Some(usage) = self.usage.take() {
            *lock(&self.shared.usage) = Some(usage);
            self.shared.usage_changed();
        }
    }
}

impl Drop for UsageActive {
    /// The screens let go of their watches when they hear of it.
    fn drop(&mut self) {
        let had = lock(&self.shared.usage).take().is_some();
        if had {
            self.shared.usage_changed();
        }
    }
}
