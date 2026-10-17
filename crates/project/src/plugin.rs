//! The `project` plugin: provides `dyn Projects` (docs/design/project.md
//! 「本 crate 的公开项」).

use std::path::PathBuf;
use std::sync::Arc;

use lectern_executor::{Executor, Priority};
use lectern_kernel::{
    Activation, Active, Context, Manifest, Plugin, PluginError, PluginHandle, WorkspaceFacts,
    service,
};
use lectern_services::project::{ProjectEvent, Projects};

use crate::git_access::{GitAccess, LecternGit};
use crate::model::{Host, Model};

/// Built by `app` and put in the `PluginSet` as "project".
pub struct ProjectPlugin {
    deps: ProjectDeps,
    git: Arc<dyn GitAccess>,
}

/// What `app` hands the plugin. Process-wide resources are created once by
/// `app` (ADR 0025); the plugin creates none.
pub struct ProjectDeps {
    pub executor: Executor,
    /// The user's home directory, to tell a dotfiles repository whose main
    /// worktree is `$HOME` from the folders below it.
    pub home: PathBuf,
}

impl ProjectPlugin {
    /// Reads `GIT_CEILING_DIRECTORIES` once; does no IO.
    pub fn new(deps: ProjectDeps) -> ProjectPlugin {
        ProjectPlugin {
            deps,
            git: Arc::new(LecternGit::from_env()),
        }
    }

    /// The same plugin over `testing::FakeGit`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_fake_git(deps: ProjectDeps, git: Arc<crate::testing::FakeGit>) -> ProjectPlugin {
        ProjectPlugin { deps, git }
    }
}

impl Plugin for ProjectPlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: vec![service::<dyn Projects>()],
            requires: Vec::new(),
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    /// An empty project table; no IO, no task.
    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        let model = Model::new(self.git.clone(), self.deps.home.clone());
        ctx.provide::<dyn Projects>(model.clone())?;
        Ok(Box::new(ProjectActive {
            model,
            executor: self.deps.executor.clone(),
        }))
    }
}

struct ProjectActive {
    model: Arc<Model>,
    executor: Executor,
}

impl Active for ProjectActive {
    /// Submits nothing: the first scan waits for the first `open`.
    fn started(&mut self, handle: PluginHandle) {
        self.model.attach(Arc::new(KernelHost {
            handle,
            executor: self.executor.clone(),
        }));
    }
}

impl Drop for ProjectActive {
    fn drop(&mut self) {
        self.model.shutdown();
    }
}

/// Work goes through `own_with` to the executor with a `BusyGuard` taken
/// before submission, so the kernel is not quiescent between `open`
/// returning and the scan starting.
struct KernelHost {
    handle: PluginHandle,
    executor: Executor,
}

impl Host for KernelHost {
    fn spawn(&self, work: Box<dyn FnOnce() + Send>) {
        let Some(busy) = self.handle.busy() else {
            return;
        };
        let executor = self.executor.clone();
        self.handle.own_with(move |gate| {
            executor.run_gated(&gate, Priority::Background, move || {
                let _busy = busy;
                work();
            })
        });
    }

    fn emit(&self, event: ProjectEvent) {
        self.handle.emitter().emit(event);
    }

    fn workspace_changed(&self, facts: Arc<dyn WorkspaceFacts>) {
        self.handle.control().workspace_changed(facts);
    }
}
