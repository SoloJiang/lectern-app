//! Claude Code 适配器（docs/design/agents.md 验收第 2 步）。
//!
//! 库层：给定一个会话文件，交出增量读取的记录、轮次索引、正文、会话摘要和恢复命令。
//! 第 4 步的发现、监听与插件在它外面包一层，按 `SessionKey` 找到文件再调用这里。

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lectern_services::session::{
    Batch, Content, ContentRef, Cursor, Format, ResumeCommand, SessionError, SessionKey,
    SessionSummary, SourceId, TurnIndex,
};

use crate::codec;
use crate::index::IndexBuilder;
use crate::lines::{HEAD, Lines, Next, PREFIX};
use crate::stats::ParseStats;

mod format;
mod load;
mod parse;
mod summary;
mod tools;

#[cfg(test)]
pub(crate) mod tests;

use parse::{Parse, State};

pub(crate) use crate::read::io_error;
pub use crate::read::{Activity, Parsed};

/// 内置 Claude Code 来源的 ID（插件 ID）。
pub const SOURCE_ID: &str = "session-claude-code";

/// 适配器版本：解析规则或游标状态的布局变化时加一，旧游标从头读、旧索引不再命中。
pub const ADAPTER_VERSION: u32 = 1;

/// 会话文件归属于谁（「外部格式」里的「继承前缀」）。
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Owner {
    /// `<sessionId>.jsonl`。
    Main { session_id: Arc<str> },
    /// `<父会话 sessionId>/subagents/…/agent-<agentId>.jsonl`。
    Subagent {
        parent_session_id: Arc<str>,
        agent_id: Arc<str>,
    },
}

impl Owner {
    /// 这条记录是不是继承自别的会话：`sessionId` 与归属的会话不同，或者在子 agent 文件里
    /// `agentId` 与文件的不同。
    pub(crate) fn inherits(&self, session_id: Option<&str>, agent_id: Option<&str>) -> bool {
        match self {
            Owner::Main { session_id: own } => session_id != Some(&**own),
            Owner::Subagent {
                parent_session_id,
                agent_id: own,
            } => session_id != Some(&**parent_session_id) || agent_id != Some(&**own),
        }
    }
}

/// 一个会话文件：路径、全局键和归属。
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct SessionFile {
    pub key: SessionKey,
    pub path: Arc<Path>,
    pub owner: Owner,
}

impl SessionFile {
    /// 子 agent 的 `agent-<agentId>.meta.json`。
    fn meta_path(&self) -> Option<PathBuf> {
        match &self.owner {
            Owner::Main { .. } => None,
            Owner::Subagent { agent_id, .. } => Some(
                self.path
                    .with_file_name(format!("agent-{agent_id}.meta.json")),
            ),
        }
    }
}

/// Claude Code 适配器。只持有来源 ID，没有可变状态，可以在多个线程上同时使用。
#[derive(Clone, Debug)]
pub struct ClaudeCode {
    source: SourceId,
}

/// 从哪里开始读。
enum Start {
    Begin,
    Resume {
        offset: u64,
        prefix: Vec<u8>,
        state: State,
    },
    Reset,
}

impl ClaudeCode {
    pub fn new(source: SourceId) -> Self {
        ClaudeCode { source }
    }

    pub fn source(&self) -> &SourceId {
        &self.source
    }

    /// 按路径认出会话文件：`<sessionId>.jsonl`，或某个 `subagents/` 目录下（可以再嵌套）的
    /// `agent-<agentId>.jsonl`，父会话是 `subagents/` 的上一级目录名。其余返回 None。
    pub fn session_file(&self, path: &Path) -> Option<SessionFile> {
        let name = path.file_name()?.to_str()?;
        let stem = name.strip_suffix(".jsonl")?;
        if stem.is_empty() {
            return None;
        }
        let subagent = stem.strip_prefix("agent-").and_then(|agent_id| {
            let parent = path
                .ancestors()
                .skip(1)
                .find(|a| a.file_name().is_some_and(|n| n == "subagents"))?
                .parent()?
                .file_name()?
                .to_str()?;
            (!agent_id.is_empty()).then(|| (Arc::<str>::from(parent), Arc::<str>::from(agent_id)))
        });
        let (id, owner): (Arc<str>, Owner) = match subagent {
            Some((parent, agent_id)) => (
                format!("{parent}/agent-{agent_id}").into(),
                Owner::Subagent {
                    parent_session_id: parent,
                    agent_id,
                },
            ),
            None => {
                let id: Arc<str> = stem.into();
                (id.clone(), Owner::Main { session_id: id })
            }
        };
        Some(SessionFile {
            key: SessionKey {
                source: self.source.clone(),
                id,
            },
            path: path.into(),
            owner,
        })
    }

    /// `SessionSource::read`：从 `from` 往后读到约 `max_bytes` 字节（软上限，至少一行）。
    pub fn read(
        &self,
        file: &SessionFile,
        from: &Cursor,
        max_bytes: u64,
    ) -> Result<Parsed, SessionError> {
        self.parse(file, from, max_bytes, None)
    }

    /// 读到末尾，同时更新 `index`（「数据流与线程」第 7 步：同一遍解析）。`from` 应当是
    /// `index` 的游标；对不上时从头读，`index` 一并重建。
    fn parse(
        &self,
        file: &SessionFile,
        from: &Cursor,
        max_bytes: u64,
        mut index: Option<&mut IndexBuilder>,
    ) -> Result<Parsed, SessionError> {
        let mut f = open(&file.path)?;
        let len = f.metadata().map_err(io_error)?.len();
        let start = self.start(&mut f, len, from).map_err(io_error)?;
        let (reset, offset, prefix, state) = match start {
            Start::Begin => (false, 0, Vec::new(), State::default()),
            Start::Resume {
                offset,
                prefix,
                state,
            } => (false, offset, prefix, state),
            Start::Reset => (true, 0, Vec::new(), State::default()),
        };
        if reset && let Some(index) = index.as_deref_mut() {
            index.clear();
        }
        f.seek(SeekFrom::Start(offset)).map_err(io_error)?;
        let mut lines = Lines::new(f, offset, &prefix);
        let mut parse = Parse {
            owner: &file.owner,
            state,
            records: Vec::new(),
            index,
            stats: ParseStats::default(),
        };
        let mut at_end = true;
        loop {
            match lines.next().map_err(io_error)? {
                Next::End => break,
                Next::Oversized(_) => parse.stats.oversized_lines += 1,
                Next::Line(pos) => {
                    if !parse.state.header && pos >= HEAD {
                        // 开头 256 KB 里开始的行里没有会话头：认不出格式，交出空的一批。
                        return Ok(Parsed {
                            batch: Batch {
                                reset,
                                records: Vec::new(),
                                cursor: if reset { Cursor::START } else { from.clone() },
                                at_end: true,
                            },
                            stats: parse.stats,
                        });
                    }
                    parse.line(pos, lines.line());
                }
            }
            if lines.pos() - offset >= max_bytes {
                break;
            }
        }
        let end = lines.pos();
        let state = codec::join_state(lines.prefix(), &parse.state.encode());
        if end - offset >= max_bytes {
            at_end = !lines.has_line().map_err(io_error)?;
        }
        if parse.stats.skipped()
            && let Some(index) = parse.index.as_deref_mut()
        {
            index.mark_partial();
        }
        let cursor = if end == 0 {
            Cursor::START
        } else {
            Cursor::new(self.source.clone(), ADAPTER_VERSION, end, state)
        };
        Ok(Parsed {
            batch: Batch {
                reset,
                records: parse.records,
                cursor,
                at_end,
            },
            stats: parse.stats,
        })
    }

    /// 核对游标：来源 ID、适配器版本、`state` 的格式、偏移之前的 256 字节和解析器状态都对得上才续读。
    fn start(&self, f: &mut File, len: u64, from: &Cursor) -> io::Result<Start> {
        if from.is_start() {
            return Ok(Start::Begin);
        }
        if from.source() != Some(&self.source) || from.adapter_version() != Some(ADAPTER_VERSION) {
            return Ok(Start::Reset);
        }
        let Some((prefix, parser)) = codec::split_state(from.state()) else {
            return Ok(Start::Reset);
        };
        let offset = from.offset();
        let expected = usize::try_from(offset.min(PREFIX as u64)).unwrap_or(PREFIX);
        if offset > len || prefix.len() != expected {
            return Ok(Start::Reset);
        }
        let mut actual = vec![0; expected];
        f.seek(SeekFrom::Start(offset - expected as u64))?;
        f.read_exact(&mut actual)?;
        if actual != prefix {
            return Ok(Start::Reset);
        }
        let Some(state) = State::decode(parser) else {
            return Ok(Start::Reset);
        };
        Ok(Start::Resume {
            offset,
            prefix: actual,
            state,
        })
    }

    /// 解析整个文件得到轮次索引（没有缓存时的 `SessionSource::turns`）。
    pub fn turns(&self, file: &SessionFile) -> Result<TurnIndex, SessionError> {
        self.turns_after(file, None)
    }

    /// 从已有的索引继续：游标仍然有效时只解析之后追加的部分，否则从头重建。
    pub fn turns_after(
        &self,
        file: &SessionFile,
        prev: Option<&TurnIndex>,
    ) -> Result<TurnIndex, SessionError> {
        let (mut builder, from) = match prev {
            Some(prev) => (IndexBuilder::from_index(prev), prev.cursor.clone()),
            None => (IndexBuilder::default(), Cursor::START),
        };
        let parsed = self.parse(file, &from, u64::MAX, Some(&mut builder))?;
        if !self.recognized(&parsed.batch.cursor) {
            return Err(SessionError::Unrecognized);
        }
        Ok(builder.build(file.key.clone(), parsed.batch.cursor))
    }

    /// 解析整个文件：全部记录和轮次索引（`testing::parse_file`）。
    pub fn parse_all(&self, file: &SessionFile) -> Result<(Parsed, TurnIndex), SessionError> {
        let mut builder = IndexBuilder::default();
        let parsed = self.parse(file, &Cursor::START, u64::MAX, Some(&mut builder))?;
        if !self.recognized(&parsed.batch.cursor) {
            return Err(SessionError::Unrecognized);
        }
        let index = builder.build(file.key.clone(), parsed.batch.cursor.clone());
        Ok((parsed, index))
    }

    /// 读到末尾之后，游标里的解析器状态见过会话头。
    fn recognized(&self, cursor: &Cursor) -> bool {
        self.state(cursor).is_some_and(|s| s.header)
    }

    fn state(&self, cursor: &Cursor) -> Option<State> {
        if cursor.source() != Some(&self.source)
            || cursor.adapter_version() != Some(ADAPTER_VERSION)
        {
            return None;
        }
        let (_, parser) = codec::split_state(cursor.state())?;
        State::decode(parser)
    }

    /// 读到 `cursor` 为止的最后活动时间和 `open_turn`。游标不是本适配器的、或是 `START` 时为 None。
    pub fn activity(&self, cursor: &Cursor) -> Option<Activity> {
        let state = self.state(cursor)?;
        Some(Activity {
            last_activity: state.last_at,
            open_turn: state.open_turn,
        })
    }

    /// `SessionSource::load`：按偏移读一行，交出其中的一部分。
    pub fn load(&self, file: &SessionFile, at: &ContentRef) -> Result<Content, SessionError> {
        load::load(&file.path, at)
    }

    /// 会话摘要（第 4 步的发现用它生成 `SessionSummary`）：读开头、末尾和子 agent 的 `meta.json`。`place` 为 `Pending`。
    /// 读不到 cwd 的文件返回 `Unrecognized`（不列出，只计数）。
    pub fn summarize(
        &self,
        file: &SessionFile,
    ) -> Result<(SessionSummary, ParseStats), SessionError> {
        summary::summarize(file)
    }

    /// `SessionSource::resume`：主会话给出 `claude --resume` 的会话 ID；子 agent 会话、认不出格式的会话
    /// 和别的来源的会话返回 None。
    pub fn resume(&self, summary: &SessionSummary) -> Option<ResumeCommand> {
        if summary.key.source != self.source
            || summary.parent.is_some()
            || summary.format == Format::Unrecognized
        {
            return None;
        }
        Some(ResumeCommand::ClaudeCode {
            session_id: summary.key.id.clone(),
        })
    }
}

fn open(path: &Path) -> Result<File, SessionError> {
    File::open(path).map_err(io_error)
}
