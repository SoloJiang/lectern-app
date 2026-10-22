//! 准备好的展示行（`Review::rows`，docs/design/review.md「接口」）。

use std::ops::Range;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{FileKey, StepId, TurnRef};
use crate::changes::SideText;
use crate::project::BlobId;
use crate::session::SessionKey;

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum RowsKey {
    /// 基准到工作区的最终改动。
    Final { file: FileKey, context: u32 },
    /// 改动叙事里的一步（这一轮之前的版本到这一轮之后的版本）。
    Step { step: StepId, context: u32 },
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct FileRows {
    pub key: RowsKey,
    pub old: RowsSide,
    pub new: RowsSide,
    pub rows: Arc<[DiffRow]>,
    /// `DiffRow` 里的 `hunk` 是这里的下标。
    pub hunks: Arc<[HunkInfo]>,
    /// 配对行的词级差异：（`rows` 的下标，两侧的字节区间）；任一行超过 1000 字节的
    /// 不给出。
    pub inline: Arc<[(u32, InlineSpans)]>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct RowsSide {
    /// 这一侧内容的 OID；改动链里的中间版本按 git 的 blob 格式计算（不在对象库里）。
    pub oid: Option<BlobId>,
    pub text: SideText,
}

/// 与 `git::Row` 一一对应，另带所在 hunk 的下标（`Folded` 没有）。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum DiffRow {
    Context {
        old: u32,
        new: u32,
        old_byte: Option<u64>,
        new_byte: Option<u64>,
        hunk: Option<u32>,
    },
    Removed {
        old: u32,
        pair: Option<u32>,
        old_byte: Option<u64>,
        hunk: u32,
    },
    Added {
        new: u32,
        pair: Option<u32>,
        new_byte: Option<u64>,
        hunk: u32,
    },
    Folded {
        old: Range<u32>,
        new: Range<u32>,
        old_byte: Option<u64>,
        new_byte: Option<u64>,
    },
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct InlineSpans {
    pub old: Arc<[Range<u32>]>,
    pub new: Arc<[Range<u32>]>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct HunkInfo {
    pub old: Range<u32>,
    pub new: Range<u32>,
    pub attribution: HunkAttribution,
}

/// 视图的规则：`drifted > 0` 且 `turns`、`others` 都为空时是 Drifted，不显示归因；
/// 都不为空时显示归因，并标出其中 Drifted 的行数。`Step` 的 hunk 总是归到这一步，
/// `turns` 只有它的轮次。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct HunkAttribution {
    /// false：这个文件只有文件级归因，下面三项为空。
    pub lines: bool,
    pub turns: Arc<[TurnRef]>,
    pub others: Arc<[SessionKey]>,
    /// 没有来源的行数（新一侧加入的行，加上基准里被链外改动删掉的行）。
    pub drifted: u32,
}
