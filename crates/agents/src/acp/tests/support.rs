//! 测试夹具：假的 `SessionSource` 与 `Projects`、会话和轮次的构造、记录事件的引擎。全部是合成数据，
//! 不读真实的会话目录。

use std::path::Path;
use std::sync::{Arc, Mutex};

use lectern_kernel::SystemClock;
use lectern_services::conversation::ConversationEvent;
use lectern_services::session::{Agent, SessionKey, SessionSource, SessionsChanged, SourceId};

use crate::acp::engine::{Engine, Open, Setup, Sources};
use crate::acp::program::Env;

pub(crate) use crate::testing::acp::*;

/// 一个引擎和它的假依赖，同步运行（没有 executor），事件记在 `events` 里。
pub(crate) struct Rig {
    pub(crate) engine: Arc<Engine>,
    pub(crate) claude: Arc<FakeSource>,
    pub(crate) codex: Arc<FakeSource>,
    pub(crate) projects: Arc<FakeProjects>,
    pub(crate) events: Arc<Mutex<Vec<ConversationEvent>>>,
}

impl Rig {
    /// `path` 是查找 ACP 程序用的 `PATH`。
    pub(crate) fn new(path: &Path) -> Rig {
        Rig::with_vars(vec![("PATH".into(), path.into())])
    }

    /// 查找用的环境变量由测试给出。
    pub(crate) fn with_vars(vars: Vec<(std::ffi::OsString, std::ffi::OsString)>) -> Rig {
        let claude = FakeSource::new(claude_source(), Agent::ClaudeCode);
        let codex = FakeSource::new(codex_source(), Agent::Codex);
        let projects = Arc::new(FakeProjects::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink_events = events.clone();
        let engine = Engine::new(Setup {
            executor: None,
            sources: Sources::Fixed(vec![claude.clone(), codex.clone()]),
            projects: Open::Fixed(projects.clone()),
            env: Env::Fixed(vars.into()),
            clock: Arc::new(SystemClock),
            sink: Some(Arc::new(move |event| {
                sink_events.lock().unwrap().push(event);
            })),
        });
        Rig {
            engine,
            claude,
            codex,
            projects,
            events,
        }
    }

    /// 一个放在已打开项目里、可以恢复的 Claude Code 会话，带两轮。
    pub(crate) fn claude_session(&self) -> SessionKey {
        let session = key(&claude_source(), CLAUDE_SESSION);
        let project = folder("/work/project");
        self.projects.set_open(vec![project.clone()]);
        self.claude.add(
            &session,
            Spec::located(&session, project, &Agent::ClaudeCode),
        );
        let turns = vec![
            self.claude.turn(1, "first prompt", vec![]),
            self.claude
                .turn(2, "second prompt", vec![self.claude.agent_text("a reply")]),
        ];
        self.claude.set_turns(&session, turns);
        session
    }

    pub(crate) fn take_events(&self) -> Vec<ConversationEvent> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }
}

/// 另一组来源的引擎（同步运行，没有事件记录）。
pub(crate) fn engine_with(
    sources: Vec<Arc<FakeSource>>,
    projects: Arc<FakeProjects>,
    path: &Path,
) -> Arc<Engine> {
    Engine::new(Setup {
        executor: None,
        sources: Sources::Fixed(
            sources
                .into_iter()
                .map(|s| s as Arc<dyn SessionSource>)
                .collect(),
        ),
        projects: Open::Fixed(projects),
        env: Env::Fixed(vec![("PATH".into(), path.into())].into()),
        clock: Arc::new(SystemClock),
        sink: None,
    })
}

/// 在 `dir` 里放一个可执行文件。
pub(crate) fn install(dir: &Path, name: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// 来源 `source` 报告 `key` 变了。
pub(crate) fn changed(source: &SourceId, key: &SessionKey) -> SessionsChanged {
    SessionsChanged {
        source: source.clone(),
        generation: 1,
        changed: vec![key.clone()].into(),
        removed: Arc::from([]),
    }
}
