//! The "review-view" plugin and the state it shares with the screens
//! (docs/design/review_view.md「接口」「插件」). `activate` only takes the
//! services, registers the three contributions and subscribes; nothing is
//! read, submitted or drawn before a screen is pushed (ADR 0017).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use futures::channel::mpsc;
use gpui::{App, AppContext, Window};
use lectern_kernel::{
    Activation, Active, Context, Manifest, Name, Plugin, PluginError, PluginHandle, ServiceRef,
    ServiceSet, service,
};
use lectern_logging::{debug, warn};
use lectern_services::review::{Review, ReviewEvent, ReviewId};
use lectern_services::session::{SessionKey, SessionSource};
use lectern_services::workspace::{
    Contribution, ContributionKind, Contributions, Layer, Milestone, ScreenLayout, Target,
    Visibility, WorkspaceEvent, screens,
};
use lectern_ui::contrib::{InspectorTab, KeyBindings, Screen};

use crate::actions;
use crate::diag;
use crate::progress_tab::ProgressTab;
use crate::screen;

/// The ID of "review-view" in the `PluginSet`.
pub const PLUGIN_ID: &str = "review-view";

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
}

pub(crate) type JobSender = mpsc::UnboundedSender<Job>;
pub(crate) type JobReceiver = mpsc::UnboundedReceiver<Job>;

/// Shared by the plugin and every screen. `Mutex`es of options: the plugin
/// fills them when it activates and empties them when it is deactivated.
#[derive(Default)]
pub(crate) struct Shared {
    contributions: Mutex<Option<ServiceRef<dyn Contributions>>>,
    review: Mutex<Option<ServiceRef<dyn Review>>>,
    sessions: Mutex<Option<ServiceSet<dyn SessionSource>>>,
    /// The living screens' channels; the listeners deliver by `ReviewId`.
    screens: Mutex<Vec<(ReviewId, u64, JobSender)>>,
    /// The next registration's token.
    next_token: AtomicU64,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
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
        let sessions = lock(&self.sessions).clone()?;
        let mut title = None;
        sessions.for_each(|source| {
            if title.is_none()
                && let Some(summary) = source.sessions().get(key)
            {
                title = summary.title.clone();
            }
        });
        title
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
    pub(crate) fn register_screen(&self, id: ReviewId) -> (u64, JobReceiver) {
        let (tx, rx) = mpsc::unbounded();
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        lock(&self.screens).push((id, token, tx));
        (token, rx)
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
            ReviewEvent::Overview { .. } => return,
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

    fn clear(&self) {
        *lock(&self.contributions) = None;
        *lock(&self.review) = None;
        *lock(&self.sessions) = None;
    }
}

/// The plugin "review-view": requires `dyn Contributions`, `dyn Review` and
/// `dyn SessionSource`, activates after the first frame. `app` builds it with
/// [`plugin`] and puts it in the `PluginSet`.
pub struct ReviewViewPlugin {
    shared: Arc<Shared>,
}

/// Does no IO. (The design's `plugins()` returns this and the handoff
/// plugin; the second one arrives with step 2.)
pub fn plugin() -> ReviewViewPlugin {
    ReviewViewPlugin {
        shared: Arc::new(Shared::default()),
    }
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
        let _ = handle;
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
