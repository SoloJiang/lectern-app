//! 测试与 bench 用：不经过插件，直接解析一个文件（docs/design/agents.md「本 crate 的公开项」）。

use std::path::Path;

use lectern_services::session::{Agent, Positioned, Record, SessionError, SourceId, TurnIndex};

use crate::claude::{ClaudeCode, Owner, SOURCE_ID, SessionFile};

/// 解析整个文件，交出全部记录和轮次索引。
///
/// Claude Code 文件按路径认出主会话或子 agent（[`ClaudeCode::session_file`]），认不出时按主会话、
/// 会话 ID 取文件名（去掉扩展名）。Codex 的适配器在第 3 步，现在返回 `Unrecognized`。
pub fn parse_file(
    agent: Agent,
    path: &Path,
) -> Result<(Vec<Positioned<Record>>, TurnIndex), SessionError> {
    match agent {
        Agent::ClaudeCode => {
            let adapter = ClaudeCode::new(SourceId(SOURCE_ID.into()));
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
        Agent::Codex | Agent::Other(_) => Err(SessionError::Unrecognized),
    }
}
