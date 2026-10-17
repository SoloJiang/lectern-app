//! 会话摘要：读文件开头和末尾（docs/design/agents.md「数据流与线程」第 2 步）。
//!
//! 开头用与 `read` 相同的解析器读在 256 KB 之内开始的行，直到有了会话头和自己的第一个 prompt。
//! 分叉会话的继承边界只用内存里已缓存的来源集合，不读来源文件；没有缓存时按边界未定处理。
//! 末尾：未压缩的文件从末尾向前找最后一条 `task_started` / `task_complete` / `turn_aborted`；
//! `.jsonl.zst` 没有办法直接读末尾，`summarize` 只流式解压开头、标 `tail_pending`，`complete` 整份流式解码一次补齐。

use std::fs::File;
use std::io::Read;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use lectern_services::session::{
    Agent, Format, Parent, Place, SessionError, SessionKey, SessionSummary, Timestamp,
};

use super::format::{Event, Line, Payload};
use super::parse::{Boundary, Parse, State};
use super::{Codex, SessionFile, decoded};
use crate::index::IndexBuilder;
use crate::lines::{HEAD, Lines, Next};
use crate::read::io_error;
use crate::stats::ParseStats;
use crate::tail::{TailEnd, scan_back};
use crate::time;

/// 一行在末尾字段里的作用：时间戳，以及它是不是决定 `open_turn` 的记录。
fn tail_fields(bytes: &[u8]) -> (Option<Timestamp>, Option<bool>) {
    let Ok(line) = serde_json::from_slice::<Line>(bytes) else {
        return (None, None);
    };
    let at = line.timestamp.as_deref().and_then(time::parse);
    let open = match line.payload {
        Payload::Event(Event::TurnStarted { .. }) => Some(true),
        Payload::Event(Event::TurnEnded { .. }) => Some(false),
        _ => None,
    };
    (at, open)
}

struct Tail {
    last_activity: Option<Timestamp>,
    open_turn: bool,
    oversized: bool,
}

/// 未压缩的文件：从末尾向前。
fn tail_back(f: &mut File, len: u64) -> Result<Tail, SessionError> {
    let mut last_at = None;
    let mut open = false;
    let end = scan_back(f, len, |bytes| {
        let (at, decisive) = tail_fields(bytes);
        if let Some(at) = at {
            last_at.get_or_insert(at);
        }
        match decisive {
            Some(o) => {
                open = o;
                true
            }
            None => false,
        }
    })
    .map_err(io_error)?;
    Ok(match end {
        TailEnd::Found => Tail {
            last_activity: last_at,
            open_turn: open,
            oversized: false,
        },
        // 回退：末尾是超长的记录，或者在上限内没找到决定 `open_turn` 的记录。
        TailEnd::Oversized | TailEnd::Exhausted => Tail {
            last_activity: None,
            open_turn: true,
            oversized: end == TailEnd::Oversized,
        },
    })
}

/// 压缩文件：整份流式解码，从前往后取最后的时间戳和最后一条决定 `open_turn` 的记录。
fn tail_forward(reader: impl Read) -> Result<(Tail, u32), SessionError> {
    let mut lines = Lines::new(reader, 0, b"");
    let mut last_at = None;
    let mut open = None;
    let mut oversized = 0u32;
    loop {
        match lines.next().map_err(io_error)? {
            Next::End => break,
            Next::Oversized(_) => oversized += 1,
            Next::Line(_) => {
                let (at, decisive) = tail_fields(lines.line());
                if at.is_some() {
                    last_at = at;
                }
                if decisive.is_some() {
                    open = decisive;
                }
            }
        }
    }
    Ok((
        Tail {
            last_activity: last_at,
            open_turn: open.unwrap_or(false),
            oversized: false,
        },
        oversized,
    ))
}

/// 开头：用读取的解析器读到会话头和自己的第一个 prompt 为止。
struct Head {
    state: State,
    stats: ParseStats,
    own_title: Option<Arc<str>>,
    any_title: Option<Arc<str>>,
    meta: Option<super::parse::Meta>,
    any_cwd: Option<Arc<std::path::Path>>,
    first_at: Option<Timestamp>,
}

fn head(codex: &Codex, reader: impl Read) -> Result<Head, SessionError> {
    // 只用已缓存的来源集合：发现阶段不解压、不扫描祖先会话。
    let lineage = |thread: &str| codex.cached_source_turns(thread);
    let mut index = IndexBuilder::default();
    let mut parse = Parse {
        state: State::default(),
        records: Vec::new(),
        index: Some(&mut index),
        stats: ParseStats::default(),
        lineage: &lineage,
        source_turns: None,
        first_prompt: None,
        meta: None,
        any_cwd: None,
    };
    let mut lines = Lines::new(reader, 0, b"");
    let mut first_at = None;
    loop {
        match lines.next().map_err(io_error)? {
            Next::End => break,
            Next::Oversized(pos) if pos < HEAD => parse.stats.oversized_lines += 1,
            Next::Line(pos) if pos < HEAD => {
                parse.line(pos, lines.line());
                if first_at.is_none() {
                    first_at = parse.state.last_at;
                }
            }
            Next::Oversized(_) | Next::Line(_) => break,
        }
        // 自己的第一个 prompt 出现了就停（只占用开头的一点时间，见「与设计的差异」）。
        if parse.state.header
            && parse
                .index
                .as_deref()
                .is_some_and(|i| i.first_prompt().is_some())
        {
            break;
        }
        if lines.pos() >= HEAD {
            break;
        }
    }
    let own_title = parse.index.as_deref().and_then(IndexBuilder::first_prompt);
    Ok(Head {
        state: parse.state,
        stats: parse.stats,
        own_title,
        any_title: parse.first_prompt,
        meta: parse.meta,
        any_cwd: parse.any_cwd,
        first_at,
    })
}

pub(super) fn summarize(
    codex: &Codex,
    file: &SessionFile,
    complete: bool,
) -> Result<(SessionSummary, ParseStats), SessionError> {
    let meta = std::fs::metadata(&file.path).map_err(io_error)?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(Timestamp(0), |d| {
            Timestamp(i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        });

    let (head, tail, tail_pending) = if file.compressed {
        let head = head(codex, decoded::decoder(&file.path).map_err(io_error)?)?;
        if complete {
            let (tail, oversized) = tail_forward(decoded::decoder(&file.path).map_err(io_error)?)?;
            let mut head = head;
            head.stats.oversized_lines += oversized;
            (head, tail, false)
        } else {
            let tail = Tail {
                last_activity: None,
                open_turn: false,
                oversized: false,
            };
            (head, tail, true)
        }
    } else {
        let mut f = File::open(&file.path).map_err(io_error)?;
        let head = head(codex, &mut f)?;
        let tail = tail_back(&mut f, meta.len())?;
        (head, tail, false)
    };
    let mut stats = head.stats;
    if tail.oversized {
        stats.oversized_lines += 1;
    }

    let Some(any_cwd) = head.meta.as_ref().map(|m| m.cwd.clone()).or(head.any_cwd) else {
        return Err(SessionError::Unrecognized);
    };
    let format = if head.state.header {
        stats.format()
    } else {
        Format::Unrecognized
    };
    let parent = head.meta.as_ref().filter(|m| m.subagent).map(|m| Parent {
        // `{"subagent": "review"}` 一类没有父线程 ID：父会话未知时 ID 为空，不指向任何会话，
        // 但仍标出这是子 agent 会话（不能恢复，见「与设计的差异」）。
        session: SessionKey {
            source: file.key.source.clone(),
            id: m.parent_thread.clone().unwrap_or_else(|| "".into()),
        },
        // 子 agent 的 rollout 里没有派出它的调用 ID（见「与设计的差异」）。
        tool_call: None,
    });
    let summary = SessionSummary {
        key: file.key.clone(),
        agent: Agent::Codex,
        parent,
        file: file.path.clone(),
        cwd: any_cwd,
        place: Place::Pending,
        started_at: head
            .meta
            .as_ref()
            .and_then(|m| m.started_at)
            .or(head.first_at)
            .unwrap_or(mtime),
        last_activity: tail.last_activity.unwrap_or(mtime),
        title: head.own_title.or(head.any_title),
        agent_version: head.meta.as_ref().and_then(|m| m.cli_version.clone()),
        logged_branch: head.state.branch.clone(),
        open_turn: tail.open_turn,
        tail_pending,
        archived: file.archived,
        inheritance_unknown: matches!(head.state.boundary, Boundary::Unknown { .. }),
        format,
    };
    Ok((summary, stats))
}
