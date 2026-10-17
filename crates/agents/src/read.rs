//! 两个适配器共用的读取结果（docs/design/agents.md「本 crate 的公开项」）。

use std::io;

use lectern_services::session::{Batch, SessionError, Timestamp};

use crate::stats::ParseStats;

/// 一次 `read` 的结果：交给消费方的 `Batch`，以及要并进 `Diagnostics` 的计数。
#[derive(Clone, PartialEq, Debug)]
pub struct Parsed {
    pub batch: Batch,
    pub stats: ParseStats,
}

/// 从解析器状态里取出的末尾字段（「数据流与线程」第 2 步：已经在增量读取的文件不再读末尾）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Activity {
    pub last_activity: Option<Timestamp>,
    pub open_turn: bool,
}

/// 一份摘要和生成它时的计数，以及它的继承边界依赖哪个来源会话（Codex 分叉与子 agent 的来源线程 ID；
/// 没有继承前缀、或边界由 `ordinal` 决定时为 None）。来源会话出现或消失时，第 4 步据此重新生成摘要。
#[derive(Clone, Debug)]
pub(crate) struct Summarized {
    pub(crate) summary: lectern_services::session::SessionSummary,
    pub(crate) stats: ParseStats,
    pub(crate) lineage: Option<std::sync::Arc<str>>,
}

pub(crate) fn io_error(e: io::Error) -> SessionError {
    if e.kind() == io::ErrorKind::NotFound {
        SessionError::NotFound
    } else {
        SessionError::Io(e.to_string().into())
    }
}
