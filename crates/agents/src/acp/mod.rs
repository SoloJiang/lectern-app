//! ACP 客户端（docs/design/agents.md「ACP 客户端」，ADR 0050、ADR 0060）：插件 "conversations" 提供
//! `lectern_services::conversation::Conversations`。
//!
//! 会话历史窗口、`watch` / `snapshot` / `LoadEarlier`、`can_prompt` 的检查和程序查找（第 6 步），
//! 进程的启动与关闭、手写的 JSON-RPC 层、实时项和权限请求（第 7 步），实时项换成历史项、权限请求的
//! 改动预览（`preview`）、`waiting()` 与 `WaitingChanged`（第 8 步）。

mod conn;
mod engine;
mod history;
mod link;
mod live;
mod plugin;
mod preview;
mod program;
mod wire;

#[cfg(test)]
mod tests;

#[cfg(any(test, feature = "test-support"))]
pub use plugin::Probe;
pub use plugin::{ConversationsDeps, ConversationsPlugin, PLUGIN_ID, conversations_plugin};
