//! 运行日志里本 crate 的取值（docs/design/agents.md「日志与诊断」、docs/design/logging.md）：会话标识、
//! 跳过一行的原因和解析一遍之后的那条记录。只记位置、长度、计数和种类，不记行的内容、记录类型名和工具名
//! （都来自会话文件），错误只记种类。

use std::path::Path;

use lectern_logging::{Ctx, Field, Value};
use lectern_services::session::Agent;

use crate::stats::ParseStats;

/// 会话 ID 的 128 位标识，用于 `session` 作用域：ID 是 UUID 时（Claude Code 与 Codex 的会话 ID）取它的
/// 16 个字节，否则（Claude Code 子 agent 的 `<父会话>/agent-<id>`）取哈希。算法与 `usage`、`review` 相同，
/// 三个 crate 对同一会话的记录可以对照。不分配内存。
pub(crate) fn session_tag(id: &str) -> [u8; 16] {
    let mut out = [0u8; 16];
    let mut digits = 0usize;
    let mut uuid = true;
    for b in id.bytes() {
        if b == b'-' {
            continue;
        }
        let Some(d) = (b as char).to_digit(16) else {
            uuid = false;
            break;
        };
        if digits == 32 {
            uuid = false;
            break;
        }
        // `to_digit(16)` 小于 16。
        let d = d as u8;
        out[digits / 2] |= if digits.is_multiple_of(2) { d << 4 } else { d };
        digits += 1;
    }
    if uuid && digits == 32 {
        return out;
    }
    use std::hash::{Hash, Hasher};
    let mut out = [0u8; 16];
    for (i, half) in out.chunks_mut(8).enumerate() {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (i, id).hash(&mut h);
        half.copy_from_slice(&h.finish().to_le_bytes());
    }
    out
}

/// 一个会话的作用域：只设 `session`，其余沿用外层（executor 任务的 `plugin`、`act`、`task`）。
pub(crate) fn session_ctx(id: &str) -> Ctx {
    Ctx {
        session: Some(session_tag(id)),
        ..Ctx::default()
    }
}

/// 一个 JSON-RPC 请求的作用域：会话和请求序号（`req`）。
pub(crate) fn rpc_ctx(id: &str, req: u64) -> Ctx {
    Ctx {
        session: Some(session_tag(id)),
        req: Some(req),
        ..Ctx::default()
    }
}

/// 一行被跳过的原因（`error` 字段）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Skip {
    /// 不是合法的 JSON。
    Syntax,
    /// 合法的 JSON，但形状不对（字段类型不符）。
    Data,
    /// JSON 在行尾之前结束。
    Eof,
    /// 缺少必需的字段（`type`、时间戳、消息、`payload` 等）。
    Missing,
    /// 在会话头之前（还没有 cwd 和会话 ID）。
    NoHeader,
    /// 超过 16 MB。
    Oversized,
    /// 不认识的记录类型。
    UnknownType,
}

impl Skip {
    pub(crate) fn of_json(e: &serde_json::Error) -> Skip {
        match e.classify() {
            serde_json::error::Category::Syntax => Skip::Syntax,
            serde_json::error::Category::Data => Skip::Data,
            serde_json::error::Category::Eof => Skip::Eof,
            // 从切片解析，不会有 IO 错误。
            serde_json::error::Category::Io => Skip::Syntax,
        }
    }

    fn text(self) -> &'static str {
        match self {
            Skip::Syntax => "syntax",
            Skip::Data => "data",
            Skip::Eof => "eof",
            Skip::Missing => "missing_field",
            Skip::NoHeader => "before_header",
            Skip::Oversized => "oversized",
            Skip::UnknownType => "unknown_type",
        }
    }
}

impl Field for Skip {
    fn value(&self) -> Value<'_> {
        Value::text(self.text())
    }
}

/// 一遍解析里第一条被跳过的行：字节偏移、长度（不含换行）和原因。末尾向前扫描时遇到的超长记录没有偏移。
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct FirstSkip(Option<(Option<u64>, Option<u64>, Skip)>);

impl FirstSkip {
    /// 记下一行；已经有一行时不变。
    pub(crate) fn note(&mut self, pos: Option<u64>, len: Option<u64>, kind: Skip) {
        if self.0.is_none() {
            self.0 = Some((pos, len, kind));
        }
    }

    /// `other` 的第一行接在这一遍之后。
    pub(crate) fn or(self, other: FirstSkip) -> FirstSkip {
        FirstSkip(self.0.or(other.0))
    }
}

/// 解析的哪一遍（`stage` 字段）。
#[derive(Clone, Copy)]
pub(crate) enum Stage {
    /// `read`、`turns` 与整份解析：从游标（`from`）读到末尾或 `max_bytes`。
    Read,
    /// 发现阶段的摘要：开头 256 KB 与末尾。
    Summary,
    /// 压缩会话补齐末尾字段（整份流式解码）。
    Complete,
}

impl Field for Stage {
    fn value(&self) -> Value<'_> {
        Value::text(match self {
            Stage::Read => "read",
            Stage::Summary => "summary",
            Stage::Complete => "complete",
        })
    }
}

/// 一遍解析结束：有行被跳过时记一条 warn（每遍一条，不是每行一条），只有表外工具名时记一条 debug。
/// 在会话的作用域里写出。没有被跳过的行、也没有表外工具名时什么也不做。
pub(crate) fn parsed(
    stage: Stage,
    id: &str,
    path: &Path,
    from: u64,
    first: FirstSkip,
    stats: &ParseStats,
) {
    let unknown_tools = stats
        .unknown_tools
        .iter()
        .map(|(_, n)| u64::from(*n))
        .sum::<u64>();
    if !stats.skipped() && unknown_tools == 0 {
        return;
    }
    let _scope = lectern_logging::scope(session_ctx(id));
    if stats.skipped() {
        let (pos, line_len, error) = match first.0 {
            Some((pos, len, kind)) => (pos, len, Some(kind)),
            None => (None, None, None),
        };
        let unknown_types = stats
            .unknown_types
            .iter()
            .map(|(_, n)| u64::from(*n))
            .sum::<u64>();
        lectern_logging::warn!(
            "session lines skipped";
            stage = stage,
            path = Value::path(path),
            from = from,
            pos = pos,
            line_len = line_len,
            error = error,
            bad_lines = stats.bad_lines,
            oversized_lines = stats.oversized_lines,
            unknown_types = unknown_types,
            unknown_tools = unknown_tools,
        );
    } else {
        lectern_logging::debug!(
            "session tools unclassified";
            stage = stage,
            path = Value::path(path),
            from = from,
            tools = stats.unknown_tools.len(),
            calls = unknown_tools,
        );
    }
}

/// `Agent` 的 `agent` 字段（`Other` 的名字是运行时字符串，不写出）。
pub(crate) fn agent(agent: &Agent) -> Value<'static> {
    Value::text(match agent {
        Agent::ClaudeCode => "claude_code",
        Agent::Codex => "codex",
        Agent::Other(_) => "other",
    })
}

/// `SessionError` 的种类；不记它附带的文字（`Io` 的原因是格式化出的错误消息）。
pub(crate) fn session_error(e: &lectern_services::session::SessionError) -> Value<'static> {
    use lectern_services::session::SessionError;
    Value::text(match e {
        SessionError::NotFound => "not_found",
        SessionError::Io(_) => "io",
        SessionError::Unrecognized => "unrecognized",
        SessionError::TooLarge(_) => "too_large",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 与 `usage`、`review` 的写法相同（先去掉 `-` 再按 32 个十六进制数字判断）。
    fn reference(id: &str) -> [u8; 16] {
        let hex: Vec<u8> = id.bytes().filter(|b| *b != b'-').collect();
        if hex.len() == 32 && hex.iter().all(u8::is_ascii_hexdigit) {
            let mut out = [0u8; 16];
            for (i, pair) in hex.chunks(2).enumerate() {
                let digit = |b: u8| (b as char).to_digit(16).unwrap_or(0) as u8;
                out[i] = digit(pair[0]) << 4 | digit(pair[1]);
            }
            return out;
        }
        use std::hash::{Hash, Hasher};
        let mut out = [0u8; 16];
        for (i, half) in out.chunks_mut(8).enumerate() {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (i, id).hash(&mut h);
            half.copy_from_slice(&h.finish().to_le_bytes());
        }
        out
    }

    #[test]
    fn logs_session_tag_matches_usage_and_review() {
        for id in [
            "5e55e55e-1111-4111-8111-0000000000a1",
            "5E55E55E-1111-4111-8111-0000000000A1",
            "5e55e55e11114111811100000000 00a1",
            "5e55e55e-1111-4111-8111-0000000000a1/agent-a0fa0001",
            "5e55e55e-1111-4111-8111-0000000000a1f",
            "5e55e55e-1111-4111-8111-0000000000a",
            "----",
            "",
            "agent-é",
        ] {
            assert_eq!(session_tag(id), reference(id), "{id:?}");
        }
        assert_eq!(
            session_tag("5e55e55e-1111-4111-8111-0000000000a1"),
            [
                0x5e, 0x55, 0xe5, 0x5e, 0x11, 0x11, 0x41, 0x11, 0x81, 0x11, 0, 0, 0, 0, 0, 0xa1
            ]
        );
    }
}
