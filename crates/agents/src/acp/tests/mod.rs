//! 历史窗口、`can_prompt` 的检查、程序查找、关注与快照、插件和日志的测试
//! （docs/design/agents.md「ACP 客户端」的「测试」）。假的 `SessionSource` 与 `Projects`，
//! 不读真实的会话目录，不运行登录 shell，不启动进程。

mod gate;
mod history;
mod logs;
mod lookup;
mod plugin;
pub(crate) mod support;
mod watch;
