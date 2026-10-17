//! 两个适配器在 `SessionSource` 实现下的共同形状。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lectern_services::session::{
    Agent, Content, ContentRef, Cursor, ResumeCommand, SessionError, SessionKey, SessionSummary,
    SourceId, TurnIndex,
};

use crate::claude::{self, ClaudeCode};
use crate::codex::{self, Codex};
use crate::index::IndexBuilder;
use crate::read::{Activity, Parsed, Summarized};

/// 发现、监听与 `SessionSource` 需要适配器做的事。`File` 是适配器自己的会话文件描述。
pub(crate) trait Adapter: Send + Sync + 'static {
    type File: Clone + Send + Sync + 'static;

    fn agent(&self) -> Agent;
    fn source(&self) -> &SourceId;
    /// 按路径认出会话文件；其余为 None。
    fn session_file(&self, path: &Path) -> Option<Self::File>;
    fn key(file: &Self::File) -> &SessionKey;
    fn path(file: &Self::File) -> &Arc<Path>;
    fn compressed(file: &Self::File) -> bool;
    /// 同一 `SessionKey` 的几个候选路径里选哪个：小的优先（「不变量」）。
    fn rank(file: &Self::File) -> u8;
    /// 这个路径不是会话文件，但它变化时要重新生成某个会话文件的摘要（Claude Code 子 agent 的 `meta.json`）。
    fn related(&self, path: &Path) -> Option<PathBuf>;

    fn summarize(&self, file: &Self::File) -> Result<Summarized, SessionError>;
    /// 读过这个会话之后的摘要：继承前缀的来源会话能找到时按需扫描它（Codex）；其余与 `summarize` 相同。
    fn summarize_with_sources(&self, file: &Self::File) -> Result<Summarized, SessionError> {
        self.summarize(file)
    }
    /// 补齐 `tail_pending` 的字段（整份流式解码一次）。
    fn complete(&self, file: &Self::File) -> Result<Summarized, SessionError>;
    /// 由路径决定的摘要字段（`file`，Codex 的 `archived`）按 `file` 重新填写：缓存里的摘要来自同一个
    /// 文件身份，但文件可能被移动过（`sessions/` 移到 `archived_sessions/`）。
    fn path_fields(file: &Self::File, summary: &mut SessionSummary) {
        summary.file = Self::path(file).clone();
    }
    fn read(
        &self,
        file: &Self::File,
        from: &Cursor,
        max_bytes: u64,
        index: &mut IndexBuilder,
    ) -> Result<Parsed, SessionError>;
    fn turns_after(
        &self,
        file: &Self::File,
        prev: Option<&TurnIndex>,
    ) -> Result<TurnIndex, SessionError>;
    fn load(&self, file: &Self::File, at: &ContentRef) -> Result<Content, SessionError>;
    fn resume(&self, summary: &SessionSummary) -> Option<ResumeCommand>;
    fn activity(&self, cursor: &Cursor) -> Option<Activity>;
    /// 会话的全部候选路径都消失之后（Codex 删掉解压副本）。
    fn removed(&self, key: &SessionKey);
}

impl Adapter for ClaudeCode {
    type File = claude::SessionFile;

    fn agent(&self) -> Agent {
        Agent::ClaudeCode
    }

    fn source(&self) -> &SourceId {
        ClaudeCode::source(self)
    }

    fn session_file(&self, path: &Path) -> Option<Self::File> {
        ClaudeCode::session_file(self, path)
    }

    fn key(file: &Self::File) -> &SessionKey {
        &file.key
    }

    fn path(file: &Self::File) -> &Arc<Path> {
        &file.path
    }

    fn compressed(_: &Self::File) -> bool {
        false
    }

    fn rank(_: &Self::File) -> u8 {
        0
    }

    fn related(&self, path: &Path) -> Option<PathBuf> {
        let name = path.file_name()?.to_str()?;
        let agent = name.strip_suffix(".meta.json")?;
        agent
            .starts_with("agent-")
            .then(|| path.with_file_name(format!("{agent}.jsonl")))
    }

    fn summarize(&self, file: &Self::File) -> Result<Summarized, SessionError> {
        let (summary, stats) = ClaudeCode::summarize(self, file)?;
        Ok(Summarized {
            summary,
            stats,
            lineage: None,
        })
    }

    fn complete(&self, file: &Self::File) -> Result<Summarized, SessionError> {
        Adapter::summarize(self, file)
    }

    fn read(
        &self,
        file: &Self::File,
        from: &Cursor,
        max_bytes: u64,
        index: &mut IndexBuilder,
    ) -> Result<Parsed, SessionError> {
        self.read_indexed(file, from, max_bytes, index)
    }

    fn turns_after(
        &self,
        file: &Self::File,
        prev: Option<&TurnIndex>,
    ) -> Result<TurnIndex, SessionError> {
        ClaudeCode::turns_after(self, file, prev)
    }

    fn load(&self, file: &Self::File, at: &ContentRef) -> Result<Content, SessionError> {
        ClaudeCode::load(self, file, at)
    }

    fn resume(&self, summary: &SessionSummary) -> Option<ResumeCommand> {
        ClaudeCode::resume(self, summary)
    }

    fn activity(&self, cursor: &Cursor) -> Option<Activity> {
        ClaudeCode::activity(self, cursor)
    }

    fn removed(&self, _: &SessionKey) {}
}

impl Adapter for Codex {
    type File = codex::SessionFile;

    fn agent(&self) -> Agent {
        Agent::Codex
    }

    fn source(&self) -> &SourceId {
        Codex::source(self)
    }

    fn session_file(&self, path: &Path) -> Option<Self::File> {
        Codex::session_file(self, path)
    }

    fn key(file: &Self::File) -> &SessionKey {
        &file.key
    }

    fn path(file: &Self::File) -> &Arc<Path> {
        &file.path
    }

    fn compressed(file: &Self::File) -> bool {
        file.compressed
    }

    /// `sessions/` 优先于 `archived_sessions/`，`.jsonl` 优先于 `.jsonl.zst`。
    fn rank(file: &Self::File) -> u8 {
        u8::from(file.archived) * 2 + u8::from(file.compressed)
    }

    fn related(&self, _: &Path) -> Option<PathBuf> {
        None
    }

    fn summarize(&self, file: &Self::File) -> Result<Summarized, SessionError> {
        self.summarize_lineage(file)
    }

    fn summarize_with_sources(&self, file: &Self::File) -> Result<Summarized, SessionError> {
        Codex::summarize_with_sources(self, file)
    }

    fn complete(&self, file: &Self::File) -> Result<Summarized, SessionError> {
        self.complete_lineage(file)
    }

    fn path_fields(file: &Self::File, summary: &mut SessionSummary) {
        summary.file = file.path.clone();
        summary.archived = file.archived;
    }

    fn read(
        &self,
        file: &Self::File,
        from: &Cursor,
        max_bytes: u64,
        index: &mut IndexBuilder,
    ) -> Result<Parsed, SessionError> {
        self.read_indexed(file, from, max_bytes, index)
    }

    fn turns_after(
        &self,
        file: &Self::File,
        prev: Option<&TurnIndex>,
    ) -> Result<TurnIndex, SessionError> {
        Codex::turns_after(self, file, prev)
    }

    fn load(&self, file: &Self::File, at: &ContentRef) -> Result<Content, SessionError> {
        Codex::load(self, file, at)
    }

    fn resume(&self, summary: &SessionSummary) -> Option<ResumeCommand> {
        Codex::resume(self, summary)
    }

    fn activity(&self, cursor: &Cursor) -> Option<Activity> {
        Codex::activity(self, cursor)
    }

    fn removed(&self, key: &SessionKey) {
        self.remove_decoded(key);
    }
}
