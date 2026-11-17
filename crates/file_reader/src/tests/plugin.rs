//! `plugin_*`: the manifest, the activation, the `ProjectEvent` listener and
//! the service through a real kernel (`TestKernel`) with the three services
//! it requires mocked.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, Weak};

use lectern_kernel::testing::TestKernel;
use lectern_kernel::{Activation, service};
use lectern_services::code_intel::{CodeIntel, Focus, TextOrigin};
use lectern_services::project::{ProjectEvent, Projects};
use lectern_services::reader::{Anchor, OpenOutcome, Reader, ReaderEvent};
use lectern_services::workspace::Contributions;
use lectern_syntax::{DocState, testing};

use super::rig::{
    FakeCodeIntel, FakeContributions, FakeProjects, Resources, Tree, file, folder_snapshot,
};
use crate::{FileReaderPlugin, Session};

/// The plugin in a kernel with the mocked services, and what the mocks saw.
struct Fixture {
    res: Resources,
    kernel: TestKernel,
    sessions: Arc<Mutex<Weak<Session>>>,
    projects: Arc<FakeProjects>,
    code_intel: Arc<FakeCodeIntel>,
}

impl Fixture {
    fn new() -> Fixture {
        let res = Resources::new();
        let plugin = FileReaderPlugin::new(res.deps());
        let sessions = plugin.sessions();
        let projects = Arc::new(FakeProjects::default());
        let code_intel = Arc::new(FakeCodeIntel::default());
        let kernel = TestKernel::new(plugin)
            .mock::<dyn Projects>(projects.clone())
            .mock::<dyn CodeIntel>(code_intel.clone())
            .mock::<dyn Contributions>(Arc::new(FakeContributions::default()));
        Fixture {
            res,
            kernel,
            sessions,
            projects,
            code_intel,
        }
    }

    fn session(&self) -> Arc<Session> {
        self.sessions
            .lock()
            .unwrap()
            .upgrade()
            .expect("the plugin is active")
    }
}

#[test]
fn plugin_manifest_is_the_designed_one() {
    let res = Resources::new();
    let manifest = lectern_kernel::Plugin::manifest(&FileReaderPlugin::new(res.deps()));
    assert_eq!(manifest.provides, vec![service::<dyn Reader>()]);
    assert_eq!(
        manifest.requires,
        vec![
            service::<dyn Projects>(),
            service::<dyn CodeIntel>(),
            service::<dyn Contributions>(),
        ]
    );
    assert_eq!(manifest.activation, vec![Activation::AfterFirstFrame]);
}

#[test]
fn plugin_activation_provides_the_service_and_submits_no_task() {
    let f = Fixture::new();
    // `new` did no IO and built no session.
    assert!(f.sessions.lock().unwrap().upgrade().is_none());
    // The kernel fails an activation that does not provide its declared
    // services, so success means `dyn Reader` is provided.
    f.kernel.activate().expect("the plugin activates");
    let session = f.session();
    // Started with a handle, but nothing was submitted: no task, no work.
    assert_eq!(session.engine().tasks_in_flight(), 0);
    assert_eq!(f.res.executor.run_until_idle(), 0);
    assert_eq!(session.pending(), 0);
    assert!(session.snapshot().is_none());
    assert!(f.code_intel.calls().is_empty());
}

#[test]
fn plugin_deactivation_ends_the_session() {
    let f = Fixture::new();
    f.kernel.activate().unwrap();
    let tree = Tree::new();
    let key = tree.folder();
    f.projects.set(folder_snapshot(&key, &tree.root));
    tree.write("a.ts", b"let a = 1;\n");
    let session = f.session();
    session.open(file(&key, "a.ts", Anchor::Start));
    let request = session.next_request().unwrap();
    session.apply(request);
    f.res.settle();
    session.sync();
    assert_eq!(f.code_intel.calls().len(), 1);
    drop(session);
    f.kernel.deactivate();
    // The focus was cleared while `code_intel` was still there, and the
    // session is gone with the activation.
    assert_eq!(f.code_intel.current().last(), Some(&Focus::Current(None)));
    assert!(f.sessions.lock().unwrap().upgrade().is_none());
}

#[test]
fn plugin_a_closed_project_releases_its_roots() {
    let f = Fixture::new();
    f.kernel.activate().unwrap();
    let session = f.session();
    let tree = Tree::new();
    let key = tree.folder();
    let kernel = f.kernel.inner().expect("the kernel");

    // Only a closed project calls `Engine::close_project` (one task).
    kernel.emitter().emit(ProjectEvent::Changed {
        project: key.clone(),
        generation: 2,
        parts: Default::default(),
    });
    assert_eq!(session.engine().tasks_in_flight(), 0);
    kernel.emitter().emit(ProjectEvent::Closed {
        project: key.clone(),
    });
    assert_eq!(session.engine().tasks_in_flight(), 1);
    assert_eq!(f.res.executor.run_until_idle(), 1);
    assert_eq!(session.engine().tasks_in_flight(), 0);
}

#[test]
fn plugin_the_service_opens_from_any_thread_and_the_view_answers_on_the_bus() {
    let f = Fixture::new();
    f.kernel.activate().unwrap();
    let session = f.session();
    let tree = Tree::new();
    let key = tree.folder();
    f.projects.set(folder_snapshot(&key, &tree.root));
    tree.write("a.ts", &testing::ts(2 << 10, 0));
    let woken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = woken.clone();
    session.set_waker(Some(Arc::new(move || {
        count.fetch_add(1, Ordering::SeqCst);
    })));

    // The service object is the session: called as `dyn Reader`.
    let reader: Arc<dyn Reader> = session.clone();
    let ticket = std::thread::scope(|scope| {
        scope
            .spawn(|| reader.open(file(&key, "a.ts", Anchor::Start)))
            .join()
            .unwrap()
    });
    assert_eq!(woken.load(Ordering::SeqCst), 1);
    let request = session.next_request().expect("queued");
    assert_eq!(request.ticket, ticket);
    session.apply(request);
    f.res.settle();
    assert_eq!(session.snapshot().unwrap().state, DocState::Ready);
    // The event went through the kernel's bus, from the plugin.
    assert_eq!(
        f.kernel.emitted::<ReaderEvent>(),
        vec![ReaderEvent::Open {
            ticket,
            outcome: OpenOutcome::Shown
        }]
    );
    // And `code_intel` was told through its service.
    session.sync();
    let focus = f.code_intel.current();
    assert!(matches!(
        focus.as_slice(),
        [Focus::Current(Some(text))] if &*text.path == "a.ts" && text.origin == TextOrigin::Worktree
    ));
}
