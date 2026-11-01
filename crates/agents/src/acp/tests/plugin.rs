//! 插件（`conversations_plugin_*`）：真实的 kernel，假的来源和项目。

use std::sync::Arc;

use lectern_executor::Executor;
use lectern_kernel::testing::TestKernel;
use lectern_kernel::{Plugin, service};
use lectern_services::conversation::{ConversationEvent, Conversations, Load, Unavailable};
use lectern_services::project::Projects;
use lectern_services::session::{Agent, SessionSource};

use super::support::*;
use crate::acp::engine::Engine;
use crate::{ConversationsDeps, conversations_plugin};

struct Fixture {
    kernel: TestKernel,
    executor: Executor,
    engine_slot: Arc<std::sync::Mutex<Option<Arc<Engine>>>>,
    source: Arc<FakeSource>,
    projects: Arc<FakeProjects>,
}

fn fixture() -> Fixture {
    let executor = Executor::manual();
    let (_, plugin) = conversations_plugin(ConversationsDeps {
        executor: executor.clone(),
    });
    // No login shell, and no adapter on the path.
    let plugin = plugin.with_login_env(vec![("PATH".into(), "/nonexistent".into())]);
    let engine_slot = plugin.engine.clone();
    let source = FakeSource::new(claude_source(), Agent::ClaudeCode);
    let projects = Arc::new(FakeProjects::default());
    let kernel = TestKernel::new(plugin)
        .mock::<dyn SessionSource>(source.clone())
        .mock::<dyn Projects>(projects.clone());
    Fixture {
        kernel,
        executor,
        engine_slot,
        source,
        projects,
    }
}

impl Fixture {
    fn engine(&self) -> Arc<Engine> {
        self.engine_slot.lock().unwrap().clone().expect("activated")
    }

    fn session(&self) -> lectern_services::session::SessionKey {
        let session = key(&claude_source(), CLAUDE_SESSION);
        let project = folder("/work/project");
        self.projects.set_open(vec![project.clone()]);
        self.source.add(
            &session,
            Spec::located(&session, project, &Agent::ClaudeCode),
        );
        let turns = vec![self.source.turn(1, "hello", vec![])];
        self.source.set_turns(&session, turns);
        session
    }
}

#[test]
fn conversations_plugin_declares_its_services() {
    let (name, plugin) = conversations_plugin(ConversationsDeps {
        executor: Executor::manual(),
    });
    assert_eq!(name, "conversations");
    let manifest = plugin.manifest();
    assert_eq!(manifest.provides, [service::<dyn Conversations>()]);
    assert!(manifest.requires.contains(&service::<dyn SessionSource>()));
    assert!(manifest.requires.contains(&service::<dyn Projects>()));
    assert_eq!(
        manifest.activation,
        [lectern_kernel::Activation::AfterFirstFrame]
    );
}

#[test]
fn conversations_plugin_starts_nothing_until_a_session_is_watched() {
    let f = fixture();
    f.kernel.activate().unwrap();
    assert_eq!(f.executor.run_until_idle(), 0, "activation submits no task");
    assert!(f.kernel.emitted::<ConversationEvent>().is_empty());
    assert!(f.engine().waiting().is_empty());
}

#[test]
fn conversations_plugin_reads_the_history_on_the_executor() {
    let f = fixture();
    f.kernel.activate().unwrap();
    let session = f.session();
    let engine = f.engine();
    let interest = engine.watch(&session);
    let loading = engine.snapshot(&session).unwrap();
    assert_eq!(loading.load, Load::Loading);
    // The lookup has not run yet: not being installed is not known, so it is not an error.
    assert_eq!(loading.can_prompt, Ok(()));
    assert!(loading.entries.is_empty());

    assert!(f.executor.run_until_idle() > 0);
    let ready = engine.snapshot(&session).unwrap();
    assert_eq!(ready.load, Load::Ready);
    assert_eq!(ready.entries.len(), 1);
    // The program lookup found nothing on the path: the answer changed with it.
    assert_eq!(
        ready.can_prompt,
        Err(Unavailable::NotInstalled(
            lectern_services::conversation::AcpProgram::ClaudeCode
        ))
    );

    let events = f.kernel.emitted::<ConversationEvent>();
    let last = events.last().expect("events");
    assert_eq!(
        last,
        &ConversationEvent::Changed {
            session: session.clone(),
            generation: ready.generation
        }
    );
    drop(interest);
    assert!(engine.snapshot(&session).is_none());
}

#[test]
fn conversations_plugin_rereads_after_a_sessions_changed_event() {
    let f = fixture();
    f.kernel.activate().unwrap();
    let session = f.session();
    let engine = f.engine();
    let _interest = engine.watch(&session);
    f.executor.run_until_idle();
    assert_eq!(engine.snapshot(&session).unwrap().entries.len(), 1);

    let turns = vec![
        f.source.turn(1, "hello", vec![]),
        f.source.turn(2, "again", vec![]),
    ];
    f.source.set_turns(&session, turns);
    f.kernel
        .inner()
        .unwrap()
        .emitter()
        .emit(changed(&claude_source(), &session));
    f.executor.run_until_idle();
    assert_eq!(engine.snapshot(&session).unwrap().entries.len(), 2);
}

#[test]
fn conversations_plugin_follows_the_open_projects() {
    let f = fixture();
    f.kernel.activate().unwrap();
    let session = f.session();
    let engine = f.engine();
    let _interest = engine.watch(&session);
    f.executor.run_until_idle();
    f.projects.set_open(vec![]);
    f.kernel
        .inner()
        .unwrap()
        .emitter()
        .emit(lectern_services::project::ProjectEvent::Closed {
            project: folder("/work/project"),
        });
    assert_eq!(
        engine.snapshot(&session).unwrap().can_prompt,
        Err(Unavailable::OutsideProject)
    );
}

#[test]
fn conversations_plugin_drops_its_state_when_deactivated() {
    let f = fixture();
    f.kernel.activate().unwrap();
    let session = f.session();
    let engine = f.engine();
    let interest = engine.watch(&session);
    f.executor.run_until_idle();
    let announced = f.kernel.emitted::<ConversationEvent>().len();
    f.kernel.deactivate();
    assert!(engine.snapshot(&session).is_none());
    // Releasing an interest of a stopped plugin does nothing; a new one is inert.
    drop(interest);
    let _late = engine.watch(&session);
    f.executor.run_until_idle();
    assert!(engine.snapshot(&session).is_none());
    assert_eq!(f.kernel.emitted::<ConversationEvent>().len(), announced);
}
