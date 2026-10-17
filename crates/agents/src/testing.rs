//! 测试与 bench 用：不经过插件，直接解析一个文件（docs/design/agents.md「本 crate 的公开项」）。

use std::path::Path;

use lectern_services::session::{Agent, Positioned, Record, SessionError, SourceId, TurnIndex};

use crate::claude::{self, ClaudeCode, Owner, SessionFile};
use crate::codex::{self, Codex};

/// 解析整个文件，交出全部记录和轮次索引。
///
/// Claude Code 文件按路径认出主会话或子 agent（[`ClaudeCode::session_file`]），认不出时按主会话、
/// 会话 ID 取文件名（去掉扩展名）。Codex 文件按文件名认出线程 ID（[`Codex::session_file`]），认不出时
/// 会话 ID 取文件名；`.jsonl.zst` 从头流式解码一遍，不建解压副本；没有来源会话可找（`legacy` 分叉的
/// 继承前缀按边界未定处理）。
pub fn parse_file(
    agent: Agent,
    path: &Path,
) -> Result<(Vec<Positioned<Record>>, TurnIndex), SessionError> {
    match agent {
        Agent::ClaudeCode => {
            let adapter = ClaudeCode::new(SourceId(claude::SOURCE_ID.into()));
            let file = adapter.session_file(path).unwrap_or_else(|| {
                let id: std::sync::Arc<str> = path
                    .file_stem()
                    .map(|s| s.to_string_lossy())
                    .unwrap_or_default()
                    .into();
                SessionFile {
                    key: lectern_services::session::SessionKey {
                        source: adapter.source().clone(),
                        id: id.clone(),
                    },
                    path: path.into(),
                    owner: Owner::Main { session_id: id },
                }
            });
            let (parsed, index) = adapter.parse_all(&file)?;
            Ok((parsed.batch.records, index))
        }
        Agent::Codex => {
            let adapter = Codex::without_decoded_copies(SourceId(codex::SOURCE_ID.into()));
            let file = adapter.session_file(path).unwrap_or_else(|| {
                let name = path
                    .file_name()
                    .map(|s| s.to_string_lossy())
                    .unwrap_or_default();
                let compressed = name.ends_with(".zst");
                let id = name
                    .trim_end_matches(".zst")
                    .trim_end_matches(".jsonl")
                    .to_owned();
                codex::SessionFile {
                    key: lectern_services::session::SessionKey {
                        source: adapter.source().clone(),
                        id: id.into(),
                    },
                    path: path.into(),
                    compressed,
                    archived: false,
                }
            });
            let (parsed, index) = adapter.parse_all(&file)?;
            Ok((parsed.batch.records, index))
        }
        Agent::Other(_) => Err(SessionError::Unrecognized),
    }
}

/// 合成的 Claude Code 2.1.302 会话（`tests/fixtures/claude-code/2.1.302/`）：主会话、两个子 agent、
/// 另一个会话，依次拼接。
const CLAUDE_SAMPLE: [&[u8]; 4] = [
    include_bytes!(
        "../tests/fixtures/claude-code/2.1.302/projects/-Users-dev-fixture-repo/11111111-1111-4111-8111-111111111111.jsonl"
    ),
    include_bytes!(
        "../tests/fixtures/claude-code/2.1.302/projects/-Users-dev-fixture-repo/11111111-1111-4111-8111-111111111111/subagents/agent-a0fa0001.jsonl"
    ),
    include_bytes!(
        "../tests/fixtures/claude-code/2.1.302/projects/-Users-dev-fixture-repo/11111111-1111-4111-8111-111111111111/subagents/workflows/wf-fixture/agent-a0fa0002.jsonl"
    ),
    include_bytes!(
        "../tests/fixtures/claude-code/2.1.302/projects/-Users-dev-fixture-repo/22222222-2222-4222-8222-222222222222.jsonl"
    ),
];

/// 合成的 Codex 0.140.0 rollout（`tests/fixtures/codex/`）里的两个主会话（不含需要来源会话的分叉）。
const CODEX_SAMPLE: [&[u8]; 2] = [
    include_bytes!(
        "../tests/fixtures/codex/0.140.0-legacy/sessions/2026/10/02/rollout-2026-10-02T09-00-00-aaaaaaaa-0000-4000-8000-000000000001.jsonl"
    ),
    include_bytes!(
        "../tests/fixtures/codex/0.140.0-paginated/sessions/2026/10/05/rollout-2026-10-05T09-00-00-bbbbbbbb-0000-4000-8000-000000000001.jsonl"
    ),
];

/// 把 `sample` 依次拼接、整体重复，直到不少于 `len` 字节（总是整份样本的倍数，每一行都完整）。
fn repeat(sample: &[&[u8]], len: usize) -> Vec<u8> {
    let one: usize = sample.iter().map(|s| s.len()).sum();
    let mut data = Vec::with_capacity(len + one);
    while data.len() < len {
        for part in sample {
            data.extend_from_slice(part);
        }
    }
    data
}

/// 一个 Claude Code 会话文件的内容，不少于 `len` 字节：fixtures 的全部会话文件重复拼接。
/// 供 bench 的 `agents/parse_throughput`、`agents/append_4k`（docs/design/bench.md「迁移」）。
pub fn synthetic_session(len: usize) -> Vec<u8> {
    repeat(&CLAUDE_SAMPLE, len)
}

/// 约 4 KB 的完整行：[`synthetic_session`] 开头到第一个不早于 4096 字节的行尾，追加到会话末尾后
/// 仍是合法的会话。
pub fn append_chunk() -> &'static [u8] {
    let first = CLAUDE_SAMPLE[0];
    let end = first
        .iter()
        .enumerate()
        .find(|(i, b)| **b == b'\n' && *i + 1 >= 4096)
        .map_or(first.len(), |(i, _)| i + 1);
    &first[..end]
}

/// 一个 Codex rollout 的内容，不少于 `len` 字节：fixtures 的两个主会话重复拼接。供 bench 的
/// `agents/zst_throughput`、`agents/zst_load_decoded`。
pub fn synthetic_rollout(len: usize) -> Vec<u8> {
    repeat(&CODEX_SAMPLE, len)
}

/// 用 `ruzstd` 的最快档压缩成一个 zstd 帧，即 `.jsonl.zst` 文件的内容。
pub fn compress_zst(data: &[u8]) -> Vec<u8> {
    ruzstd::encoding::compress_to_vec(data, ruzstd::encoding::CompressionLevel::Fastest)
}
