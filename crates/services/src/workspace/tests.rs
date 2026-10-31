use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use lectern_kernel::testing::TestKernel;
use lectern_kernel::{
    Activation, Active, Context, Event, Manifest, Plugin, PluginError, Service, TokenState, service,
};

use super::*;
use crate::code_intel::TextOrigin;
use crate::project::BlobId;
use crate::reader::Anchor;
use crate::session::{SessionKey, SourceId};

/// `Probe::<T>::IS_SERVICE` is true exactly when `T: Service`: the inherent
/// constant only applies when its bound holds, otherwise the blanket trait's
/// constant does.
struct Probe<T: ?Sized>(PhantomData<T>);

trait NotService {
    const IS_SERVICE: bool = false;
}

impl<T: ?Sized> NotService for T {}

impl<T: ?Sized + Service> Probe<T> {
    const IS_SERVICE: bool = true;
}

/// A `Contributions` that accepts everything and shows nothing, to check the
/// trait is usable as a shared trait object.
struct Empty;

impl Contributions for Empty {
    fn register(
        &self,
        _contribution: Contribution,
        _token: Option<ActivationToken>,
    ) -> Registration {
        Registration::new(|_| {}, || {})
    }
    fn handle_command(
        &self,
        _name: Name,
        _handler: CommandHandler,
        _token: Option<ActivationToken>,
    ) -> Registration {
        Registration::new(|_| {}, || {})
    }
    fn register_leave_guard(
        &self,
        _guard: Arc<dyn LeaveGuard>,
        _token: Option<ActivationToken>,
    ) -> Registration {
        Registration::new(|_| {}, || {})
    }
    fn dispatch(&self, _command: WorkspaceCommand) {}
    fn sidebar_entries(&self) -> Arc<[SidebarEntry]> {
        Arc::from(Vec::new())
    }
    fn current_route(&self, _window: WindowId) -> Option<Route> {
        None
    }
    fn report_milestone(&self, _milestone: Milestone) {}
}

/// A guard that always lets the user leave, answering from another thread.
struct Permissive;

impl LeaveGuard for Permissive {
    fn ask(&self, _scope: LeaveScope, done: Box<dyn FnOnce(bool) + Send>) {
        std::thread::spawn(move || done(true)).join().unwrap();
    }
}

/// What the staged registration of [`Staging`] observed.
#[derive(Default)]
struct Observed {
    commits: AtomicUsize,
    releases: AtomicUsize,
    /// The token `commit` received, and its state at that moment.
    token: Mutex<Option<(ActivationToken, TokenState)>>,
}

/// A plugin whose `activate` hands one `Registration` to `ctx.own_staged`,
/// the way `welcome` or `project_home` register their screens.
struct Staging(Arc<Observed>);

impl Plugin for Staging {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: Vec::new(),
            requires: Vec::new(),
            activation: vec![Activation::Manual],
        }
    }

    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        let observed = self.0.clone();
        let o = observed.clone();
        ctx.own_staged(Registration::new(
            move |token| {
                o.commits.fetch_add(1, Ordering::SeqCst);
                let state = token.state();
                *o.token.lock().unwrap() = Some((token, state));
            },
            move || {
                observed.releases.fetch_add(1, Ordering::SeqCst);
            },
        ));
        Ok(Box::new(()))
    }
}

fn path(p: &str) -> Arc<Path> {
    Arc::from(PathBuf::from(p))
}

fn text(s: &str) -> Arc<str> {
    Arc::from(s)
}

fn name(s: &str) -> Name {
    Name::Owned(s.to_owned())
}

fn shareable<T: Send + Sync + 'static>() {}

fn sendable<T: Send + 'static>() {}

/// Serializes to JSON (the process plugin protocol is JSON-RPC, ADR 0018)
/// and back.
fn round_trip<T>(value: &T) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let json = serde_json::to_string(value).expect("serializes");
    serde_json::from_str(&json).expect("deserializes")
}

fn project() -> ProjectKey {
    ProjectKey::Git {
        common_dir: path("/r/.git"),
    }
}

fn worktree() -> WorktreeKey {
    WorktreeKey(path("/r/.git/worktrees/w"))
}

fn session(id: &str) -> SessionKey {
    SessionKey {
        source: SourceId(text("session-claude-code")),
        id: text(id),
    }
}

fn read_target() -> ReadTarget {
    ReadTarget {
        project: project(),
        worktree: Some(worktree()),
        path: text("src/lib.rs"),
        origin: TextOrigin::Worktree,
        expect: Some(BlobId::Sha1([7; 20])),
        at: Anchor::Line {
            line: 12,
            column: 4,
        },
    }
}

fn route(screen: &'static str, target: Target) -> Route {
    Route {
        screen: Name::Borrowed(screen),
        target,
    }
}

fn layout() -> ScreenLayout {
    ScreenLayout {
        sidebar: Visibility::Hidden,
        back_button: true,
        reading_column: true,
        inspector: true,
        floating_bar: true,
    }
}

#[test]
fn workspace_contributions_service_id_and_multiple() {
    assert_eq!(<dyn Contributions as Service>::ID, "contributions");
    const { assert!(!<dyn Contributions as Service>::MULTIPLE) };
    let key = service::<dyn Contributions>();
    assert_eq!(key.id(), "contributions");
    assert!(!key.multiple());
}

#[test]
fn workspace_leave_guard_is_not_a_service() {
    // Guards register through `Contributions::register_leave_guard`; a
    // `MULTIPLE` service would make `workspace` wait for one to activate.
    const { assert!(!Probe::<dyn LeaveGuard>::IS_SERVICE) };
    // The probe does tell services apart.
    const { assert!(Probe::<dyn Contributions>::IS_SERVICE) };
}

#[test]
fn workspace_contributions_is_a_shared_trait_object() {
    let contributions: Arc<dyn Contributions> = Arc::new(Empty);
    // Inside `activate` there is no token yet: `None`, then `own_staged`.
    let registration = contributions.register(
        Contribution {
            kind: ContributionKind::Screen(layout()),
            id: Name::Borrowed(screens::REVIEW),
            order: 0,
            payload: Payload(Box::new(42u8)),
        },
        None,
    );
    assert_eq!(
        format!("{registration:?}"),
        "Registration { committed: false, .. }"
    );
    drop(registration);
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    drop(contributions.handle_command(
        name("finder.open"),
        Box::new(move |call| {
            assert_eq!(call.window, WindowId(0));
            c.fetch_add(1, Ordering::SeqCst);
        }),
        None,
    ));
    drop(contributions.register_leave_guard(Arc::new(Permissive), None));
    contributions.dispatch(WorkspaceCommand::Escape);
    assert!(contributions.sidebar_entries().is_empty());
    assert_eq!(contributions.current_route(WindowId(0)), None);
    contributions.report_milestone(Milestone::ProjectHome);

    // The service and the guards are shared across threads; registrations
    // and payloads move to the kernel (`own_staged`) or to the main thread;
    // sidebar entries are shared with the views; events cross threads.
    shareable::<Arc<dyn Contributions>>();
    shareable::<Arc<dyn LeaveGuard>>();
    shareable::<CommandHandler>();
    shareable::<Arc<[SidebarEntry]>>();
    shareable::<WorkspaceEvent>();
    shareable::<WorkspaceCommand>();
    sendable::<Registration>();
    sendable::<Contribution>();
    sendable::<Payload>();
}

#[test]
fn workspace_leave_guard_answers_through_done() {
    let guard: Arc<dyn LeaveGuard> = Arc::new(Permissive);
    let answer = Arc::new(Mutex::new(None));
    let a = answer.clone();
    guard.ask(
        LeaveScope::Project(project()),
        Box::new(move |ok| *a.lock().unwrap() = Some(ok)),
    );
    assert_eq!(*answer.lock().unwrap(), Some(true));
}

#[test]
fn workspace_command_handler_receives_the_call() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    let handler: CommandHandler = Box::new(move |call| s.lock().unwrap().push(call.clone()));
    let call = CommandCall {
        window: WindowId(0),
        name: name("finder.open"),
    };
    handler(&call);
    assert_eq!(*seen.lock().unwrap(), vec![call.clone()]);
    assert_eq!(round_trip(&call), call);
}

#[test]
fn workspace_payload_downcasts_to_its_type() {
    #[derive(Debug, PartialEq)]
    struct Screen(&'static str);

    let payload = Payload(Box::new(Screen("welcome")));
    assert_eq!(format!("{payload:?}"), "Payload { .. }");
    assert!(payload.0.downcast_ref::<u8>().is_none());
    let screen = payload.0.downcast::<Screen>().expect("the registered type");
    assert_eq!(*screen, Screen("welcome"));

    // Applied entries share the same value with every view.
    let shared = SharedPayload(Arc::new(Screen("files")));
    assert_eq!(format!("{shared:?}"), "SharedPayload { .. }");
    let entry = SidebarEntry {
        id: name("files"),
        section: SidebarSection::Views,
        order: 20,
        payload: shared.clone(),
    };
    let again = entry.clone();
    assert!(again.payload.0.clone().downcast::<u8>().is_err());
    let screen = again
        .payload
        .0
        .downcast::<Screen>()
        .expect("the applied type");
    assert_eq!(*screen, Screen("files"));
    assert!(Arc::ptr_eq(&shared.0, &entry.payload.0));
    assert!(format!("{entry:?}").starts_with("SidebarEntry { id: \"files\""));
}

#[test]
fn workspace_sidebar_sections_order_top_to_bottom() {
    assert!(SidebarSection::Project < SidebarSection::Views);
    assert!(SidebarSection::Views < SidebarSection::Bottom);
}

#[test]
fn workspace_registration_releases_once_on_drop() {
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    let registration = Registration::new(
        |_| panic!("never committed"),
        move || {
            c.fetch_add(1, Ordering::SeqCst);
        },
    );
    assert_eq!(count.load(Ordering::SeqCst), 0);
    // Dropped on another thread, as a plugin's owned values may be.
    std::thread::spawn(move || drop(registration))
        .join()
        .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn workspace_staged_registration_commits_with_the_activation_token() {
    let observed = Arc::new(Observed::default());
    let kernel = TestKernel::new(Staging(observed.clone()));
    kernel.activate().unwrap();

    // `commit` ran once on the kernel thread, before `started` made the
    // token live; the token it received is the activation's.
    assert_eq!(observed.commits.load(Ordering::SeqCst), 1);
    assert_eq!(observed.releases.load(Ordering::SeqCst), 0);
    let (token, state) = observed.token.lock().unwrap().clone().unwrap();
    assert_eq!(state, TokenState::Pending);
    assert_eq!(token.state(), TokenState::Live);

    // Deactivation drops the guard: it releases once, after the token died.
    kernel.deactivate();
    assert_eq!(observed.commits.load(Ordering::SeqCst), 1);
    assert_eq!(observed.releases.load(Ordering::SeqCst), 1);
    assert_eq!(token.state(), TokenState::Dead);
}

#[test]
fn workspace_screen_ids_are_valid_names() {
    for id in [
        screens::WELCOME,
        screens::PROJECT_HOME,
        screens::REVIEW,
        screens::READER,
        screens::WORKTREES,
        screens::USAGE,
    ] {
        assert!((1..=64).contains(&id.len()), "{id}");
        assert!(
            id.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-'),
            "{id}"
        );
    }
}

#[test]
fn workspace_contribution_kinds_round_trip() {
    let kinds = [
        ContributionKind::Screen(layout()),
        ContributionKind::Screen(ScreenLayout {
            sidebar: Visibility::Shown,
            back_button: false,
            reading_column: false,
            inspector: false,
            floating_bar: false,
        }),
        ContributionKind::SidebarPanel,
        ContributionKind::SidebarEntry {
            section: SidebarSection::Project,
        },
        ContributionKind::SidebarEntry {
            section: SidebarSection::Views,
        },
        ContributionKind::SidebarEntry {
            section: SidebarSection::Bottom,
        },
        ContributionKind::InspectorTab,
        ContributionKind::Panel,
        ContributionKind::Overlay,
        ContributionKind::KeyBindings,
    ];
    for kind in kinds {
        assert_eq!(round_trip(&kind), kind);
    }
    let contribution = Contribution {
        kind: ContributionKind::Panel,
        id: name("context"),
        order: u16::MAX,
        payload: Payload(Box::new(())),
    };
    assert_eq!(
        format!("{contribution:?}"),
        "Contribution { kind: Panel, id: \"context\", order: 65535, .. }"
    );
}

#[test]
fn workspace_routes_and_targets_round_trip() {
    let targets = [
        Target::None,
        Target::Project(ProjectKey::Folder { root: path("/f") }),
        Target::Review(ReviewTarget::Session(session("s-1"))),
        Target::Review(ReviewTarget::Outside {
            project: project(),
            worktree: worktree(),
        }),
        Target::Read(read_target()),
    ];
    for target in targets {
        assert_eq!(round_trip(&target), target);
        let route = route(screens::REVIEW, target);
        assert_eq!(round_trip(&route), route);
    }
    // A borrowed screen name comes back owned and still compares equal.
    let route = route(screens::READER, Target::None);
    let back = round_trip(&route);
    assert!(matches!(back.screen, Name::Owned(_)));
    assert_eq!(back, route);
}

#[test]
fn workspace_commands_round_trip() {
    let commands = [
        WorkspaceCommand::OpenProject(PathBuf::from("/p/with space/项目")),
        WorkspaceCommand::PromptOpenProject,
        WorkspaceCommand::CloseProject,
        WorkspaceCommand::SetActiveWorktree(worktree()),
        WorkspaceCommand::Push(route(screens::READER, Target::Read(read_target()))),
        WorkspaceCommand::Pop,
        WorkspaceCommand::PopToBase,
        WorkspaceCommand::SetReadingColumn(true),
        WorkspaceCommand::SetReadingColumn(false),
        WorkspaceCommand::ShowSidebarPanel(Some(name("files"))),
        WorkspaceCommand::ShowSidebarPanel(None),
        WorkspaceCommand::SetInspector(true),
        WorkspaceCommand::SetInspector(false),
        WorkspaceCommand::SelectInspectorTab(name("conversation")),
        WorkspaceCommand::OpenPanel(name("context")),
        WorkspaceCommand::ClosePanel,
        WorkspaceCommand::OpenOverlay(name("finder")),
        WorkspaceCommand::CloseOverlay,
        WorkspaceCommand::Escape,
        WorkspaceCommand::CloseWindow,
    ];
    for command in commands {
        assert_eq!(round_trip(&command), command);
    }
}

#[test]
fn workspace_events_round_trip() {
    fn event<E: Event + Clone>() {}
    event::<WorkspaceEvent>();

    let layers = [
        Layer::SidebarPanel(name("files")),
        Layer::Inspector,
        Layer::Panel(name("context")),
        Layer::Overlay(name("finder")),
    ];
    let mut events = vec![
        WorkspaceEvent::ScreenChanged {
            window: WindowId(0),
            from: None,
            to: route(screens::WELCOME, Target::None),
        },
        WorkspaceEvent::ScreenChanged {
            window: WindowId(u32::MAX),
            from: Some(route(screens::PROJECT_HOME, Target::Project(project()))),
            to: route(
                screens::REVIEW,
                Target::Review(ReviewTarget::Session(session("s-1"))),
            ),
        },
        WorkspaceEvent::ContributionsChanged,
        WorkspaceEvent::WindowClosed {
            window: WindowId(0),
        },
    ];
    for (i, layer) in layers.into_iter().enumerate() {
        assert_eq!(round_trip(&layer), layer);
        events.push(WorkspaceEvent::LayerChanged {
            window: WindowId(0),
            layer,
            open: i % 2 == 0,
        });
    }
    for e in events {
        assert_eq!(round_trip(&e), e);
    }
}

#[test]
fn workspace_scopes_and_milestones_round_trip() {
    let scopes = [
        LeaveScope::View,
        LeaveScope::Project(project()),
        LeaveScope::Worktree(ProjectKey::Folder { root: path("/f") }),
        LeaveScope::Window,
        LeaveScope::App,
    ];
    for scope in scopes {
        assert_eq!(round_trip(&scope), scope);
    }
    let milestones = [
        Milestone::ProjectHome,
        Milestone::FileHighlighted,
        Milestone::DiffShown,
        Milestone::WorktreeSwitched,
    ];
    for milestone in milestones {
        assert_eq!(round_trip(&milestone), milestone);
    }
    for id in [WindowId(0), WindowId(u32::MAX)] {
        assert_eq!(round_trip(&id), id);
    }
    // Enums are serde's default external tagging, like every other module.
    assert_eq!(
        serde_json::to_string(&Visibility::Shown).unwrap(),
        "\"Shown\""
    );
    assert_eq!(
        serde_json::to_string(&LeaveScope::Worktree(ProjectKey::Folder {
            root: path("/f")
        }))
        .unwrap(),
        r#"{"Worktree":{"Folder":{"root":"/f"}}}"#
    );
}
