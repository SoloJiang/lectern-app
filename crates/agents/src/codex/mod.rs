//! Codex 适配器（docs/design/agents.md 验收第 3 步）。
//!
//! 库层，形状与 [`crate::claude::ClaudeCode`] 相同：给定一个 rollout 文件，交出增量读取的记录、
//! 轮次索引、正文、会话摘要和恢复命令。`.jsonl.zst` 会话第一次 `read` / `turns` / `load` 时完整解压成
//! `decoded_dir` 下的只读副本，之后在副本上按偏移读（「压缩会话的随机读取」）。第 4 步的发现、监听与插件
//! 在它外面包一层：按 `SessionKey` 找到文件，并实现 [`Lineage`] 让分叉会话找到来源会话。

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lectern_services::session::{
    Batch, Content, ContentRef, Cursor, Format, ResumeCommand, SessionError, SessionKey,
    SessionSummary, SourceId, ToolCall, ToolKind, TurnIndex,
};
use serde::Deserialize;

use lectern_logging::Value;

use crate::codec;
use crate::diag::{self, FirstSkip, Skip, Stage};
use crate::index::IndexBuilder;
use crate::lines::{HEAD, Lines, Next, PREFIX, oversized_len};
use crate::read::{Summarized, io_error};
use crate::stats::ParseStats;

mod decoded;
mod format;
mod load;
mod parse;
mod summary;
mod tools;

#[cfg(test)]
pub(crate) mod tests;

use decoded::{CopyError, InUse, Store, ZstReader};
use parse::{Parse, State};

pub use crate::read::{Activity, Parsed};

/// 内置 Codex 来源的 ID（插件 ID）。
pub const SOURCE_ID: &str = "session-codex";

/// 适配器版本：解析规则或游标状态的布局变化时加一，旧游标从头读、旧索引不再命中。
pub const ADAPTER_VERSION: u32 = 1;

/// 一个 rollout 文件：路径、全局键，以及是否压缩、是否归档。
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct SessionFile {
    pub key: SessionKey,
    pub path: Arc<Path>,
    /// `.jsonl.zst`。
    pub compressed: bool,
    /// 在 `archived_sessions/` 之下。
    pub archived: bool,
}

/// 按线程 ID 找来源会话的文件：分叉会话的 `forked_from_id`、子 agent 的 `parent_thread_id`
/// （「外部格式」的「继承前缀」）。第 4 步按 `SessionList` 实现；不在本机时返回 None。
pub trait Lineage: Send + Sync {
    fn locate(&self, thread_id: &str) -> Option<SessionFile>;
}

/// 找不到任何来源会话。
struct NoLineage;

impl Lineage for NoLineage {
    fn locate(&self, _: &str) -> Option<SessionFile> {
        None
    }
}

/// 来源会话文件里全部 `task_started` 的 `turn_id`（含来源自己继承的轮次），以及扫描到的位置，
/// 来源追加之后从这里接着扫。
pub(crate) struct SourceTurns {
    offset: u64,
    prefix: Vec<u8>,
    pub(crate) ids: HashSet<Arc<str>>,
}

/// 内存里最多记这么多个来源会话的轮次 ID 集合（第 4 步随轮次索引一起缓存）。
const MAX_SOURCES: usize = 64;

/// Codex 适配器。可以在多个线程上同时使用：解析状态在游标里，只有解压副本的使用登记和来源集合的缓存
/// 各用一把只在内存操作时短暂持有的锁。
#[derive(Clone)]
pub struct Codex {
    source: SourceId,
    store: Arc<Store>,
    lineage: Arc<dyn Lineage>,
    sources: Arc<Mutex<HashMap<Arc<Path>, Arc<SourceTurns>>>>,
}

impl fmt::Debug for Codex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Codex")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

/// 从哪里开始读。
enum Start {
    Begin,
    Resume {
        offset: u64,
        prefix: Vec<u8>,
        state: Box<State>,
        turns: Option<Arc<SourceTurns>>,
    },
    Reset,
}

/// 读取的对象：未压缩的文件、解压副本，或者（副本写不出来时）从头流式解码。
enum Body<'s> {
    File(File, Option<InUse<'s>>),
    Stream(Box<ZstReader<BufReader<File>>>),
}

impl Read for Body<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Body::File(f, _) => f.read(buf),
            Body::Stream(r) => r.read(buf),
        }
    }
}

impl Body<'_> {
    /// 定位到 `offset`，交出它之前的 `prefix` 个字节；内容比 `offset` 短时返回 None。
    fn seek_to(&mut self, offset: u64, prefix: usize) -> io::Result<Option<Vec<u8>>> {
        let from = offset - prefix as u64;
        let mut actual = vec![0; prefix];
        match self {
            Body::File(f, _) => {
                if offset > f.metadata()?.len() {
                    return Ok(None);
                }
                f.seek(SeekFrom::Start(from))?;
                f.read_exact(&mut actual)?;
            }
            Body::Stream(r) => {
                if io::copy(&mut r.take(from), &mut io::sink())? < from {
                    return Ok(None);
                }
                match r.read_exact(&mut actual) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(Some(actual))
    }
}

impl Codex {
    /// `decoded_dir`：压缩会话的解压副本目录（`AgentsDeps::decoded_dir`），副本合计上限 1 GB。
    pub fn new(source: SourceId, decoded_dir: PathBuf) -> Self {
        Codex {
            source,
            store: Arc::new(Store::new(Some(decoded_dir), decoded::DEFAULT_BUDGET)),
            lineage: Arc::new(NoLineage),
            sources: Arc::default(),
        }
    }

    /// 不建解压副本：压缩会话的每次读取都从头流式解码（`testing::parse_file`、bench 的一遍解析）。
    pub fn without_decoded_copies(source: SourceId) -> Self {
        Codex {
            source,
            store: Arc::new(Store::new(None, 0)),
            lineage: Arc::new(NoLineage),
            sources: Arc::default(),
        }
    }

    /// 改变解压副本的合计上限（字节）。在开始使用之前调用。
    #[must_use]
    pub fn with_decoded_budget(mut self, bytes: u64) -> Self {
        let dir = self.store.dir().map(Path::to_path_buf);
        self.store = Arc::new(Store::new(dir, bytes));
        self
    }

    /// 按线程 ID 找来源会话（默认找不到任何来源）。
    #[must_use]
    pub fn with_lineage(mut self, lineage: Arc<dyn Lineage>) -> Self {
        self.lineage = lineage;
        self
    }

    pub fn source(&self) -> &SourceId {
        &self.source
    }

    /// 按文件名认出 rollout：`rollout-<YYYY-MM-DDThh-mm-ss>-<线程 ID>[_<rollout ID>].jsonl` 或 `.jsonl.zst`。
    /// 会话 ID 取文件名里的线程 ID（Codex 用 `session_meta.id` 命名文件，`rollout_file_name.rs`）。
    pub fn session_file(&self, path: &Path) -> Option<SessionFile> {
        let name = path.file_name()?.to_str()?;
        let (core, compressed) = match name.strip_suffix(".jsonl.zst") {
            Some(core) => (core, true),
            None => (name.strip_suffix(".jsonl")?, false),
        };
        let core = core.strip_prefix("rollout-")?;
        if core.get(19..20)? != "-" {
            return None;
        }
        let ids = core.get(20..)?;
        let thread = ids.split_once('_').map_or(ids, |(t, _)| t);
        if !is_uuid(thread) {
            return None;
        }
        Some(SessionFile {
            key: SessionKey {
                source: self.source.clone(),
                id: thread.into(),
            },
            path: path.into(),
            compressed,
            archived: path
                .ancestors()
                .skip(1)
                .any(|a| a.file_name().is_some_and(|n| n == "archived_sessions")),
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

    /// `read`，同时在 `index` 上接着建轮次索引（「数据流与线程」第 7 步：同一遍解析）。`index` 应当建到
    /// `from` 为止；读取从头回放（`Batch::reset`）时它被清空、从头重建。
    pub(crate) fn read_indexed(
        &self,
        file: &SessionFile,
        from: &Cursor,
        max_bytes: u64,
        index: &mut IndexBuilder,
    ) -> Result<Parsed, SessionError> {
        self.parse(file, from, max_bytes, Some(index))
    }

    /// 打开读取的对象。压缩会话先找解压副本，没有就建一个；建不出来时从头流式解码（第二个值为 true）。
    fn body(&self, file: &SessionFile) -> Result<(Body<'_>, bool), SessionError> {
        if !file.compressed {
            return Ok((
                Body::File(File::open(&file.path).map_err(io_error)?, None),
                false,
            ));
        }
        match self.store.copy(&self.source.0, &file.key.id, &file.path) {
            Ok(guard) => {
                let f = File::open(&guard.path).map_err(io_error)?;
                Ok((Body::File(f, Some(guard)), false))
            }
            Err(CopyError::Source(e)) => Err(io_error(e)),
            Err(CopyError::NoDir) => Ok((
                Body::Stream(Box::new(decoded::decoder(&file.path).map_err(io_error)?)),
                true,
            )),
            Err(CopyError::Copy(step, e)) => {
                {
                    let _scope = lectern_logging::scope(diag::session_ctx(&file.key.id));
                    lectern_logging::warn!(
                        "decoded copy not written; decoding from the start";
                        path = Value::path(&file.path),
                        step = step,
                        io = Value::io(&e),
                    );
                }
                Ok((
                    Body::Stream(Box::new(decoded::decoder(&file.path).map_err(io_error)?)),
                    true,
                ))
            }
        }
    }

    /// 读到 `max_bytes`，同时更新 `index`（「数据流与线程」第 7 步：同一遍解析）。`from` 应当是
    /// `index` 的游标；对不上时从头读，`index` 一并重建。
    fn parse(
        &self,
        file: &SessionFile,
        from: &Cursor,
        max_bytes: u64,
        mut index: Option<&mut IndexBuilder>,
    ) -> Result<Parsed, SessionError> {
        let (mut body, streamed) = self.body(file)?;
        // 从头解码时不分批：分批会让每一批都从头解码一次。
        let max_bytes = if streamed { u64::MAX } else { max_bytes };
        let mut start = self.start(from);
        if let Start::Resume { offset, prefix, .. } = &start
            && body
                .seek_to(*offset, prefix.len())
                .map_err(io_error)?
                .as_ref()
                != Some(prefix)
        {
            start = Start::Reset;
        }
        let (reset, offset, prefix, state, turns) = match start {
            Start::Begin => (false, 0, Vec::new(), State::default(), None),
            Start::Resume {
                offset,
                prefix,
                state,
                turns,
            } => (false, offset, prefix, *state, turns),
            Start::Reset => {
                // 流已经读过一段：重新打开。
                (body, _) = self.body(file)?;
                (true, 0, Vec::new(), State::default(), None)
            }
        };
        if reset && let Some(index) = index.as_deref_mut() {
            index.clear();
        }
        if let Body::File(f, _) = &mut body {
            f.seek(SeekFrom::Start(offset)).map_err(io_error)?;
        }
        let lineage = |thread: &str| self.source_turns(thread);
        let mut lines = Lines::new(body, offset, &prefix);
        let mut parse = Parse {
            state,
            records: Vec::new(),
            index,
            stats: ParseStats::default(),
            lineage: &lineage,
            source_turns: turns,
            first_prompt: None,
            meta: None,
            any_cwd: None,
            skip: FirstSkip::default(),
            cur: (0, 0),
        };
        let mut at_end = true;
        loop {
            match lines.next().map_err(io_error)? {
                Next::End => break,
                Next::Oversized(pos) => {
                    parse.stats.oversized_lines += 1;
                    parse
                        .skip
                        .note(Some(pos), Some(oversized_len(&lines, pos)), Skip::Oversized);
                }
                Next::Line(pos) => {
                    if !parse.state.header && pos >= HEAD {
                        // 开头 256 KB 里开始的行里没有会话头：认不出格式，交出空的一批。
                        diag::parsed(
                            Stage::Read,
                            &file.key.id,
                            &file.path,
                            offset,
                            parse.skip,
                            &parse.stats,
                        );
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
        diag::parsed(
            Stage::Read,
            &file.key.id,
            &file.path,
            offset,
            parse.skip,
            &parse.stats,
        );
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

    /// 核对游标：来源 ID、适配器版本、`state` 的格式和解析器状态都对得上，继承前缀的边界也还能判断，
    /// 才续读（偏移之前的 256 字节在打开文件之后比较）。边界未定的分叉，来源会话现在找得到了：从头读。
    fn start(&self, from: &Cursor) -> Start {
        if from.is_start() {
            return Start::Begin;
        }
        if from.source() != Some(&self.source) || from.adapter_version() != Some(ADAPTER_VERSION) {
            return Start::Reset;
        }
        let Some((prefix, parser)) = codec::split_state(from.state()) else {
            return Start::Reset;
        };
        let offset = from.offset();
        if prefix.len() != usize::try_from(offset.min(PREFIX as u64)).unwrap_or(PREFIX) {
            return Start::Reset;
        }
        let Some(state) = State::decode(parser) else {
            return Start::Reset;
        };
        let mut turns = None;
        match &state.boundary {
            parse::Boundary::Unknown { source } => {
                if self.source_turns(source).is_some() {
                    return Start::Reset;
                }
            }
            _ => {
                if let Some(source) = state.needs_source_turns() {
                    match self.source_turns(source) {
                        Some(t) => turns = Some(t),
                        // 来源在回放中途消失：从头读，按边界未定处理。
                        None => return Start::Reset,
                    }
                }
            }
        }
        Start::Resume {
            offset,
            prefix: prefix.to_vec(),
            state: Box::new(state),
            turns,
        }
    }

    /// 来源会话（按线程 ID）文件里全部 `task_started` 的 `turn_id`。找不到或读不了来源时为 None。
    /// 结果按来源文件缓存在内存里，来源追加之后只扫追加的部分。
    pub(crate) fn source_turns(&self, thread: &str) -> Option<Arc<SourceTurns>> {
        let file = self.lineage.locate(thread)?;
        let cached = self
            .sources
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&file.path)
            .cloned();
        let turns = self.scan_turns(&file, cached.as_deref()).ok()?;
        let turns = match (cached, turns) {
            (Some(old), None) => old,
            (_, Some(new)) => {
                let new = Arc::new(new);
                let mut sources = self.sources.lock().unwrap_or_else(|e| e.into_inner());
                if sources.len() >= MAX_SOURCES && !sources.contains_key(&file.path) {
                    let evict = sources.keys().next().cloned();
                    if let Some(k) = evict {
                        sources.remove(&k);
                    }
                }
                sources.insert(file.path.clone(), new.clone());
                new
            }
            (None, None) => unreachable!("a scan without a previous result always returns one"),
        };
        Some(turns)
    }

    /// 只取内存里已有的来源集合，不读来源文件（摘要用：发现阶段不解压、不扫描祖先会话）。
    /// 没有缓存时为 None，摘要按边界未定处理；第一次 `read` / `turns` 这个分叉时才扫描来源。
    pub(crate) fn cached_source_turns(&self, thread: &str) -> Option<Arc<SourceTurns>> {
        let file = self.lineage.locate(thread)?;
        self.sources
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&file.path)
            .cloned()
    }

    #[cfg(test)]
    pub(crate) fn cached_sources(&self) -> usize {
        self.sources.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// 从 `prev` 的位置接着扫（对不上时从头扫）。没有新内容时返回 None。
    fn scan_turns(
        &self,
        file: &SessionFile,
        prev: Option<&SourceTurns>,
    ) -> Result<Option<SourceTurns>, SessionError> {
        let (mut body, _) = self.body(file)?;
        let mut start = prev.map(|p| (p.offset, p.prefix.clone(), p.ids.clone()));
        if let Some((offset, prefix, _)) = &start
            && body
                .seek_to(*offset, prefix.len())
                .map_err(io_error)?
                .as_ref()
                != Some(prefix)
        {
            start = None;
            (body, _) = self.body(file)?;
        }
        let (offset, prefix, mut ids) = start.unwrap_or_default();
        let mut lines = Lines::new(body, offset, &prefix);
        let mut grew = prev.is_none() || offset == 0;
        while let Next::Line(_) | Next::Oversized(_) = lines.next().map_err(io_error)? {
            grew = true;
            let bytes = lines.line();
            if !contains(bytes, b"task_started") && !contains(bytes, b"turn_started") {
                continue;
            }
            if let Ok(line) = serde_json::from_slice::<format::Line>(bytes)
                && let format::Payload::Event(format::Event::TurnStarted {
                    turn_id: Some(turn),
                }) = line.payload
            {
                ids.insert(Arc::from(&*turn));
            }
        }
        Ok(grew.then(|| SourceTurns {
            offset: lines.pos(),
            prefix: lines.prefix().to_vec(),
            ids,
        }))
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
        self.turns_continued(file, prev).map(|(index, _)| index)
    }

    /// `turns_after`，另给出新索引是否接在 `prev` 之后（有 `prev`，游标仍有效、没有从头重建）。
    pub(crate) fn turns_continued(
        &self,
        file: &SessionFile,
        prev: Option<&TurnIndex>,
    ) -> Result<(TurnIndex, bool), SessionError> {
        let (mut builder, from) = match prev {
            Some(prev) => (IndexBuilder::from_index(prev), prev.cursor.clone()),
            None => (IndexBuilder::default(), Cursor::START),
        };
        let parsed = self.parse(file, &from, u64::MAX, Some(&mut builder))?;
        if !self.recognized(&parsed.batch.cursor) {
            return Err(SessionError::Unrecognized);
        }
        let continued = prev.is_some() && !parsed.batch.reset;
        Ok((
            builder.build(file.key.clone(), parsed.batch.cursor),
            continued,
        ))
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

    /// `SessionSource::load`：按偏移读一行，交出其中的一部分。压缩会话在解压副本上读。
    pub fn load(&self, file: &SessionFile, at: &ContentRef) -> Result<Content, SessionError> {
        let (body, _) = self.body(file)?;
        load::load(body, at)
    }

    /// 读这次 `spawn_agent` 调用的输出（对 `call.result` 一次 `load`），取子线程 ID（docs/design/agents.md
    /// 「Codex 子 agent 与派出它的调用」）。阻塞，只在 executor 上调用。`call.kind` 不是 `SpawnAgent`、调用没有
    /// 结果、输出不是 JSON 对象或顶层没有规范写法的 `agent_id` 时为 None。
    pub fn spawned_thread(
        &self,
        file: &SessionFile,
        call: &ToolCall,
    ) -> Result<Option<Arc<str>>, SessionError> {
        if call.kind != ToolKind::SpawnAgent {
            return Ok(None);
        }
        let Some(result) = &call.result else {
            return Ok(None);
        };
        Ok(match self.load(file, result)? {
            Content::Text(output) => spawned_agent_id(&output),
            Content::Edits(_) => None,
        })
    }

    /// 会话摘要（第 4 步的发现用它生成 `SessionSummary`）：读开头 256 KB 和末尾，`place` 为 `Pending`。
    /// 压缩会话只流式解压开头，末尾字段标 `tail_pending`（`last_activity` 暂取修改时间），由 [`Codex::complete`] 补齐。
    /// 读不到 cwd 的文件返回 `Unrecognized`（不列出，只计数）。
    pub fn summarize(
        &self,
        file: &SessionFile,
    ) -> Result<(SessionSummary, ParseStats), SessionError> {
        summary::summarize(self, file, false, false).map(|s| (s.summary, s.stats))
    }

    /// `summarize`，另给出继承边界依赖的来源会话（第 4 步）。
    pub(crate) fn summarize_lineage(&self, file: &SessionFile) -> Result<Summarized, SessionError> {
        summary::summarize(self, file, false, false)
    }

    /// `summarize`，但分叉与子 agent 的来源会话能找到时扫描它的轮次 ID（不只用已缓存的集合），所以继承边界
    /// 能定下来。读过这个会话之后、或来源会话后来出现时，第 4 步用它重新生成摘要。
    pub(crate) fn summarize_with_sources(
        &self,
        file: &SessionFile,
    ) -> Result<Summarized, SessionError> {
        summary::summarize(self, file, false, true)
    }

    /// 补齐压缩会话的末尾字段：整份流式解码一次（不建副本），`tail_pending` 为 false。未压缩的会话与 `summarize` 相同。
    pub fn complete(
        &self,
        file: &SessionFile,
    ) -> Result<(SessionSummary, ParseStats), SessionError> {
        summary::summarize(self, file, true, false).map(|s| (s.summary, s.stats))
    }

    /// `complete`，另给出继承边界依赖的来源会话（第 4 步）。
    pub(crate) fn complete_lineage(&self, file: &SessionFile) -> Result<Summarized, SessionError> {
        summary::summarize(self, file, true, false)
    }

    /// 删掉这个会话的解压副本（压缩文件消失时，第 4 步的发现调用）。
    pub fn remove_decoded(&self, key: &SessionKey) {
        self.store.remove(&self.source.0, &key.id);
    }

    /// `SessionSource::resume`：给出 `codex resume` 的会话 ID；子 agent 会话（有 `parent`，父会话未知时它的 ID 为空）、
    /// 认不出格式的会话和别的来源的会话返回 None。
    pub fn resume(&self, summary: &SessionSummary) -> Option<ResumeCommand> {
        if summary.key.source != self.source
            || summary.parent.is_some()
            || summary.format == Format::Unrecognized
        {
            return None;
        }
        Some(ResumeCommand::Codex {
            session_id: summary.key.id.clone(),
        })
    }

    #[cfg(test)]
    pub(crate) fn decodes(&self) -> u64 {
        self.store.decodes()
    }

    #[cfg(test)]
    pub(crate) fn hold_copy(&self, file: &SessionFile) -> Option<PathBuf> {
        let guard = self
            .store
            .copy(&self.source.0, &file.key.id, &file.path)
            .ok()?;
        let path = guard.path.clone();
        // 测试里故意不释放：这个副本一直算作正在使用。
        std::mem::forget(guard);
        Some(path)
    }
}

/// `spawn_agent` 的输出：multi_agent v1 的 `SpawnAgentResult`（`{"agent_id": "<子线程 ID>", "nickname": …}`，
/// openai/codex `codex-rs/core/src/tools/handlers/multi_agents/spawn.rs`）。只取顶层的 `agent_id`，必须是 UUID 的
/// 规范写法；v2（`{"task_name": …}`）、派出失败的文本和其他形状没有子线程。
fn spawned_agent_id(output: &str) -> Option<Arc<str>> {
    #[derive(Deserialize)]
    struct Spawned<'a> {
        #[serde(borrow)]
        agent_id: Option<Cow<'a, str>>,
    }
    // 只认 JSON 对象（serde 的结构也接受数组）。
    if !output.trim_start().starts_with('{') {
        return None;
    }
    let spawned: Spawned = serde_json::from_str(output).ok()?;
    let id = spawned.agent_id?;
    is_uuid(&id).then(|| Arc::from(&*id))
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// UUID 的规范写法（8-4-4-4-12 个十六进制数字）。
fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}
