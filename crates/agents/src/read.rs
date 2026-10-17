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

pub(crate) fn io_error(e: io::Error) -> SessionError {
    if e.kind() == io::ErrorKind::NotFound {
        SessionError::NotFound
    } else {
        SessionError::Io(e.to_string().into())
    }
}
