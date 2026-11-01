//! 连接的生命周期：命令（`Prompt`、`Cancel`、`Answer`、`Disconnect`、`Dismiss`）、进程的启动、
//! JSON-RPC 消息的处理、计时器和关闭（docs/design/agents.md「ACP 客户端」的「启动」「运行、关闭、权限」）。
//!
//! 命令在调用线程上只改状态、换出快照，然后把 IO 交给 executor；读 agent 输出的线程（`conn::read_loop`）
//! 收到消息后在引擎的锁里改状态。所有会产生副作用的事（发事件、写管道、关进程、提交任务、设计时器）
//! 先记在 [`Effects`]，放锁之后由 [`Engine::run`] 执行，所以任何锁里都没有 IO，也没有回调。

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::task::{Wake, Waker};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lectern_executor::Priority;
use lectern_services::conversation::{
    CallKind, ConversationEvent, EditPreview, Link, LinkError, OptionId, OptionKind,
    PermissionDetail, PermissionId, PermissionOption, PermissionRequest, PromptOrigin,
};
use lectern_services::project::ProjectKey;
use lectern_services::session::{Place, SessionKey, Timestamp};
use serde_json::value::RawValue;

use super::conn::{self, Conn, End, Exit, Sink, Spec};
use super::engine::{COALESCE, Engine, IDLE, Lookup, State, ignored};
use super::live::{self, Live};
use super::wire::{
    self, AUTH_REQUIRED, Id, Incoming, Malformed, Method, PermissionAsk, Restore, Stop, Update,
};
use crate::diag;

/// `initialize` 和恢复会话各有这么久。
const HANDSHAKE: Duration = Duration::from_secs(30);
/// `session/cancel` 发出之后，适配器这么久还没有回复 `session/prompt`，连接以 `Protocol` 失败并关闭。
const CANCEL_WAIT: Duration = Duration::from_secs(30);
/// 关闭 stdin 并发 SIGTERM 之后，这么久还没退出就 SIGKILL。
const KILL_AFTER: Duration = Duration::from_secs(2);
/// Lectern 退出时等进程退出的时间上限。
const STOP_WAIT: Duration = Duration::from_millis(500);
/// 同时待确认的权限请求上限；再多的回答 `cancelled`。
const PERMISSIONS_MAX: usize = 16;
/// 命令文本、标题与错误文字的上限。
const COMMAND_MAX: usize = 16 << 10;
const TITLE_CHARS: usize = 512;
const MESSAGE_MAX: usize = 1 << 10;

/// 读线程在取锁之前备好的权限请求：参数，和工具调用内容里的改动预览。
type Prepared = Result<(PermissionAsk, Option<Arc<[EditPreview]>>), Malformed>;

/// 握手走到哪一步。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Phase {
    /// 没有进程，或进程还没有登记。
    Down,
    /// 已发 `initialize`。
    Initializing,
    /// 已发 `session/resume` 或 `session/load`。
    Restoring,
    /// 可以发 prompt。
    Ready,
}

/// 发出去还没有回复的请求。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Request {
    Initialize,
    Restore(Restore),
    Prompt,
}

/// 一个待确认的权限请求，和回复它要用的 JSON-RPC `id`。
struct Ask {
    request: PermissionRequest,
    rpc: Id,
    tool: String,
}

/// 一个会话的连接状态与实时内容。
pub(super) struct Run {
    pub(super) link: Link,
    pub(super) conn: Option<Arc<Conn>>,
    /// 正在启动的连接尝试（非零）；`start_task` 据此确认没有被取消。
    pub(super) attempt: u64,
    pub(super) phase: Phase,
    phase_seq: u64,
    /// `Cancel` 的次数：取消超时的计时器据此认出自己是不是这一次的。
    stop_seq: u64,
    requests: HashMap<i64, (Request, Instant)>,
    next_request: i64,
    /// 握手完成后发出的 prompt（它的实时项已经在列表里）。
    pub(super) queued: Option<Arc<str>>,
    /// 程序的查找还没完成，完成后启动。
    pub(super) awaiting_lookup: bool,
    permissions: Vec<Ask>,
    /// 进入空闲的时刻，和空闲计时器是否已设。
    pub(super) idle_since: Option<Instant>,
    idle_armed: bool,
    last_swap: Option<Instant>,
    flush_armed: bool,
    cwd: Option<String>,
    pub(super) live: Live,
}

impl Run {
    pub(super) fn new() -> Run {
        Run {
            link: Link::None,
            conn: None,
            attempt: 0,
            phase: Phase::Down,
            phase_seq: 0,
            stop_seq: 0,
            requests: HashMap::new(),
            next_request: 1,
            queued: None,
            awaiting_lookup: false,
            permissions: Vec::new(),
            idle_since: None,
            idle_armed: false,
            last_swap: None,
            flush_armed: false,
            cwd: None,
            live: Live::default(),
        }
    }

    /// 有进程，或正在启动一个（占一个连接名额）。
    pub(super) fn has_connection(&self) -> bool {
        self.conn.is_some() || self.attempt != 0 || self.link == Link::Connecting
    }

    /// 已连接、没有进行中的 prompt、没有待确认的请求：空闲计时中。
    pub(super) fn is_idle(&self) -> bool {
        self.conn.is_some()
            && self.phase == Phase::Ready
            && self.permissions.is_empty()
            && self.idle_since.is_some()
            && matches!(self.link, Link::Idle | Link::Failed(_))
    }

    /// 最早的待确认请求（`waiting()` 的排序依据）。
    pub(super) fn first_permission(&self) -> Option<PermissionId> {
        self.permissions.iter().map(|ask| ask.request.id).min()
    }

    pub(super) fn permission_requests(&self) -> Arc<[PermissionRequest]> {
        self.permissions
            .iter()
            .map(|ask| ask.request.clone())
            .collect()
    }

    fn new_request(&mut self, kind: Request) -> i64 {
        let id = self.next_request;
        self.next_request += 1;
        self.requests.insert(id, (kind, Instant::now()));
        id
    }

    /// 待确认的请求都回答 `cancelled`（`Cancel`、关闭连接）。
    fn cancel_permissions(&mut self, key: &SessionKey, fx: &mut Effects) {
        let asks = std::mem::take(&mut self.permissions);
        if let Some(conn) = &self.conn {
            for ask in asks {
                fx.send(
                    key,
                    conn,
                    Method::RequestPermission,
                    None,
                    wire::cancelled(&ask.rpc),
                );
            }
        }
    }

    /// 与进程的关系清零，实时项留着（`Failed` 时界面还要显示）。
    fn reset_connection(&mut self) {
        self.conn = None;
        self.attempt = 0;
        self.phase = Phase::Down;
        self.requests.clear();
        self.queued = None;
        self.awaiting_lookup = false;
        self.idle_since = None;
        self.idle_armed = false;
        self.cwd = None;
    }
}

/// 为什么关闭连接（日志的 `reason`）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Closed {
    Idle,
    Disconnect,
    Project,
    Evicted,
    Failure,
}

impl Closed {
    fn name(self) -> &'static str {
        match self {
            Closed::Idle => "idle",
            Closed::Disconnect => "disconnect",
            Closed::Project => "project",
            Closed::Evicted => "evicted",
            Closed::Failure => "failure",
        }
    }
}

/// 计时器到点时做什么。
pub(super) enum Timer {
    /// 握手超时：这个连接仍停在第 `seq` 步。
    Phase {
        key: SessionKey,
        conn: u64,
        seq: u64,
    },
    /// `Cancel` 之后适配器一直没有回复：这个连接仍停在第 `seq` 次取消的 `Stopping`。
    Stop {
        key: SessionKey,
        conn: u64,
        seq: u64,
    },
    Idle {
        key: SessionKey,
        conn: u64,
    },
    Kill(Arc<Conn>),
    Flush {
        key: SessionKey,
    },
}

/// 放锁之后执行的副作用。
#[derive(Default)]
pub(super) struct Effects {
    pub(super) events: Vec<ConversationEvent>,
    drains: Vec<Arc<Conn>>,
    closes: Vec<(Arc<Conn>, Closed)>,
    reads: Vec<SessionKey>,
    starts: Vec<(SessionKey, u64)>,
    timers: Vec<(Instant, Timer)>,
    pub(super) lookups: bool,
}

impl Effects {
    /// 一行放进连接的发送队列（日志：方法、`req`、字节数）。
    fn send(
        &mut self,
        key: &SessionKey,
        conn: &Arc<Conn>,
        method: Method,
        req: Option<u64>,
        line: Vec<u8>,
    ) {
        let bytes = line.len() as u64;
        let _scope = lectern_logging::scope(match req {
            Some(req) => diag::rpc_ctx(&key.id, req),
            None => diag::session_ctx(&key.id),
        });
        lectern_logging::debug!("acp message sent"; method = method.name(), bytes = bytes);
        if conn.push(line) && !self.drains.iter().any(|c| c.id == conn.id) {
            self.drains.push(conn.clone());
        }
    }
}

/// 时钟到点时调用一次闭包。
struct Wakeup(Mutex<Option<Box<dyn FnOnce() + Send>>>);

impl Wake for Wakeup {
    fn wake(self: Arc<Self>) {
        let work = self.0.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(work) = work {
            work();
        }
    }
}

/// `conn::read_loop` 的 [`Sink`]：把读线程的回调交给引擎。
struct Wire(std::sync::Weak<Engine>);

impl Sink for Wire {
    fn line(&self, conn: &Arc<Conn>, line: &[u8]) {
        if let Some(engine) = self.0.upgrade() {
            engine.on_line(conn, line);
        }
    }

    fn ended(&self, conn: &Arc<Conn>, end: End) {
        if let Some(engine) = self.0.upgrade() {
            engine.on_ended(conn, end);
        }
    }

    fn exited(&self, conn: &Arc<Conn>, exit: Exit) {
        if let Some(engine) = self.0.upgrade() {
            engine.on_exited(conn, exit);
        }
    }
}

fn now_ms() -> Timestamp {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    Timestamp(millis)
}

fn link_error_name(error: &LinkError) -> &'static str {
    match error {
        LinkError::Spawn => "spawn",
        LinkError::Unsupported => "unsupported",
        LinkError::LoadFailed => "load_failed",
        LinkError::Protocol => "protocol",
        LinkError::Exited { .. } => "exited",
        LinkError::AuthRequired => "auth_required",
        LinkError::PromptFailed { .. } => "prompt_failed",
    }
}

fn restore_method(restore: Restore) -> Method {
    match restore {
        Restore::Resume => Method::SessionResume,
        Restore::Load => Method::SessionLoad,
    }
}

impl Engine {
    // ---- 副作用 ----

    /// 执行 [`Effects`]。不持有状态锁时调用。
    pub(super) fn run(&self, fx: Effects) {
        self.emit(fx.events);
        for conn in fx.drains {
            let job = conn.clone();
            // Background: a write to an agent that does not read blocks its worker, and `Interactive`
            // keeps its own worker for the history reads.
            if !self.spawn(Priority::Background, move |_| job.drain()) {
                conn.drain();
            }
        }
        for (conn, why) in fx.closes {
            let job = conn.clone();
            if !self.spawn(Priority::Background, move |engine| {
                engine.close_conn(&job, why);
            }) {
                conn.close();
            }
        }
        for key in fx.reads {
            self.request_read(&key);
        }
        for (key, attempt) in fx.starts {
            let job = key.clone();
            if !self.spawn(Priority::Interactive, move |engine| {
                engine.start_task(&job, attempt);
            }) {
                self.start_failed(&key, attempt);
            }
        }
        for (at, timer) in fx.timers {
            self.arm(at, timer);
        }
        if fx.lookups {
            self.start_lookups(false);
        }
    }

    /// 设一个计时器。到点的回调在时钟的线程上运行（测试里是拨动时钟的线程），不得久占。
    fn arm(&self, at: Instant, timer: Timer) {
        let me = self.me.clone();
        let work: Box<dyn FnOnce() + Send> = Box::new(move || {
            if let Some(engine) = me.upgrade() {
                engine.fire(timer);
            }
        });
        self.setup
            .clock
            .wake_at(at, Waker::from(Arc::new(Wakeup(Mutex::new(Some(work))))));
    }

    fn fire(&self, timer: Timer) {
        let mut fx = Effects::default();
        match timer {
            Timer::Kill(conn) => {
                // 先是 SIGTERM；这么久还没退出的，进程组整个杀掉。
                conn.kill();
                return;
            }
            Timer::Phase { key, conn, seq } => {
                let mut state = self.state();
                let stale = state.sessions.get(&key).is_none_or(|c| {
                    c.run.conn.as_ref().is_none_or(|x| x.id != conn)
                        || c.run.phase_seq != seq
                        || !matches!(c.run.phase, Phase::Initializing | Phase::Restoring)
                });
                if stale {
                    return;
                }
                let _scope = lectern_logging::scope(diag::session_ctx(&key.id));
                self.fail(&mut state, &key, LinkError::Protocol, &mut fx);
                self.publish(&mut state, &key, true, &mut fx);
            }
            Timer::Stop { key, conn, seq } => {
                let mut state = self.state();
                let stale = state.sessions.get(&key).is_none_or(|c| {
                    c.run.conn.as_ref().is_none_or(|x| x.id != conn)
                        || c.run.stop_seq != seq
                        || c.run.link != Link::Stopping
                });
                if stale {
                    return;
                }
                let _scope = lectern_logging::scope(diag::session_ctx(&key.id));
                self.fail(&mut state, &key, LinkError::Protocol, &mut fx);
                self.publish(&mut state, &key, true, &mut fx);
            }
            Timer::Idle { key, conn } => {
                let mut state = self.state();
                let now = self.setup.clock.now();
                let Some(conv) = state.sessions.get_mut(&key) else {
                    return;
                };
                if conv.run.conn.as_ref().is_none_or(|x| x.id != conn) {
                    return;
                }
                if !conv.run.is_idle() {
                    conv.run.idle_armed = false;
                    return;
                }
                let since = conv.run.idle_since.unwrap_or(now);
                if now.saturating_duration_since(since) >= IDLE {
                    let _scope = lectern_logging::scope(diag::session_ctx(&key.id));
                    Self::close_session(&mut state, &key, Closed::Idle, &mut fx);
                    self.publish(&mut state, &key, true, &mut fx);
                } else {
                    fx.timers.push((since + IDLE, Timer::Idle { key, conn }));
                }
            }
            Timer::Flush { key } => {
                let mut state = self.state();
                let Some(conv) = state.sessions.get_mut(&key) else {
                    return;
                };
                conv.run.flush_armed = false;
                self.publish(&mut state, &key, true, &mut fx);
            }
        }
        self.run(fx);
    }

    /// 换出快照。`urgent`（连接状态、权限、一轮的开始与结束）时马上换并重算所有会话的 `can_prompt`；
    /// 否则（文字块、工具更新、计划）合并：距上次换出不到 50 ms 时设一个计时器，到点再换。
    pub(super) fn publish(
        &self,
        state: &mut State,
        key: &SessionKey,
        urgent: bool,
        fx: &mut Effects,
    ) {
        let now = self.setup.clock.now();
        let Some(conv) = state.sessions.get_mut(key) else {
            // The session's state went with its connection; the others' `can_prompt` may still change.
            if urgent {
                Self::rebuild_all(state, &mut fx.events);
            }
            return;
        };
        let due = urgent
            || conv
                .run
                .last_swap
                .is_none_or(|at| now.saturating_duration_since(at) >= COALESCE);
        if !due {
            if !conv.run.flush_armed {
                conv.run.flush_armed = true;
                let at = conv.run.last_swap.unwrap_or(now) + COALESCE;
                fx.timers.push((at, Timer::Flush { key: key.clone() }));
            }
            return;
        }
        conv.run.last_swap = Some(now);
        if urgent {
            Self::rebuild_all(state, &mut fx.events);
        } else if Self::rebuild(state, key) {
            fx.events.extend(Self::changed(state, key));
        }
    }

    // ---- 命令 ----

    /// `Prompt`：有可用的连接就直接发，否则启动一个（程序的查找没完成就等它）。
    pub(super) fn prompt(&self, session: &SessionKey, text: &str, origin: PromptOrigin) {
        let mut fx = Effects::default();
        {
            let mut state = self.state();
            if state.stopped {
                return;
            }
            let Some(conv) = state.sessions.get(session) else {
                ignored("prompt", "not_watched");
                return;
            };
            if Self::can_prompt(&state, conv).is_err() {
                ignored("prompt", "unavailable");
                return;
            }
            let reuse = conv.run.conn.is_some() && conv.run.phase == Phase::Ready;
            lectern_logging::debug!(
                "conversation prompt accepted";
                bytes = text.len() as u64,
                connects = !reuse,
            );
            if reuse {
                let conv = state.sessions.get_mut(session).expect("checked above");
                conv.run.live.begin_prompt(text, origin, now_ms().0);
                Self::dispatch_prompt(session, &mut conv.run, text, &mut fx);
            } else {
                if state.connections_full()
                    && let Some(oldest) = state.oldest_idle()
                {
                    Self::close_session(&mut state, &oldest, Closed::Evicted, &mut fx);
                }
                let conv = state.sessions.get_mut(session).expect("checked above");
                conv.run.live.begin_prompt(text, origin, now_ms().0);
                conv.run.link = Link::Connecting;
                conv.run.queued = Some(text.into());
                Self::plan_start(&mut state, session, &mut fx);
            }
            self.publish(&mut state, session, true, &mut fx);
        }
        self.run(fx);
    }

    /// 在已连接的会话上发出一个 prompt 请求，`link` 变为 `Working`。
    fn dispatch_prompt(key: &SessionKey, run: &mut Run, text: &str, fx: &mut Effects) {
        run.link = Link::Working;
        run.idle_since = None;
        let Some(conn) = run.conn.clone() else {
            return;
        };
        let id = run.new_request(Request::Prompt);
        fx.send(
            key,
            &conn,
            Method::SessionPrompt,
            Some(id as u64),
            wire::prompt(id, &key.id, text),
        );
    }

    /// 要启动一个新连接：程序找到了就提交启动任务；还在查找就等；找不到是 `Failed(Spawn)`。
    fn plan_start(state: &mut State, key: &SessionKey, fx: &mut Effects) {
        let Some(conv) = state.sessions.get(key) else {
            return;
        };
        let Ok(program) = conv.facts.gate.clone() else {
            return;
        };
        match state.programs.get(&program) {
            Some(Lookup::Found(_)) => {
                let attempt = state.next_attempt;
                state.next_attempt += 1;
                if let Some(conv) = state.sessions.get_mut(key) {
                    conv.run.attempt = attempt;
                    conv.run.awaiting_lookup = false;
                }
                fx.starts.push((key.clone(), attempt));
            }
            Some(Lookup::Missing) => {
                if let Some(conv) = state.sessions.get_mut(key) {
                    conv.run.queued = None;
                    conv.run.link = Link::Failed(LinkError::Spawn);
                }
            }
            _ => {
                if let Some(conv) = state.sessions.get_mut(key) {
                    conv.run.awaiting_lookup = true;
                }
                fx.lookups = true;
            }
        }
    }

    /// 程序的查找完成了：等它的 prompt 现在启动，找不到就是 `Failed(Spawn)`。
    pub(super) fn resolve_waiting(
        state: &mut State,
        key: &SessionKey,
        found: bool,
        fx: &mut Effects,
    ) {
        if let Some(conv) = state.sessions.get_mut(key) {
            conv.run.awaiting_lookup = false;
            if !found {
                conv.run.queued = None;
                conv.run.link = Link::Failed(LinkError::Spawn);
                return;
            }
        }
        Self::plan_start(state, key, fx);
    }

    /// `Cancel`：只在 `Working` 时有效。待确认的请求回答 `cancelled`，发 `session/cancel`，`link` 为 `Stopping`。
    /// 适配器 30 秒内没有回复 `session/prompt`（不理会 `session/cancel`）时，连接以 `Protocol` 失败并关闭。
    pub(super) fn cancel(&self, session: &SessionKey) {
        let mut fx = Effects::default();
        {
            let mut state = self.state();
            let Some(conv) = state.sessions.get_mut(session) else {
                ignored("cancel", "not_watched");
                return;
            };
            let (Link::Working, Some(conn)) = (&conv.run.link, conv.run.conn.clone()) else {
                ignored("cancel", "not_working");
                return;
            };
            conv.run.cancel_permissions(session, &mut fx);
            conv.run.link = Link::Stopping;
            conv.run.stop_seq += 1;
            fx.timers.push((
                self.setup.clock.now() + CANCEL_WAIT,
                Timer::Stop {
                    key: session.clone(),
                    conn: conn.id,
                    seq: conv.run.stop_seq,
                },
            ));
            fx.send(
                session,
                &conn,
                Method::SessionCancel,
                None,
                wire::cancel(&session.id),
            );
            self.publish(&mut state, session, true, &mut fx);
        }
        self.run(fx);
    }

    /// `Answer`：回答一个待确认的请求。
    pub(super) fn answer(&self, session: &SessionKey, request: PermissionId, option: &OptionId) {
        let mut fx = Effects::default();
        {
            let mut state = self.state();
            let Some(conv) = state.sessions.get_mut(session) else {
                ignored("answer", "not_watched");
                return;
            };
            let Some(at) = conv
                .run
                .permissions
                .iter()
                .position(|ask| ask.request.id == request)
            else {
                ignored("answer", "unknown_request");
                return;
            };
            let Some(chosen) = conv.run.permissions[at]
                .request
                .options
                .iter()
                .find(|o| o.id == *option)
                .cloned()
            else {
                ignored("answer", "unknown_option");
                return;
            };
            let ask = conv.run.permissions.remove(at);
            if let Some(conn) = conv.run.conn.clone() {
                fx.send(
                    session,
                    &conn,
                    Method::RequestPermission,
                    None,
                    wire::selected(&ask.rpc, &option.0),
                );
            }
            if matches!(
                chosen.kind,
                OptionKind::RejectOnce | OptionKind::RejectAlways
            ) {
                conv.run.live.deny(&ask.tool);
            }
            self.publish(&mut state, session, true, &mut fx);
        }
        self.run(fx);
    }

    /// `Disconnect`：`Idle` 或连接仍在的 `Failed` 时关闭连接；没有连接时只把 `Failed` 清为 `None`；
    /// `Connecting`、`Working`、`Stopping` 时忽略。
    pub(super) fn disconnect(&self, session: &SessionKey) {
        let mut fx = Effects::default();
        {
            let mut state = self.state();
            let Some(conv) = state.sessions.get_mut(session) else {
                ignored("disconnect", "not_watched");
                return;
            };
            match (&conv.run.link, conv.run.conn.is_some()) {
                (Link::Idle | Link::Failed(_), true) => {
                    Self::close_session(&mut state, session, Closed::Disconnect, &mut fx);
                }
                (Link::Failed(_), false) => conv.run.link = Link::None,
                (Link::None, _) => {
                    ignored("disconnect", "no_connection");
                    return;
                }
                _ => {
                    ignored("disconnect", "busy");
                    return;
                }
            }
            self.publish(&mut state, session, true, &mut fx);
        }
        self.run(fx);
    }

    /// `Dismiss`：清除 `Failed`；连接仍在时回到 `Idle`，否则回到 `None`。
    pub(super) fn dismiss(&self, session: &SessionKey) {
        let mut fx = Effects::default();
        {
            let mut state = self.state();
            let now = self.setup.clock.now();
            let Some(conv) = state.sessions.get_mut(session) else {
                ignored("dismiss", "not_watched");
                return;
            };
            if !matches!(conv.run.link, Link::Failed(_)) {
                ignored("dismiss", "not_failed");
                return;
            }
            if conv.run.conn.is_some() {
                conv.run.link = Link::Idle;
                Self::arm_idle(session, &mut conv.run, now, &mut fx);
            } else {
                conv.run.link = Link::None;
            }
            self.publish(&mut state, session, true, &mut fx);
        }
        self.run(fx);
    }

    // ---- 状态转换 ----

    /// 进入空闲：计时从现在起，计时器没设就设一个。
    fn arm_idle(key: &SessionKey, run: &mut Run, now: Instant, fx: &mut Effects) {
        run.idle_since = Some(now);
        if !run.idle_armed
            && let Some(conn) = &run.conn
        {
            run.idle_armed = true;
            fx.timers.push((
                now + IDLE,
                Timer::Idle {
                    key: key.clone(),
                    conn: conn.id,
                },
            ));
        }
    }

    /// 关闭一个会话的连接（空闲、`Disconnect`、项目关闭、让位）：待确认的请求回答 `cancelled`，
    /// 连接交给 `fx` 关闭，`link` 为 `None`，实时项清掉并重读历史（这一轮已写进本机日志）。
    pub(super) fn close_session(
        state: &mut State,
        key: &SessionKey,
        why: Closed,
        fx: &mut Effects,
    ) {
        let Some(conv) = state.sessions.get_mut(key) else {
            return;
        };
        conv.run.cancel_permissions(key, fx);
        let detached = conv.run.conn.take();
        conv.run.reset_connection();
        conv.run.link = Link::None;
        conv.run.live.clear();
        fx.reads.push(key.clone());
        if let Some(conn) = detached {
            fx.closes.push((conn.clone(), why));
            state.closing.insert(conn.id, conn);
        }
        Self::drop_if_unused(state, key);
    }

    /// 连接失败：`link` 为 `Failed(error)`，待确认的请求回答 `cancelled`，没有结果的工具调用是
    /// 「已取消」，进程关闭。实时项留着（界面在它们后面显示原因）。
    pub(super) fn fail(
        &self,
        state: &mut State,
        key: &SessionKey,
        error: LinkError,
        fx: &mut Effects,
    ) {
        let Some(conv) = state.sessions.get_mut(key) else {
            return;
        };
        lectern_logging::warn!("acp link failed"; error = link_error_name(&error));
        conv.run.live.abort();
        conv.run.cancel_permissions(key, fx);
        let detached = conv.run.conn.take();
        conv.run.reset_connection();
        conv.run.link = Link::Failed(error);
        // The turn may be in the log already: read the history again so it replaces the live items.
        fx.reads.push(key.clone());
        if let Some(conn) = detached {
            fx.closes.push((conn.clone(), Closed::Failure));
            state.closing.insert(conn.id, conn);
        }
    }

    // ---- 启动 ----

    /// executor 上：检查工作目录、启动进程、登记、开读线程、发 `initialize`。
    fn start_task(&self, key: &SessionKey, attempt: u64) {
        let _scope = lectern_logging::scope(diag::session_ctx(&key.id));
        let Some(launch) = self.launch_info(key, attempt) else {
            self.start_failed(key, attempt);
            return;
        };
        let Some(cwd_str) = launch.cwd.to_str().map(str::to_owned) else {
            self.start_failed(key, attempt);
            return;
        };
        if self.setup.executor.is_none() || !self.cwd_in_project(&launch.cwd, &launch.project) {
            self.start_failed(key, attempt);
            return;
        }
        let mut env = self.setup.env.child_vars();
        env.push(("PWD".into(), launch.cwd.clone().into_os_string()));
        let conn_id = {
            let mut state = self.state();
            let id = state.next_conn;
            state.next_conn += 1;
            id
        };
        let spec = Spec {
            program: launch.path,
            cwd: launch.cwd.clone(),
            env,
        };
        let launched = match Conn::launch(&spec, conn_id) {
            Ok(launched) => launched,
            Err(error) => {
                lectern_logging::warn!("acp process not started"; error = lectern_logging::Value::io(&error));
                self.start_failed(key, attempt);
                return;
            }
        };
        let conn = launched.conn.clone();
        lectern_logging::info!(
            "acp process started";
            program = launch.program.name(),
            argc = 0u64,
            cwd = lectern_logging::Value::path(&launch.cwd),
            pid = conn.pid,
        );
        let mut fx = Effects::default();
        let registered = {
            let mut state = self.state();
            let wanted = !state.stopped
                && state
                    .sessions
                    .get(key)
                    .is_some_and(|conv| conv.run.attempt == attempt);
            if wanted {
                state.by_conn.insert(conn.id, key.clone());
                let now = self.setup.clock.now();
                let conv = state.sessions.get_mut(key).expect("checked above");
                conv.run.conn = Some(conn.clone());
                conv.run.cwd = Some(cwd_str);
                conv.run.phase = Phase::Initializing;
                conv.run.phase_seq += 1;
                let id = conv.run.new_request(Request::Initialize);
                fx.send(
                    key,
                    &conn,
                    Method::Initialize,
                    Some(id as u64),
                    wire::initialize(id),
                );
                fx.timers.push((
                    now + HANDSHAKE,
                    Timer::Phase {
                        key: key.clone(),
                        conn: conn.id,
                        seq: conv.run.phase_seq,
                    },
                ));
            }
            wanted
        };
        if !registered {
            // 启动期间被取消（项目关闭、插件停用）：进程没有主人了。
            conn.close();
            conn.kill();
            let mut child = launched.child;
            let _ = child.wait();
            return;
        }
        let sink: Arc<dyn Sink> = Arc::new(Wire(self.me.clone()));
        let reader = {
            let conn = conn.clone();
            move || conn::read_loop(conn, launched.child, launched.stdout, &*sink)
        };
        let errors = {
            let conn = conn.clone();
            move || conn::drain_stderr(&conn, launched.stderr)
        };
        if !self.spawn_dedicated("acp-read", reader) || !self.spawn_dedicated("acp-stderr", errors)
        {
            let mut state = self.state();
            let attached = state
                .sessions
                .get(key)
                .is_some_and(|conv| conv.run.conn.as_ref().is_some_and(|c| c.id == conn.id));
            if attached {
                self.fail(&mut state, key, LinkError::Spawn, &mut fx);
                self.publish(&mut state, key, true, &mut fx);
            }
            conn.kill();
        }
        self.run(fx);
    }

    /// 启动任务要用的信息：程序的路径、会话的 `cwd` 和项目。已被取消时为 None。
    fn launch_info(&self, key: &SessionKey, attempt: u64) -> Option<Launch> {
        let (program, path) = {
            let state = self.state();
            let conv = state.sessions.get(key)?;
            if state.stopped || conv.run.attempt != attempt {
                return None;
            }
            let program = conv.facts.gate.clone().ok()?;
            match state.programs.get(&program) {
                Some(Lookup::Found(path)) => (program, path.clone()),
                _ => return None,
            }
        };
        let (cwd, project) = self.setup.sources.with(&key.source, |source| {
            let list = source.sessions();
            let summary = list.get(key)?;
            match &summary.place {
                Place::Located(location) => {
                    Some((summary.cwd.to_path_buf(), location.project.clone()))
                }
                _ => None,
            }
        })??;
        Some(Launch {
            program,
            path,
            cwd,
            project,
        })
    }

    /// 工作目录存在、是目录，并且（规范化之后）在会话归位的项目里（同 `terminal` 的 `check_cwd`）。
    fn cwd_in_project(&self, cwd: &Path, project: &ProjectKey) -> bool {
        let Ok(canonical) = std::fs::canonicalize(cwd) else {
            return false;
        };
        if !std::fs::metadata(&canonical).is_ok_and(|m| m.is_dir()) {
            return false;
        }
        self.setup
            .projects
            .with(|projects| projects.locate(&canonical))
            .flatten()
            .is_some_and(|location| &location.project == project)
    }

    /// 启动没有成功（检查没过、进程起不来、被取消）：`Failed(Spawn)`。
    fn start_failed(&self, key: &SessionKey, attempt: u64) {
        let mut fx = Effects::default();
        {
            let mut state = self.state();
            let Some(conv) = state.sessions.get_mut(key) else {
                return;
            };
            if conv.run.attempt != attempt {
                return;
            }
            conv.run.reset_connection();
            conv.run.link = Link::Failed(LinkError::Spawn);
            lectern_logging::warn!("acp link failed"; error = link_error_name(&LinkError::Spawn));
            self.publish(&mut state, key, true, &mut fx);
            Self::drop_if_unused(&mut state, key);
        }
        self.run(fx);
    }

    // ---- 读线程的回调 ----

    /// 读线程收到一行。
    fn on_line(&self, conn: &Arc<Conn>, line: &[u8]) {
        // Parsing, and the diff of a permission request's change preview, take their time and read no
        // engine state: they run before the lock, which `snapshot` on the UI thread also takes.
        let parsed = wire::parse(line);
        let prepared = match &parsed {
            Ok(Incoming::Request {
                method: Method::RequestPermission,
                params,
                ..
            }) => Some(wire::permission(*params).map(|ask| {
                let edits = self.previews.edits(&ask.diffs);
                (ask, edits)
            })),
            _ => None,
        };
        let mut fx = Effects::default();
        {
            let mut state = self.state();
            let Some(key) = state.by_conn.get(&conn.id).cloned() else {
                return;
            };
            let attached = state
                .sessions
                .get(&key)
                .is_some_and(|conv| conv.run.conn.as_ref().is_some_and(|c| c.id == conn.id));
            if state.stopped || !attached {
                return;
            }
            let _scope = lectern_logging::scope(diag::session_ctx(&key.id));
            let bytes = line.len() as u64;
            match parsed {
                Err(_) => {
                    self.fail(&mut state, &key, LinkError::Protocol, &mut fx);
                    self.publish(&mut state, &key, true, &mut fx);
                }
                Ok(Incoming::Response { id, outcome }) => {
                    self.on_response(&mut state, &key, &id, outcome, bytes, &mut fx);
                }
                Ok(Incoming::Notification { method, params }) => {
                    lectern_logging::debug!("acp message received"; method = method.name(), bytes = bytes);
                    if method == Method::SessionUpdate {
                        self.on_update(&mut state, &key, params, &mut fx);
                    } else if let Some(conv) = state.sessions.get_mut(&key) {
                        conv.run.live.skip();
                    }
                }
                Ok(Incoming::Request { id, method, .. }) => {
                    lectern_logging::debug!("acp message received"; method = method.name(), bytes = bytes);
                    if let Some(prepared) = prepared {
                        self.on_permission(&mut state, &key, conn, &id, prepared, &mut fx);
                    } else {
                        // 没有声明的能力（`fs/*`、`terminal/*`）和不认识的方法。
                        fx.send(&key, conn, Method::Other, None, wire::method_not_found(&id));
                    }
                }
            }
        }
        self.run(fx);
    }

    /// 对我们的请求的回复。
    fn on_response(
        &self,
        state: &mut State,
        key: &SessionKey,
        id: &Id,
        outcome: Result<Option<&RawValue>, wire::RpcError>,
        bytes: u64,
        fx: &mut Effects,
    ) {
        let Id::Num(num) = id else {
            return self.protocol_error(state, key, fx);
        };
        let Some((request, sent)) = state
            .sessions
            .get_mut(key)
            .and_then(|conv| conv.run.requests.remove(num))
        else {
            return self.protocol_error(state, key, fx);
        };
        {
            let method = match request {
                Request::Initialize => Method::Initialize,
                Request::Restore(restore) => restore_method(restore),
                Request::Prompt => Method::SessionPrompt,
            };
            let _scope = lectern_logging::scope(diag::rpc_ctx(&key.id, *num as u64));
            lectern_logging::debug!(
                "acp response received";
                method = method.name(),
                bytes = bytes,
                ok = outcome.is_ok(),
                took_us = u64::try_from(sent.elapsed().as_micros()).unwrap_or(u64::MAX),
            );
        }
        let now = self.setup.clock.now();
        match (request, outcome) {
            (Request::Initialize, Ok(raw)) => self.on_initialized(state, key, raw, now, fx),
            (Request::Restore(_), Ok(_)) => {
                let Some(conv) = state.sessions.get_mut(key) else {
                    return;
                };
                conv.run.phase = Phase::Ready;
                match conv.run.queued.take() {
                    Some(text) => Self::dispatch_prompt(key, &mut conv.run, &text, fx),
                    None => {
                        conv.run.link = Link::Idle;
                        Self::arm_idle(key, &mut conv.run, now, fx);
                    }
                }
                self.publish(state, key, true, fx);
            }
            (Request::Prompt, Ok(raw)) => {
                let Ok(stop) = wire::stop_reason(raw) else {
                    return self.protocol_error(state, key, fx);
                };
                let Some(conv) = state.sessions.get_mut(key) else {
                    return;
                };
                // An open request is answered, not forgotten: the adapter may wait for it.
                conv.run.cancel_permissions(key, fx);
                conv.run.live.finish(stop);
                conv.run.link = Link::Idle;
                Self::arm_idle(key, &mut conv.run, now, fx);
                lectern_logging::debug!(
                    "acp turn finished";
                    ended = stop != Stop::EndTurn,
                    skipped = conv.run.live.skipped(),
                    dropped = conv.run.live.dropped(),
                );
                // The turn is in the log by now: read it, so the live items give way to the history.
                fx.reads.push(key.clone());
                self.publish(state, key, true, fx);
            }
            (Request::Prompt, Err(error)) => {
                let Some(conv) = state.sessions.get_mut(key) else {
                    return;
                };
                // An open request is answered, not forgotten: the adapter may wait for it.
                conv.run.cancel_permissions(key, fx);
                conv.run.live.abort();
                // 连接还在：按 `Idle` 计空闲时间，下一次 `Prompt` 直接用它。
                conv.run.link = Link::Failed(if error.code == AUTH_REQUIRED {
                    LinkError::AuthRequired
                } else {
                    LinkError::PromptFailed {
                        message: first_line(&error.message),
                    }
                });
                lectern_logging::warn!("acp link failed"; error = link_error_name(&conv.run.link_error()));
                Self::arm_idle(key, &mut conv.run, now, fx);
                fx.reads.push(key.clone());
                self.publish(state, key, true, fx);
            }
            (Request::Initialize | Request::Restore(_), Err(error)) => {
                let failure = match (request, error.code == AUTH_REQUIRED) {
                    (_, true) => LinkError::AuthRequired,
                    (Request::Restore(_), false) => LinkError::LoadFailed,
                    _ => LinkError::Protocol,
                };
                self.fail(state, key, failure, fx);
                self.publish(state, key, true, fx);
            }
        }
    }

    /// `initialize` 的回复：协议版本必须是 1，然后恢复会话。
    fn on_initialized(
        &self,
        state: &mut State,
        key: &SessionKey,
        raw: Option<&RawValue>,
        now: Instant,
        fx: &mut Effects,
    ) {
        let Ok(result) = wire::initialize_result(raw) else {
            return self.protocol_error(state, key, fx);
        };
        let program = state
            .sessions
            .get(key)
            .and_then(|conv| conv.facts.gate.clone().ok());
        if result.protocol_version != wire::PROTOCOL_VERSION {
            if let Some(program) = program {
                state.unsupported.insert(program);
            }
            self.fail(state, key, LinkError::Unsupported, fx);
            self.publish(state, key, true, fx);
            return;
        }
        if let Some(program) = program {
            state.unsupported.remove(&program);
        }
        let Some(restore) = result.restore() else {
            self.fail(state, key, LinkError::Unsupported, fx);
            self.publish(state, key, true, fx);
            return;
        };
        let Some(conv) = state.sessions.get_mut(key) else {
            return;
        };
        let (Some(conn), Some(cwd)) = (conv.run.conn.clone(), conv.run.cwd.clone()) else {
            return;
        };
        conv.run.phase = Phase::Restoring;
        conv.run.phase_seq += 1;
        let id = conv.run.new_request(Request::Restore(restore));
        fx.send(
            key,
            &conn,
            restore_method(restore),
            Some(id as u64),
            wire::resume(id, restore == Restore::Load, &key.id, &cwd),
        );
        fx.timers.push((
            now + HANDSHAKE,
            Timer::Phase {
                key: key.clone(),
                conn: conn.id,
                seq: conv.run.phase_seq,
            },
        ));
    }

    /// 不合协议的消息（回复里的 `id` 我们没发过、整条消息无法解析）：连接以 `Protocol` 失败。
    fn protocol_error(&self, state: &mut State, key: &SessionKey, fx: &mut Effects) {
        self.fail(state, key, LinkError::Protocol, fx);
        self.publish(state, key, true, fx);
    }

    /// `session/update`。
    fn on_update(
        &self,
        state: &mut State,
        key: &SessionKey,
        params: Option<&RawValue>,
        fx: &mut Effects,
    ) {
        let Ok((session, update)) = wire::session_update(params) else {
            return self.protocol_error(state, key, fx);
        };
        let Some(conv) = state.sessions.get_mut(key) else {
            return;
        };
        let in_turn = matches!(conv.run.link, Link::Working | Link::Stopping);
        let live = &mut conv.run.live;
        // 别的会话的更新、握手期间的回放（`session/load`）和一轮之外的更新：计数后跳过。
        // 上下文占用在回复 prompt 之后也会来，只要连接已经就绪。
        let usable = session == *key.id
            && match update {
                Update::Usage { .. } => conv.run.phase == Phase::Ready,
                _ => in_turn,
            };
        if !usable {
            live.skip();
            return;
        }
        match update {
            Update::AgentText(text) => live.agent_text(&text),
            Update::Tool { info, .. } => live.tool(info),
            Update::Plan(items) => live.set_plan(&items),
            Update::Usage { used, size } => live.set_usage(used, size),
            Update::Skipped => {
                live.skip();
                return;
            }
        }
        self.publish(state, key, false, fx);
    }

    /// `session/request_permission`：不自动回答，放进快照等用户点击。一轮之外的、没有可显示的选项的、
    /// 超过上限的请求回答 `cancelled`。
    fn on_permission(
        &self,
        state: &mut State,
        key: &SessionKey,
        conn: &Arc<Conn>,
        id: &Id,
        prepared: Prepared,
        fx: &mut Effects,
    ) {
        let Ok((ask, edits)) = prepared else {
            return self.protocol_error(state, key, fx);
        };
        let rid = PermissionId(state.next_permission);
        let Some(conv) = state.sessions.get_mut(key) else {
            return;
        };
        let mut by_kind: BTreeMap<OptionKind, OptionId> = BTreeMap::new();
        for (option, kind) in &ask.options {
            by_kind
                .entry(*kind)
                .or_insert_with(|| OptionId(option.as_str().into()));
        }
        if ask.session_id != *key.id
            || conv.run.link != Link::Working
            || by_kind.is_empty()
            || conv.run.permissions.len() >= PERMISSIONS_MAX
        {
            fx.send(
                key,
                conn,
                Method::RequestPermission,
                None,
                wire::cancelled(id),
            );
            return;
        }
        let tool = ask.tool;
        let kind = tool
            .kind
            .or_else(|| conv.run.live.tool_kind(&tool.id))
            .unwrap_or(CallKind::Other);
        let detail = match (kind, &tool.command) {
            (CallKind::Execute, Some(command)) => PermissionDetail::Command {
                text: live::clip_bytes(command, COMMAND_MAX).0.into(),
                workdir: tool.cwd.as_deref().map(|cwd| Arc::from(PathBuf::from(cwd))),
            },
            // The change preview was computed before the lock (`on_line`).
            _ => edits.map_or(PermissionDetail::None, PermissionDetail::Edits),
        };
        let request = PermissionRequest {
            id: rid,
            kind,
            title: live::clip(tool.title.as_deref().unwrap_or_default(), TITLE_CHARS),
            detail,
            options: by_kind
                .into_iter()
                .map(|(kind, id)| PermissionOption { id, kind })
                .collect(),
            asked_at: now_ms(),
        };
        state.next_permission += 1;
        let conv = state.sessions.get_mut(key).expect("checked above");
        conv.run.permissions.push(Ask {
            request,
            rpc: id.clone(),
            tool: tool.id,
        });
        self.publish(state, key, true, fx);
    }

    /// stdout 不会再有消息。
    fn on_ended(&self, conn: &Arc<Conn>, end: End) {
        let mut fx = Effects::default();
        {
            let mut state = self.state();
            let key = state.by_conn.get(&conn.id).cloned();
            if let Some(key) = key
                && !state.stopped
                && matches!(end, End::TooLong | End::Read)
                && state
                    .sessions
                    .get(&key)
                    .is_some_and(|conv| conv.run.conn.as_ref().is_some_and(|c| c.id == conn.id))
            {
                let _scope = lectern_logging::scope(diag::session_ctx(&key.id));
                self.fail(&mut state, &key, LinkError::Protocol, &mut fx);
                self.publish(&mut state, &key, true, &mut fx);
            }
        }
        // 对端关了 stdout 却不退出：2 秒后杀掉整个进程组，读线程才等得到它。
        fx.timers.push((
            self.setup.clock.now() + KILL_AFTER,
            Timer::Kill(conn.clone()),
        ));
        self.run(fx);
    }

    /// 进程已被回收。
    fn on_exited(&self, conn: &Arc<Conn>, exit: Exit) {
        let mut fx = Effects::default();
        {
            let mut state = self.state();
            let key = state.by_conn.remove(&conn.id);
            state.closing.remove(&conn.id);
            if let Some(key) = &key {
                let _scope = lectern_logging::scope(diag::session_ctx(&key.id));
                lectern_logging::info!(
                    "acp process exited";
                    pid = conn.pid,
                    code = exit.code.map_or(-1, i64::from),
                    signaled = exit.signaled,
                    stderr_bytes = conn.stderr_bytes(),
                    runtime_ms = conn.runtime_ms(),
                );
                let attached = !state.stopped
                    && state.sessions.get(key).is_some_and(|conv| {
                        conv.run.conn.as_ref().is_some_and(|c| c.id == conn.id)
                    });
                if attached {
                    // 我们没有要它退出：连接期间进程死了。
                    self.fail(
                        &mut state,
                        key,
                        LinkError::Exited { code: exit.code },
                        &mut fx,
                    );
                    self.publish(&mut state, key, true, &mut fx);
                }
                Self::drop_if_unused(&mut state, key);
            } else {
                lectern_logging::info!(
                    "acp process exited";
                    pid = conn.pid,
                    code = exit.code.map_or(-1, i64::from),
                    signaled = exit.signaled,
                    stderr_bytes = conn.stderr_bytes(),
                    runtime_ms = conn.runtime_ms(),
                );
            }
        }
        self.run(fx);
    }

    /// executor 上：关闭 stdin、SIGTERM，2 秒后 SIGKILL。
    fn close_conn(&self, conn: &Arc<Conn>, why: Closed) {
        let key = self.state().by_conn.get(&conn.id).cloned();
        let _scope = key.map(|key| lectern_logging::scope(diag::session_ctx(&key.id)));
        lectern_logging::info!("acp connection closing"; reason = why.name(), pid = conn.pid);
        conn.close();
        self.arm(
            self.setup.clock.now() + KILL_AFTER,
            Timer::Kill(conn.clone()),
        );
    }

    /// 插件停用（也是 Lectern 退出时的路径）：所有连接关闭，待确认的请求回答 `cancelled`，
    /// 等进程退出至多 500 ms，还在的 SIGKILL。丢掉全部状态（关注的回调随后什么也不做）。
    pub(crate) fn stop(&self) {
        let conns: Vec<Arc<Conn>> = {
            let mut state = self.state();
            state.stopped = true;
            let mut fx = Effects::default();
            let mut conns = Vec::new();
            for (key, conv) in &mut state.sessions {
                conv.run.cancel_permissions(key, &mut fx);
                if let Some(conn) = conv.run.conn.take() {
                    conns.push(conn);
                }
            }
            // Connections that were closed before: their close task may still be queued, and the
            // executor drops queued tasks of a revoked plugin, so they are closed here as well.
            conns.extend(state.closing.drain().map(|(_, conn)| conn));
            state.sessions.clear();
            state.waiting = Arc::from([]);
            conns
        };
        for conn in &conns {
            conn.close();
        }
        let deadline = Instant::now() + STOP_WAIT;
        for conn in &conns {
            conn.wait_exited(deadline.saturating_duration_since(Instant::now()));
        }
        for conn in conns.iter().filter(|conn| !conn.has_exited()) {
            conn.kill();
        }
    }
}

/// 启动一个连接要用的东西。
struct Launch {
    program: lectern_services::conversation::AcpProgram,
    path: PathBuf,
    cwd: PathBuf,
    project: ProjectKey,
}

/// agent 写的错误文字：第一行，至多 1 KiB。
fn first_line(message: &str) -> Arc<str> {
    live::clip_bytes(message.lines().next().unwrap_or_default(), MESSAGE_MAX)
        .0
        .into()
}

impl Run {
    fn link_error(&self) -> LinkError {
        match &self.link {
            Link::Failed(error) => error.clone(),
            _ => LinkError::Protocol,
        }
    }
}
