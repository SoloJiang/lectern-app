//! 插件 "conversations"（docs/design/agents.md「ACP 客户端」的「插件与模块」）。

use std::ffi::OsString;
use std::sync::Arc;

use lectern_executor::Executor;
use lectern_kernel::{
    Activation, Active, Context, Manifest, Name, Plugin, PluginError, PluginHandle, service,
};
use lectern_services::conversation::Conversations;
use lectern_services::project::{ProjectEvent, Projects};
use lectern_services::session::{SessionSource, SessionsChanged};

use super::engine::{Engine, Open, Setup, Sources};
use super::program::Env;

/// 插件 ID，同时是 `provides` 的服务 `dyn Conversations` 的提供者。
pub const PLUGIN_ID: &str = "conversations";

/// `app` 交给插件的资源。进程级的资源由 `app` 创建一次（ADR 0025），插件不创建。
pub struct ConversationsDeps {
    pub executor: Executor,
}

/// 插件 "conversations"：provides `dyn Conversations`，requires `dyn SessionSource`、`dyn Projects`，
/// 激活条件 `AfterFirstFrame`。构造和 `activate` 不做 IO、不提交任务；没有发送之前不启动进程（ADR 0050）。
pub struct ConversationsPlugin {
    deps: ConversationsDeps,
    env: Env,
    #[cfg(test)]
    pub(crate) engine: Arc<std::sync::Mutex<Option<Arc<Engine>>>>,
}

/// 插件和它的 ID，`app` 放进 `PluginSet::with`。
pub fn conversations_plugin(deps: ConversationsDeps) -> (Name, ConversationsPlugin) {
    (
        Name::Borrowed(PLUGIN_ID),
        ConversationsPlugin {
            deps,
            env: Env::Login,
            #[cfg(test)]
            engine: Arc::default(),
        },
    )
}

impl ConversationsPlugin {
    /// 在这份环境里找 ACP 程序，不运行用户的登录 shell（测试，以及已经取得环境的装配）。
    #[must_use]
    pub fn with_login_env(mut self, vars: Vec<(OsString, OsString)>) -> ConversationsPlugin {
        self.env = Env::Fixed(vars.into());
        self
    }
}

impl Plugin for ConversationsPlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: vec![service::<dyn Conversations>()],
            requires: vec![service::<dyn SessionSource>(), service::<dyn Projects>()],
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    /// 只建空的状态、提供服务、订阅 `SessionsChanged` 和 `ProjectEvent`。
    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        let engine = Engine::new(Setup {
            executor: Some(self.deps.executor.clone()),
            sources: Sources::Service(ctx.service_set::<dyn SessionSource>()),
            projects: Open::Service(ctx.service::<dyn Projects>()),
            env: self.env.clone(),
            #[cfg(test)]
            sink: None,
        });
        #[cfg(test)]
        {
            *self
                .engine
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(engine.clone());
        }
        ctx.provide::<dyn Conversations>(engine.clone())?;
        let listener = engine.clone();
        ctx.on::<SessionsChanged>(move |event, _| listener.on_sessions_changed(event));
        let listener = engine.clone();
        ctx.on::<ProjectEvent>(move |_, _| listener.on_projects_changed());
        Ok(Box::new(ConversationsActive { engine }))
    }
}

struct ConversationsActive {
    engine: Arc<Engine>,
}

impl Active for ConversationsActive {
    fn started(&mut self, handle: PluginHandle) {
        self.engine.start(handle);
    }
}

impl Drop for ConversationsActive {
    fn drop(&mut self) {
        self.engine.stop();
    }
}
