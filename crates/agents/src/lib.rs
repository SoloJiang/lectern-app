//! Claude Code 与 Codex 会话日志的发现与解析（docs/design/agents.md）。
//!
//! 第 3 步（本版本）有两个适配器的库层：[`claude::ClaudeCode`] 和 [`codex::Codex`] 按会话文件交出
//! 增量读取的记录、轮次索引、正文、会话摘要和恢复命令；Codex 的 `.jsonl.zst` 在解压副本上读。
//! 发现、监听、归位和插件（第 4 步）在它们外面实现 `lectern_services::session::SessionSource`。
//!
//! 解析不先转成 JSON 的通用值树：每行反序列化到只声明用到字段的结构，字符串借用原行（ADR 0026）。

pub mod claude;
mod codec;
pub mod codex;
mod index;
mod lines;
mod path;
mod read;
mod stats;
mod tail;
mod time;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

#[cfg(test)]
mod redact;

pub use read::{Activity, Parsed};
pub use stats::ParseStats;
