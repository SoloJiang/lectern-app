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
