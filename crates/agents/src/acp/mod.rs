//! ACP 客户端（docs/design/agents.md「ACP 客户端」，ADR 0050、ADR 0060）：插件 "conversations" 提供
//! `lectern_services::conversation::Conversations`。
//!
//! 现在有会话历史窗口、`watch` / `snapshot` / `LoadEarlier`、`can_prompt` 的检查和程序查找；
//! 进程与 JSON-RPC 在设计文档的第 7 步。

mod engine;
mod history;
mod plugin;
mod program;

#[cfg(test)]
mod tests;

pub use plugin::{ConversationsDeps, ConversationsPlugin, PLUGIN_ID, conversations_plugin};
