//! 会话摘要：读文件开头、末尾和子 agent 的 `meta.json`（docs/design/agents.md「数据流与线程」第 2 步）。

use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use lectern_services::session::{
    Agent, Format, Parent, Place, SessionError, SessionKey, SessionSummary, Timestamp, ToolCallId,
};

use super::format::{AgentMeta, Line};
use super::parse::{UserKind, assistant_leaves_open, text_head, user_kind, user_leaves_open};
use super::{Owner, SessionFile, io_error};
use crate::diag::{self, FirstSkip, Skip, Stage};
use crate::index::preview;
use crate::lines::{HEAD, Lines, Next, oversized_len};
use crate::stats::ParseStats;
use crate::tail::{TailEnd, scan_back};
use crate::time;

/// 会话的基本信息，取自一条记录。
#[derive(Clone)]
struct Basics {
    cwd: Arc<Path>,
    version: Option<Arc<str>>,
    branch: Option<Arc<str>>,
    at: Option<Timestamp>,
}

#[derive(Default)]
struct Head {
    /// 任何一条带 `cwd` 的记录。
    any_cwd: Option<Arc<Path>>,
    /// 会话头：第一条带 `sessionId` 和 `cwd` 的记录。
    header: Option<Basics>,
    /// 第一条自己的（不是继承的）`user` / `assistant` 记录。
    own: Option<Basics>,
    first_at: Option<Timestamp>,
    own_title: Option<Arc<str>>,
    any_title: Option<Arc<str>>,
    first_user_seen: bool,
    stats: ParseStats,
    skip: FirstSkip,
}

impl Head {
    fn done(&self) -> bool {
        self.own.is_some() && self.own_title.is_some()
    }

    fn bad(&mut self, pos: u64, bytes: &[u8], kind: Skip) {
        self.stats.bad_lines += 1;
        self.skip.note(Some(pos), Some(bytes.len() as u64), kind);
    }

    fn line(&mut self, owner: &Owner, pos: u64, bytes: &[u8]) {
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return;
        }
        let line = match serde_json::from_slice::<Line>(bytes) {
            Ok(line) => line,
            Err(e) => return self.bad(pos, bytes, Skip::of_json(&e)),
        };
        let Some(kind) = line.kind.as_deref() else {
            return self.bad(pos, bytes, Skip::Missing);
        };
        let at = line.timestamp.as_deref().and_then(time::parse);
        let cwd: Option<Arc<Path>> = line.cwd.as_deref().map(|c| Path::new(c).into());
        let basics = |cwd: Arc<Path>| Basics {
            cwd,
            version: line.version.as_deref().map(Arc::from),
            branch: line
                .git_branch
                .as_deref()
                .filter(|b| !b.is_empty())
                .map(Arc::from),
            at,
        };
        if self.any_cwd.is_none() {
            self.any_cwd.clone_from(&cwd);
        }
        if self.first_at.is_none() {
            self.first_at = at;
        }
        if self.header.is_none()
            && line.session_id.is_some()
            && let Some(cwd) = &cwd
        {
            self.header = Some(basics(cwd.clone()));
        }
        match kind {
            "user" | "assistant" => {}
            other => {
                if !super::parse::is_known_ignored(other) {
                    self.stats.unknown_type(other);
                    self.skip
                        .note(Some(pos), Some(bytes.len() as u64), Skip::UnknownType);
                }
                return;
            }
        }
        if self.header.is_none() {
            return self.bad(pos, bytes, Skip::NoHeader);
        }
        if line.message.is_none() || at.is_none() {
            return self.bad(pos, bytes, Skip::Missing);
        }
        let inherited = owner.inherits(line.session_id.as_deref(), line.agent_id.as_deref());
        if !inherited && self.own.is_none() {
            let cwd = cwd.or_else(|| self.header.as_ref().map(|h| h.cwd.clone()));
            self.own = cwd.map(basics);
        }
        if kind != "user" {
            return;
        }
        let subagent_task =
            matches!(owner, Owner::Subagent { .. }) && !inherited && !self.first_user_seen;
        if !inherited {
            self.first_user_seen = true;
        }
        if user_kind(&line, subagent_task) == UserKind::Prompt && line.uuid.is_some() {
            let title = preview(text_head(
                line.message.as_ref().and_then(|m| m.content.as_ref()),
            ));
            if !inherited && self.own_title.is_none() {
                self.own_title = Some(title.clone());
            }
            if self.any_title.is_none() {
                self.any_title = Some(title);
            }
        }
    }
}

/// 从末尾得到的字段。
struct Tail {
    last_activity: Option<Timestamp>,
    open_turn: bool,
    oversized: bool,
}

fn tail(f: &mut File, len: u64) -> Result<Tail, SessionError> {
    let mut last_at = None;
    let mut open = false;
    let end = scan_back(f, len, |bytes| {
        let Ok(line) = serde_json::from_slice::<Line>(bytes) else {
            return false;
        };
        let at = line.timestamp.as_deref().and_then(time::parse);
        match (line.kind.as_deref(), &line.message, at) {
            (Some("user"), Some(_), Some(at)) => {
                last_at.get_or_insert(at);
                open = user_leaves_open(&line);
                true
            }
            (Some("assistant"), Some(message), Some(at)) => {
                last_at.get_or_insert(at);
                open = assistant_leaves_open(message);
                true
            }
            (Some("user" | "assistant"), _, _) | (None, _, _) => false,
            (Some(_), _, at) => {
                if let Some(at) = at {
                    last_at.get_or_insert(at);
                }
                false
            }
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

pub(super) fn summarize(file: &SessionFile) -> Result<(SessionSummary, ParseStats), SessionError> {
    let mut f = File::open(&file.path).map_err(io_error)?;
    let meta = f.metadata().map_err(io_error)?;
    let len = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(Timestamp(0), |d| {
            Timestamp(i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        });

    let mut head = Head::default();
    {
        let mut lines = Lines::new(&mut f, 0, b"");
        loop {
            match lines.next().map_err(io_error)? {
                Next::End => break,
                Next::Oversized(pos) if pos < HEAD => {
                    head.stats.oversized_lines += 1;
                    head.skip
                        .note(Some(pos), Some(oversized_len(&lines, pos)), Skip::Oversized);
                }
                Next::Line(pos) if pos < HEAD => head.line(&file.owner, pos, lines.line()),
                Next::Oversized(_) | Next::Line(_) => break,
            }
            if head.done() || lines.pos() >= HEAD {
                break;
            }
        }
    }
    f.seek(SeekFrom::Start(0)).map_err(io_error)?;
    let tail = tail(&mut f, len)?;
    let mut stats = head.stats;
    let mut skip = head.skip;
    if tail.oversized {
        stats.oversized_lines += 1;
        skip.note(None, None, Skip::Oversized);
    }
    diag::parsed(Stage::Summary, &file.key.id, &file.path, 0, skip, &stats);

    let Some(any_cwd) = head.any_cwd else {
        return Err(SessionError::Unrecognized);
    };
    let format = if head.header.is_none() {
        Format::Unrecognized
    } else {
        stats.format()
    };
    let basics = head.own.or(head.header);
    let parent = match &file.owner {
        Owner::Main { .. } => None,
        Owner::Subagent {
            parent_session_id, ..
        } => Some(Parent {
            session: SessionKey {
                source: file.key.source.clone(),
                id: parent_session_id.clone(),
            },
            tool_call: file
                .meta_path()
                .and_then(|p| std::fs::read(p).ok())
                .and_then(|bytes| serde_json::from_slice::<AgentMeta>(&bytes).ok())
                .and_then(|m| m.tool_use_id)
                .map(|id| ToolCallId(id.into())),
        }),
    };
    let summary = SessionSummary {
        key: file.key.clone(),
        agent: Agent::ClaudeCode,
        parent,
        file: file.path.clone(),
        cwd: basics.as_ref().map_or(any_cwd, |b| b.cwd.clone()),
        place: Place::Pending,
        started_at: basics
            .as_ref()
            .and_then(|b| b.at)
            .or(head.first_at)
            .unwrap_or(mtime),
        last_activity: tail.last_activity.unwrap_or(mtime),
        title: head.own_title.or(head.any_title),
        agent_version: basics.as_ref().and_then(|b| b.version.clone()),
        logged_branch: basics.as_ref().and_then(|b| b.branch.clone()),
        open_turn: tail.open_turn,
        tail_pending: false,
        archived: false,
        inheritance_unknown: false,
        format,
    };
    Ok((summary, stats))
}
