//! 两个插件："session-claude-code" 与 "session-codex"（docs/design/agents.md「依赖」）。

use std::path::PathBuf;
use std::sync::{Arc, Weak};

use lectern_cache::Cache;
use lectern_executor::Executor;
use lectern_fs_watch::Watcher;
use lectern_kernel::{
    Activation, Active, Context, Manifest, Name, Plugin, PluginError, PluginHandle, service,
};
use lectern_services::project::{ProjectEvent, Projects};
use lectern_services::session::{SessionSource, SourceId};

use crate::claude::{self, ClaudeCode};
use crate::codex::{self, Codex, Lineage};
use crate::filter::SessionFilter;
use crate::roots::AgentRoots;
use crate::source::place::ProjectsRef;
use crate::source::{Adapter, Engine, Setup};

/// `app` 交给插件的资源。进程级的资源由 `app` 创建一次（ADR 0025），插件不创建。
pub struct AgentsDeps {
    pub executor: Executor,
    pub cache: Cache,
    pub watch: Watcher,
    /// 压缩会话的解压副本目录，`app` 取 `platform` 提供的应用缓存目录下的 `agents/decoded/`。
    pub decoded_dir: PathBuf,
    /// 会话目录：默认 [`AgentRoots::from_env`]，测试传临时目录。
    pub roots: AgentRoots,
    /// 隐藏的会话，两个插件共用（Analyze 的后续 issue 隐藏辅助会话）。
    pub filter: SessionFilter,
}

/// 两个插件：ID "session-claude-code" 与 "session-codex"，各提供一个 `dyn SessionSource`，激活条件
/// `AfterFirstFrame`，依赖 `dyn Projects`。构造时不做 IO。`app` 把它们逐个放进 `PluginSet::with`。
pub fn plugins(deps: AgentsDeps) -> [(Name, SessionPlugin); 2] {
    let deps = Arc::new(deps);
    [
        (
            Name::Borrowed(claude::SOURCE_ID),
            SessionPlugin {
                kind: Kind::ClaudeCode,
                deps: deps.clone(),
                #[cfg(test)]
                engine: Arc::default(),
            },
        ),
        (
            Name::Borrowed(codex::SOURCE_ID),
            SessionPlugin {
                kind: Kind::Codex,
                deps,
                #[cfg(test)]
                engine: Arc::default(),
            },
        ),
    ]
}

#[derive(Clone, Copy)]
enum Kind {
    ClaudeCode,
    Codex,
}

/// 一个会话来源插件（[`plugins`] 给出的两个之一）。
pub struct SessionPlugin {
    kind: Kind,
    deps: Arc<AgentsDeps>,
    /// 测试用：最近一次激活建的 `Engine`。
    #[cfg(test)]
    pub(crate) engine: Arc<std::sync::Mutex<Option<Arc<dyn std::any::Any + Send + Sync>>>>,
}

impl SessionPlugin {
    fn setup(&self, ctx: &Context<'_>, adapter_version: u32) -> Setup {
        let roots = match self.kind {
            Kind::ClaudeCode => self.deps.roots.claude_sessions(),
            Kind::Codex => self.deps.roots.codex_sessions(),
        };
        Setup {
            roots,
            executor: Some(self.deps.executor.clone()),
            cache: Some(self.deps.cache.app()),
            watcher: Some(self.deps.watch.clone()),
            projects: ProjectsRef::Service(ctx.service::<dyn Projects>()),
            filter: self.deps.filter.clone(),
            adapter_version,
        }
    }
}

#[cfg(test)]
impl SessionPlugin {
    fn keep<A: Adapter>(&self, engine: &Arc<Engine<A>>) {
        *self.engine.lock().unwrap() = Some(engine.clone());
    }
}

impl Plugin for SessionPlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: vec![service::<dyn SessionSource>()],
            requires: vec![service::<dyn Projects>()],
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    /// 只建空的会话列表、提供服务、订阅 `ProjectEvent`；不做 IO，不提交任务。
    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        match self.kind {
            Kind::ClaudeCode => {
                let setup = self.setup(ctx, claude::ADAPTER_VERSION);
                let engine = Engine::new(
                    |_| ClaudeCode::new(SourceId(claude::SOURCE_ID.into())),
                    setup,
                );
                #[cfg(test)]
                self.keep(&engine);
                activate(engine, ctx)
            }
            Kind::Codex => {
                let setup = self.setup(ctx, codex::ADAPTER_VERSION);
                let decoded = self.deps.decoded_dir.clone();
                let engine = Engine::new(
                    move |me| {
                        Codex::new(SourceId(codex::SOURCE_ID.into()), decoded)
                            .with_lineage(Arc::new(ListLineage(me.clone())))
                    },
                    setup,
                );
                #[cfg(test)]
                self.keep(&engine);
                activate(engine, ctx)
            }
        }
    }
}

fn activate<A: Adapter>(
    engine: Arc<Engine<A>>,
    ctx: &mut Context<'_>,
) -> Result<Box<dyn Active>, PluginError> {
    ctx.provide::<dyn SessionSource>(engine.clone())?;
    let listener = engine.clone();
    ctx.on::<ProjectEvent>(move |event, _| listener.on_project_event(event));
    Ok(Box::new(SourceActive { engine }))
}

struct SourceActive<A: Adapter> {
    engine: Arc<Engine<A>>,
}

impl<A: Adapter> Active for SourceActive<A> {
    /// 注册目录监听，提交第一次发现。
    fn started(&mut self, handle: PluginHandle) {
        self.engine.start(Some(handle));
    }
}

impl<A: Adapter> Drop for SourceActive<A> {
    fn drop(&mut self) {
        self.engine.stop();
    }
}

/// 分叉与子 agent 的来源会话按线程 ID 在当前会话列表里找（「外部格式」的「继承前缀」）。
pub(crate) struct ListLineage(pub(crate) Weak<Engine<Codex>>);

impl Lineage for ListLineage {
    fn locate(&self, thread_id: &str) -> Option<codex::SessionFile> {
        self.0.upgrade()?.file_by_id(thread_id)
    }
}
