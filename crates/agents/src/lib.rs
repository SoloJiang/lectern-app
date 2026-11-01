//! Claude Code 与 Codex 会话日志的发现与解析（docs/design/agents.md）。
//!
//! 两个适配器的库层：[`claude::ClaudeCode`] 和 [`codex::Codex`] 按会话文件交出增量读取的记录、轮次索引、
//! 正文、会话摘要和恢复命令；Codex 的 `.jsonl.zst` 在解压副本上读。[`plugins`] 给出两个插件，它们在适配器
//! 外面实现 `lectern_services::session::SessionSource`：发现与监听会话目录、按 cwd 归位、缓存轮次索引与
//! 压缩会话的摘要。
//!
//! 解析不先转成 JSON 的通用值树：每行反序列化到只声明用到字段的结构，字符串借用原行（ADR 0026）。

mod acp;
pub mod claude;
mod codec;
pub mod codex;
mod diag;
mod filter;
mod index;
mod lines;
mod path;
mod plugin;
mod read;
mod roots;
mod source;
mod stats;
mod tail;
mod time;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

#[cfg(test)]
mod redact;

#[cfg(any(test, feature = "test-support"))]
pub use acp::Probe;
pub use acp::{
    ConversationsDeps, ConversationsPlugin, PLUGIN_ID as CONVERSATIONS_PLUGIN_ID,
    conversations_plugin,
};
pub use filter::SessionFilter;
pub use plugin::{AgentsDeps, SessionPlugin, plugins};
pub use read::{Activity, Parsed};
pub use roots::AgentRoots;
pub use stats::ParseStats;
