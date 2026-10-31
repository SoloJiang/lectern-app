//! `Conversations` 的实现：每个被关注的会话一份状态和快照
//! （docs/design/agents.md「ACP 客户端」的「会话的状态与快照」）。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::time::Instant;

use lectern_executor::{Executor, Priority};
use lectern_kernel::{PluginHandle, ServiceRef, ServiceSet};
use lectern_logging::Value;
use lectern_services::conversation::{
    AcpProgram, ConversationCommand, ConversationEvent, ConversationInterest, ConversationSnapshot,
    Conversations, Entry, HistoryError, Load, Unavailable,
};
use lectern_services::project::Projects;
use lectern_services::session::{
    Agent, Place, ResumeCommand, SessionKey, SessionSource, SessionsChanged,
};

use super::history::{self, Window};
use super::program::{Env, is_canonical_uuid, program_of_source};
use crate::diag;

/// 进程与协议（设计文档第 7 步）还没有实现：`can_prompt` 对通过检查的会话是 `Unsupported`。
/// 第 7 步删掉这个常量。
const CONNECTIONS: bool = false;
/// `LoadEarlier` 每次多读的轮数与上限。
const STEP: u32 = 20;
const MOST: u32 = 200;

/// 会话来源：真实的 kernel 服务集合，或测试里固定的几个。
pub(crate) enum Sources {
    Service(ServiceSet<dyn SessionSource>),
    #[cfg(test)]
    Fixed(Vec<Arc<dyn SessionSource>>),
}

impl Sources {
    /// 对 ID 为 `id` 的提供者调用一次 `f`，期间持有它的准入。没有这个提供者时为 None。
    fn with<R>(
        &self,
        id: &lectern_services::session::SourceId,
        f: impl FnOnce(&dyn SessionSource) -> R,
    ) -> Option<R> {
        match self {
            Sources::Service(set) => {
                let providers = set.current();
                let provider = providers.iter().find(|p| p.id() == *id)?;
                Some(f(&**provider))
            }
            #[cfg(test)]
            Sources::Fixed(all) => all.iter().find(|p| p.id() == *id).map(|p| f(&**p)),
        }
    }
}

/// 已打开的项目。
pub(crate) enum Open {
    Service(ServiceRef<dyn Projects>),
    #[cfg(test)]
    Fixed(Arc<dyn Projects>),
}

impl Open {
    fn with<R>(&self, f: impl FnOnce(&dyn Projects) -> R) -> Option<R> {
        match self {
            Open::Service(service) => service.get().map(|p| f(&*p)),
            #[cfg(test)]
            Open::Fixed(p) => Some(f(&**p)),
        }
    }
}

pub(crate) struct Setup {
    /// None：在调用线程上同步运行（测试）。
    pub(crate) executor: Option<Executor>,
    pub(crate) sources: Sources,
    pub(crate) projects: Open,
    pub(crate) env: Env,
    /// 测试里代替 kernel 的事件发射。
    #[cfg(test)]
    pub(crate) sink: Option<Arc<dyn Fn(ConversationEvent) + Send + Sync>>,
}

/// 程序的查找状态。
#[derive(Clone, PartialEq, Eq, Debug)]
enum Lookup {
    Unknown,
    Searching,
    /// 第 7 步从这个路径启动程序。
    Found(std::path::PathBuf),
    Missing,
}

/// 不读状态就能得出的事实：重算在来源列表或项目变化之后。
#[derive(Clone, PartialEq, Eq, Debug)]
struct Facts {
    agent: Agent,
    /// 这个会话能不能经 ACP 恢复（`can_prompt` 的前两项），可以时是要用的程序。
    gate: Result<AcpProgram, Unavailable>,
    /// 提供者在。
    source_found: bool,
}

struct Conv {
    interests: HashSet<u64>,
    /// 要读的轮次数。
    wanted: u32,
    /// 一个读取正在 executor 上；期间的新请求记在 `reread`。
    reading: bool,
    reread: bool,
    facts: Facts,
    load: Load,
    window: Window,
    snapshot: Arc<ConversationSnapshot>,
}

struct State {
    sessions: HashMap<SessionKey, Conv>,
    programs: HashMap<AcpProgram, Lookup>,
    next_interest: u64,
    stopped: bool,
    waiting: Arc<[SessionKey]>,
}

pub(crate) struct Engine {
    me: Weak<Engine>,
    setup: Setup,
    host: OnceLock<PluginHandle>,
    state: Mutex<State>,
}

impl Engine {
    pub(crate) fn new(setup: Setup) -> Arc<Engine> {
        Arc::new_cyclic(|me| Engine {
            me: me.clone(),
            setup,
            host: OnceLock::new(),
            state: Mutex::new(State {
                sessions: HashMap::new(),
                programs: HashMap::new(),
                next_interest: 0,
                stopped: false,
                waiting: Arc::from([]),
            }),
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 插件已启动：之后可以提交任务和发事件。启动之前被关注的会话现在开始读。
    pub(crate) fn start(&self, handle: PluginHandle) {
        let _ = self.host.set(handle);
        let keys: Vec<SessionKey> = self.state().sessions.keys().cloned().collect();
        for key in keys {
            self.request_read(&key);
        }
        self.start_lookups(false);
    }

    /// 插件停用：不再提交任务，丢掉全部状态（关注的回调随后什么也不做）。
    pub(crate) fn stop(&self) {
        let mut state = self.state();
        state.stopped = true;
        state.sessions.clear();
    }

    pub(crate) fn on_sessions_changed(&self, event: &SessionsChanged) {
        let keys: Vec<SessionKey> = {
            let state = self.state();
            event
                .changed
                .iter()
                .chain(event.removed.iter())
                .filter(|key| state.sessions.contains_key(key))
                .cloned()
                .collect()
        };
        if keys.is_empty() {
            return;
        }
        self.refresh(Some(&keys));
        for key in &keys {
            self.request_read(key);
        }
    }

    /// 已打开的项目变了：`can_prompt` 的 `OutsideProject` 随之变化。
    pub(crate) fn on_projects_changed(&self) {
        self.refresh(None);
    }

    // ---- 事实与快照 ----

    /// 会话的事实。不取状态锁。
    fn facts(&self, key: &SessionKey) -> Facts {
        let found = self.setup.sources.with(&key.source, |source| {
            let agent = source.agent();
            let gate = self.gate(source, key);
            (agent, gate)
        });
        match found {
            Some((agent, gate)) => Facts {
                agent,
                gate,
                source_found: true,
            },
            None => Facts {
                agent: Agent::Other(key.source.0.clone()),
                gate: Err(Unavailable::NotResumable),
                source_found: false,
            },
        }
    }

    /// `can_prompt` 的前两项：与 `terminal` 对 `Launch::Resume` 的检查相同。
    fn gate(
        &self,
        source: &dyn SessionSource,
        key: &SessionKey,
    ) -> Result<AcpProgram, Unavailable> {
        let program = program_of_source(&key.source).ok_or(Unavailable::NotResumable)?;
        let list = source.sessions();
        let summary = list.get(key).ok_or(Unavailable::NotResumable)?;
        if summary.parent.is_some() {
            return Err(Unavailable::NotResumable);
        }
        let id = match (source.resume(key), program) {
            (Some(ResumeCommand::ClaudeCode { session_id }), AcpProgram::ClaudeCode)
            | (Some(ResumeCommand::Codex { session_id }), AcpProgram::Codex) => session_id,
            _ => return Err(Unavailable::NotResumable),
        };
        if *id != *key.id || !is_canonical_uuid(&id) {
            return Err(Unavailable::NotResumable);
        }
        let Place::Located(location) = &summary.place else {
            return Err(Unavailable::OutsideProject);
        };
        let open = self
            .setup
            .projects
            .with(|projects| projects.open_projects().contains(&location.project))
            .unwrap_or(false);
        if open {
            Ok(program)
        } else {
            Err(Unavailable::OutsideProject)
        }
    }

    fn can_prompt(state: &State, facts: &Facts) -> Result<(), Unavailable> {
        let program = facts.gate.clone()?;
        if state.programs.get(&program) == Some(&Lookup::Missing) {
            return Err(Unavailable::NotInstalled(program));
        }
        if CONNECTIONS {
            Ok(())
        } else {
            Err(Unavailable::Unsupported)
        }
    }

    /// 按状态重建一个会话的快照；内容没变（`generation` 除外）时不换，返回是否换了。
    fn rebuild(state: &mut State, key: &SessionKey) -> bool {
        let Some(conv) = state.sessions.get(key) else {
            return false;
        };
        let can_prompt = Self::can_prompt(state, &conv.facts);
        let conv = state.sessions.get_mut(key).expect("checked above");
        let old = &conv.snapshot;
        let new = ConversationSnapshot {
            session: key.clone(),
            generation: old.generation,
            agent: conv.facts.agent.clone(),
            load: conv.load,
            link: old.link.clone(),
            can_prompt,
            context: old.context,
            earlier: conv.window.earlier,
            entries: conv.window.entries.clone(),
            plan: old.plan.clone(),
            permissions: old.permissions.clone(),
        };
        if new == **old {
            return false;
        }
        conv.snapshot = Arc::new(ConversationSnapshot {
            generation: old.generation + 1,
            ..new
        });
        true
    }

    fn changed(state: &State, key: &SessionKey) -> Option<ConversationEvent> {
        state
            .sessions
            .get(key)
            .map(|conv| ConversationEvent::Changed {
                session: key.clone(),
                generation: conv.snapshot.generation,
            })
    }

    fn emit(&self, events: Vec<ConversationEvent>) {
        for event in events {
            #[cfg(test)]
            if let Some(sink) = &self.setup.sink {
                sink(event);
                continue;
            }
            if let Some(handle) = self.host.get() {
                handle.emitter().emit(event);
            }
        }
    }

    /// 重算事实（`keys` 为 None 时对全部被关注的会话），换出变化了的快照。
    fn refresh(&self, keys: Option<&[SessionKey]>) {
        let keys: Vec<SessionKey> = match keys {
            Some(keys) => keys.to_vec(),
            None => self.state().sessions.keys().cloned().collect(),
        };
        let facts: Vec<(SessionKey, Facts)> = keys
            .into_iter()
            .map(|key| {
                let facts = self.facts(&key);
                (key, facts)
            })
            .collect();
        let mut events = Vec::new();
        {
            let mut state = self.state();
            for (key, facts) in facts {
                let Some(conv) = state.sessions.get_mut(&key) else {
                    continue;
                };
                if conv.facts != facts {
                    conv.facts = facts;
                }
                if Self::rebuild(&mut state, &key) {
                    events.extend(Self::changed(&state, &key));
                }
            }
        }
        self.emit(events);
        self.start_lookups(false);
    }

    // ---- 任务 ----

    /// 在 executor 上运行 `work`，带一个 `BusyGuard`；没有 executor 时在调用线程上同步运行。
    /// 返回 false：插件还没有启动或已停用，没有运行。
    fn spawn(&self, priority: Priority, work: impl FnOnce(&Arc<Engine>) + Send + 'static) -> bool {
        let Some(executor) = &self.setup.executor else {
            if let Some(me) = self.me.upgrade() {
                work(&me);
            }
            return true;
        };
        let Some(handle) = self.host.get() else {
            return false;
        };
        let Some(busy) = handle.busy() else {
            return false;
        };
        let executor = executor.clone();
        let me = self.me.clone();
        handle.own_with(move |gate| {
            executor.run_gated(&gate, priority, move || {
                let _busy = busy;
                if let Some(engine) = me.upgrade() {
                    work(&engine);
                }
            })
        })
    }

    /// 请求读一个会话的历史窗口；已有一个读取在进行时，记下读完再读一次。
    fn request_read(&self, key: &SessionKey) {
        {
            let mut state = self.state();
            if state.stopped {
                return;
            }
            let Some(conv) = state.sessions.get_mut(key) else {
                return;
            };
            if conv.reading {
                conv.reread = true;
                return;
            }
            conv.reading = true;
        }
        let job_key = key.clone();
        if !self.spawn(Priority::Interactive, move |engine| {
            engine.read_loop(&job_key)
        }) && let Some(conv) = self.state().sessions.get_mut(key)
        {
            conv.reading = false;
        }
    }

    /// executor 上：读，换出快照，期间有新请求就再读。
    fn read_loop(&self, key: &SessionKey) {
        loop {
            let wanted = {
                let mut state = self.state();
                let Some(conv) = state.sessions.get_mut(key) else {
                    return;
                };
                conv.reread = false;
                conv.wanted
            };
            let _scope = lectern_logging::scope(diag::session_ctx(&key.id));
            let started = Instant::now();
            let result = self
                .setup
                .sources
                .with(&key.source, |source| {
                    history::read_window(source, key, wanted)
                })
                .unwrap_or(Err(HistoryError::NotFound));
            let took_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
            match &result {
                Ok(window) => lectern_logging::debug!(
                    "conversation history read";
                    turns = wanted,
                    entries = window.entries.len(),
                    earlier = window.earlier,
                    took_us = took_us,
                ),
                Err(error) => lectern_logging::debug!(
                    "conversation history failed";
                    error = history_error_name(*error),
                ),
            }
            let mut events = Vec::new();
            let again = {
                let mut state = self.state();
                let Some(conv) = state.sessions.get_mut(key) else {
                    return;
                };
                match result {
                    Ok(window) => {
                        conv.load = Load::Ready;
                        conv.window = window;
                    }
                    Err(error) => {
                        conv.load = Load::Failed(error);
                        conv.window = Window::default();
                    }
                }
                let again = conv.reread;
                if !again {
                    conv.reading = false;
                }
                if Self::rebuild(&mut state, key) {
                    events.extend(Self::changed(&state, key));
                }
                again
            };
            self.emit(events);
            if !again {
                return;
            }
        }
    }

    /// 对状态是 `Unknown` 的程序（`retry_missing` 时也对 `Missing` 的）开始查找，但只为有会话要用它的程序。
    fn start_lookups(&self, retry_missing: bool) {
        let wanted: Vec<AcpProgram> = {
            let mut state = self.state();
            if state.stopped {
                return;
            }
            let needed: HashSet<AcpProgram> = state
                .sessions
                .values()
                .filter_map(|conv| conv.facts.gate.clone().ok())
                .collect();
            let mut start = Vec::new();
            for program in needed {
                let current = state
                    .programs
                    .get(&program)
                    .cloned()
                    .unwrap_or(Lookup::Unknown);
                if current == Lookup::Unknown || (retry_missing && current == Lookup::Missing) {
                    state.programs.insert(program, Lookup::Searching);
                    start.push(program);
                }
            }
            start
        };
        for program in wanted {
            let submitted = self.spawn(Priority::Background, move |engine| engine.lookup(program));
            if !submitted {
                // 插件还没有启动：`start` 再来一次。
                self.state().programs.insert(program, Lookup::Unknown);
            }
        }
    }

    /// executor 上：在登录环境的 PATH 里找程序，换出受影响的会话的快照。
    fn lookup(&self, program: AcpProgram) {
        let started = Instant::now();
        let found = self.setup.env.find(program);
        lectern_logging::debug!(
            "conversation program lookup";
            program = program_name(program),
            found = found.is_some(),
            took_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
        );
        let mut events = Vec::new();
        {
            let mut state = self.state();
            if state.stopped {
                return;
            }
            state.programs.insert(
                program,
                match found {
                    Some(path) => Lookup::Found(path),
                    None => Lookup::Missing,
                },
            );
            let keys: Vec<SessionKey> = state
                .sessions
                .iter()
                .filter(|(_, conv)| conv.facts.gate == Ok(program))
                .map(|(key, _)| key.clone())
                .collect();
            for key in keys {
                if Self::rebuild(&mut state, &key) {
                    events.extend(Self::changed(&state, &key));
                }
            }
        }
        self.emit(events);
    }

    fn release(&self, key: &SessionKey, interest: u64) {
        let mut state = self.state();
        let Some(conv) = state.sessions.get_mut(key) else {
            return;
        };
        conv.interests.remove(&interest);
        if conv.interests.is_empty() {
            // 第 7 步起，有连接的会话保留状态。
            state.sessions.remove(key);
        }
    }
}

impl Conversations for Engine {
    fn watch(&self, session: &SessionKey) -> ConversationInterest {
        let facts = self.facts(session);
        let (interest, events) = {
            let mut state = self.state();
            if state.stopped {
                return ConversationInterest::new(|| {});
            }
            let interest = state.next_interest;
            state.next_interest += 1;
            let mut events = Vec::new();
            if !state.sessions.contains_key(session) {
                let load = if facts.source_found {
                    Load::Loading
                } else {
                    Load::Failed(HistoryError::NotFound)
                };
                let can_prompt = Self::can_prompt(&state, &facts);
                let snapshot = Arc::new(ConversationSnapshot {
                    session: session.clone(),
                    generation: 1,
                    agent: facts.agent.clone(),
                    load,
                    link: lectern_services::conversation::Link::None,
                    can_prompt,
                    context: None,
                    earlier: 0,
                    entries: Arc::<[Entry]>::from([]),
                    plan: None,
                    permissions: Arc::from([]),
                });
                state.sessions.insert(
                    session.clone(),
                    Conv {
                        interests: HashSet::new(),
                        wanted: STEP,
                        reading: false,
                        reread: false,
                        facts: facts.clone(),
                        load,
                        window: Window::default(),
                        snapshot,
                    },
                );
                events.extend(Self::changed(&state, session));
            }
            state
                .sessions
                .get_mut(session)
                .expect("inserted above")
                .interests
                .insert(interest);
            (interest, events)
        };
        let _scope = lectern_logging::scope(diag::session_ctx(&session.id));
        lectern_logging::debug!("conversation watched"; wanted = STEP);
        self.emit(events);
        if facts.source_found {
            self.request_read(session);
        } else {
            lectern_logging::debug!(
                "conversation history failed";
                error = history_error_name(HistoryError::NotFound),
            );
        }
        // 没找到的程序在下一次 `watch` 重新查找（登录环境不重取）。
        self.start_lookups(true);
        let me = self.me.clone();
        let key = session.clone();
        ConversationInterest::new(move || {
            if let Some(engine) = me.upgrade() {
                engine.release(&key, interest);
            }
        })
    }

    fn snapshot(&self, session: &SessionKey) -> Option<Arc<ConversationSnapshot>> {
        self.state()
            .sessions
            .get(session)
            .map(|conv| conv.snapshot.clone())
    }

    fn command(&self, session: &SessionKey, command: ConversationCommand) {
        if matches!(command, ConversationCommand::LoadEarlier) {
            let more = {
                let mut state = self.state();
                match state.sessions.get_mut(session) {
                    Some(conv) if conv.window.earlier > 0 && conv.wanted < MOST => {
                        conv.wanted = (conv.wanted + STEP).min(MOST);
                        true
                    }
                    _ => false,
                }
            };
            if more {
                self.request_read(session);
            }
            return;
        }
        let _scope = lectern_logging::scope(diag::session_ctx(&session.id));
        let reason = if self.state().sessions.contains_key(session) {
            "no_connection"
        } else {
            "not_watched"
        };
        lectern_logging::debug!(
            "conversation command ignored";
            command = command_name(&command),
            reason = reason,
        );
    }

    fn waiting(&self) -> Arc<[SessionKey]> {
        self.state().waiting.clone()
    }
}

fn history_error_name(error: HistoryError) -> Value<'static> {
    Value::text(match error {
        HistoryError::NotFound => "not_found",
        HistoryError::Unrecognized => "unrecognized",
        HistoryError::Io => "io",
    })
}

fn program_name(program: AcpProgram) -> Value<'static> {
    Value::text(match program {
        AcpProgram::ClaudeCode => "claude_code",
        AcpProgram::Codex => "codex",
    })
}

fn command_name(command: &ConversationCommand) -> Value<'static> {
    Value::text(match command {
        ConversationCommand::Prompt { .. } => "prompt",
        ConversationCommand::Cancel => "cancel",
        ConversationCommand::Answer { .. } => "answer",
        ConversationCommand::LoadEarlier => "load_earlier",
        ConversationCommand::Disconnect => "disconnect",
        ConversationCommand::Dismiss => "dismiss",
    })
}

#[cfg(test)]
impl Engine {
    pub(crate) fn lookup_state(&self, program: AcpProgram) -> Option<bool> {
        match self.state().programs.get(&program) {
            Some(Lookup::Found(_)) => Some(true),
            Some(Lookup::Missing) => Some(false),
            _ => None,
        }
    }

    pub(crate) fn watched(&self) -> usize {
        self.state().sessions.len()
    }
}
