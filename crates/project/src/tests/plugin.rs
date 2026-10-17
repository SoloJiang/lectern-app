//! The plugin in a real kernel with a manual executor (`plugin_*`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lectern_executor::Executor;
use lectern_fs_watch::Flags;
use lectern_kernel::{
    Activation, Active, Context, Kernel, Manifest, Plugin, PluginError, PluginSet, PluginState,
    ServiceRef, service,
};
use lectern_services::project::{ProjectEvent, Projects};

use super::support::*;
use crate::testing::FakeGit;
use crate::{ProjectDeps, ProjectPlugin};

type Slot = Arc<Mutex<Option<ServiceRef<dyn Projects>>>>;

/// Requires `dyn Projects`, keeps it and logs every `ProjectEvent`.
struct Consumer {
    slot: Slot,
    events: Arc<Mutex<Vec<ProjectEvent>>>,
}

impl Plugin for Consumer {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: Vec::new(),
            requires: vec![service::<dyn Projects>()],
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        *self.slot.lock().unwrap() = Some(ctx.service::<dyn Projects>());
        let events = self.events.clone();
        ctx.on::<ProjectEvent>(move |event, _| events.lock().unwrap().push(event.clone()));
        Ok(Box::new(()))
    }
}

/// Activated by a `pom.xml` anywhere in the open projects.
struct Java(Arc<AtomicBool>);

impl Plugin for Java {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: Vec::new(),
            requires: Vec::new(),
            activation: vec![Activation::Workspace(vec!["pom.xml".into()])],
        }
    }

    fn activate(&self, _: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        self.0.store(true, Ordering::SeqCst);
        Ok(Box::new(()))
    }
}

struct Setup {
    kernel: Arc<Kernel>,
    executor: Executor,
    git: Arc<FakeGit>,
    slot: Slot,
    events: Arc<Mutex<Vec<ProjectEvent>>>,
    java: Arc<AtomicBool>,
    watch: Watch,
}

impl Setup {
    fn start(git: FakeGit) -> Setup {
        let git = Arc::new(git);
        let executor = Executor::manual();
        // fs_watch runs on its own manual executor: `executor` holds only
        // the plugin's work.
        let watch = Watch::new();
        let deps = ProjectDeps {
            executor: executor.clone(),
            watcher: watch.watcher.clone(),
            home: git.base().join("home"),
        };
        let slot = Slot::default();
        let events = Arc::default();
        let java = Arc::new(AtomicBool::new(false));
        let set = PluginSet::new()
            .with("project", ProjectPlugin::with_fake_git(deps, git.clone()))
            .with(
                "consumer",
                Consumer {
                    slot: slot.clone(),
                    events: Arc::clone(&events),
                },
            )
            .with("java", Java(java.clone()));
        let kernel = Kernel::start(set);
        kernel.barrier();
        Setup {
            kernel,
            executor,
            git,
            slot,
            events,
            java,
            watch,
        }
    }

    fn projects(&self) -> ServiceRef<dyn Projects> {
        self.slot
            .lock()
            .unwrap()
            .clone()
            .expect("consumer is active")
    }

    fn state(&self, id: &str) -> PluginState {
        self.kernel
            .status()
            .into_iter()
            .find(|s| s.id == id)
            .map(|s| s.state)
            .unwrap()
    }

    /// Runs the executor and fs_watch in turn and lets the kernel handle
    /// what they queued, until none of them has work left.
    fn settle(&self) {
        loop {
            self.watch.driver.advance(LONG);
            let ran = self.executor.run_until_idle();
            self.kernel.barrier();
            if ran == 0 && self.watch.driver.run_until_idle() == 0 {
                return;
            }
        }
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        let (done, wait) = std::sync::mpsc::channel();
        self.kernel.shutdown(move || {
            let _ = done.send(());
        });
        let _ = wait.recv_timeout(Duration::from_secs(10));
        self.executor.shutdown(Duration::from_secs(10));
    }
}

#[test]
fn plugin_activation_reaches_no_git_before_the_first_open() {
    let git = FakeGit::new();
    let git_dir = git.init("repo");
    monorepo(&git, &git_dir);
    let s = Setup::start(git);
    assert_eq!(s.state("project"), PluginState::Active);
    assert_eq!(s.state("consumer"), PluginState::Active);
    // Nothing was submitted and git was never called.
    assert_eq!(s.executor.run_until_idle(), 0);
    assert!(s.git.calls().is_empty());
    assert!(s.projects().get().unwrap().open_projects().is_empty());
}

#[test]
fn plugin_open_submits_facts_that_activate_workspace_plugins() {
    let git = FakeGit::new();
    let git_dir = git.init("repo");
    monorepo(&git, &git_dir);
    let s = Setup::start(git);
    assert_eq!(s.state("java"), PluginState::Inactive);
    let path = s.git.base().join("repo");
    s.projects().get().unwrap().open(&path);
    s.settle();
    assert_eq!(s.state("java"), PluginState::Active);
    assert!(s.java.load(Ordering::SeqCst));
}

#[test]
fn plugin_without_matching_files_activates_nothing() {
    let git = FakeGit::new();
    let git_dir = git.init("repo");
    git.write(&git_dir, "package.json", b"{}");
    let s = Setup::start(git);
    s.projects().get().unwrap().open(&s.git.base().join("repo"));
    s.settle();
    assert_eq!(s.state("java"), PluginState::Inactive);
}

#[test]
fn plugin_events_are_opened_then_changed() {
    let git = FakeGit::new();
    let git_dir = git.init("repo");
    monorepo(&git, &git_dir);
    let s = Setup::start(git);
    let request = s.projects().get().unwrap().open(&s.git.base().join("repo"));
    s.settle();
    let events = s.events.lock().unwrap().clone();
    assert!(opened(&events, request).is_ok());
    check_event_order(&events);
    insta::assert_snapshot!(render_events(&events, s.git.base()), @r#"
    Opened Ok(Git { common_dir: "[base]/repo/.git" })
    Changed 2 [files]
    Changed 3 [packages]
    "#);
    let key = s.projects().get().unwrap().open_projects().pop().unwrap();
    let snapshot = s.projects().get().unwrap().snapshot(&key).unwrap();
    assert_eq!(snapshot.generation, 3);
}

#[test]
fn plugin_deactivation_drops_the_projects() {
    let git = FakeGit::new();
    let git_dir = git.init("repo");
    monorepo(&git, &git_dir);
    let s = Setup::start(git);
    let projects = s.projects();
    projects.get().unwrap().open(&s.git.base().join("repo"));
    s.settle();
    s.kernel.set_enabled("project", false);
    s.kernel.barrier();
    assert!(matches!(s.state("consumer"), PluginState::Waiting { .. }));
    // The provider is revoked: the reference no longer admits calls.
    assert!(projects.get().is_none());
}

#[test]
fn plugin_refresh_keeps_the_kernel_busy_until_applied() {
    let git = FakeGit::new();
    let git_dir = git.init("repo");
    monorepo(&git, &git_dir);
    let s = Setup::start(git);
    s.projects().get().unwrap().open(&s.git.base().join("repo"));
    s.settle();
    // The kernel thread drops its last guards on its own time.
    wait_quiescent(&s.kernel);
    s.events.lock().unwrap().clear();

    s.git
        .write(&git_dir, "apps/web/src/new.ts", b"export {};\n");
    let changed = Flags(Flags::FILE.0 | Flags::MODIFIED.0);
    s.watch.backend.emit(&git_dir, &[("index", changed)]);
    assert!(
        !s.kernel.is_quiescent(),
        "the event waits in fs_watch's queue"
    );
    s.watch.driver.run_until_idle();
    s.watch.driver.advance(LONG);
    s.kernel.barrier();
    assert!(!s.kernel.is_quiescent(), "the batch's refresh is queued");
    s.executor.run_until_idle();
    s.kernel.barrier();
    wait_quiescent(&s.kernel);
    let events = s.events.lock().unwrap().clone();
    insta::assert_snapshot!(render_events(&events, s.git.base()), @"Changed 4 [files, packages]");
}

fn wait_quiescent(kernel: &Kernel) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !kernel.is_quiescent() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for quiescence"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
