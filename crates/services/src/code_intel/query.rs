//! 查询与查询结果（docs/design/code_intel.md「服务接口」）。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{Candidate, DefinitionRef, Location, Position, Reference, Symbol, TextRef};
use crate::project::{ProjectKey, RelPath};

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Query {
    pub project: ProjectKey,
    pub kind: QueryKind,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum QueryKind {
    /// 光标所在符号的定义。
    Definition { at: TextRef, position: Position },
    /// 一个定义的引用与调用方。
    References { of: DefinitionRef },
    /// 按名字列出定义（⌘K 里选中一个符号名）。没有调用位置，所以候选没有绑定依据，
    /// 也没有第 1、2 层。
    Named {
        name: Arc<str>,
        near: Option<RelPath>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct QueryId(pub u64);

/// 不可变。每追加一批整体替换为新的 `Arc`，视图持有旧结果不受影响。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct QueryResults {
    pub id: QueryId,
    /// 每追加一批加一，与 `CodeIntelEvent::Results` 的 generation 一致。
    pub generation: u32,
    /// 位置上的符号。`Named` 查询只有名字。
    pub symbol: Option<Symbol>,
    /// `Definition`、`Named` 的候选。只追加：后到的一批排在已有条目之后，已有条目的
    /// 顺序和下标不变。
    pub candidates: Arc<[Candidate]>,
    /// `References` 的结果：解析回被查定义的引用（调用方）。只追加。
    pub callers: Arc<[Reference]>,
    /// `References` 的结果：同名、但没有解析回这个定义的引用。不计入调用方。只追加。
    pub same_name: Arc<[Reference]>,
    /// 有绑定依据的唯一候选在 `candidates` 里的下标：视图直接跳转。
    pub jump: Option<u32>,
    /// 视图预选的候选下标（`jump` 为 None、有候选时为第一个候选）。
    pub preselect: Option<u32>,
    /// 语义导航（#26）的结果。与 `candidates` 不同时才为 `Ready`，相同时保持 `Same`。
    pub semantic: SemanticState,
    pub state: QueryState,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum SemanticState {
    NotRequested,
    Pending,
    /// 与 `candidates`（或 `callers`）相同，视图不提示。
    Same,
    Ready(Arc<[Location]>),
    Failed,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum QueryState {
    Running,
    Done(Completeness),
    Failed(QueryError),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Completeness {
    /// 能参与回答的文件都已入索引或在这次查询里搜过；还在在途抽取里的文件算作没搜过。
    Complete,
    /// 限时搜索到时停止：当前子项目里还有 `unsearched` 个未入索引的文件没有搜，
    /// 子项目之外还有 `outside` 个未入索引。
    Partial { unsearched: u32, outside: u32 },
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum QueryError {
    NotOpen,
    /// 这个路径的语言没有可用的 grammar。
    UnsupportedLanguage,
    /// 位置不在标识符上。
    NoSymbol,
    /// `TextRef` 的内容已经变了：调用方在下一次刷新后用新的 OID 重新查询。
    Stale,
    /// 当前文件超过按需抽取的上限（4 MiB）。
    TooLarge,
    /// 读文件失败（附原因，日志用）。
    Unreadable(Arc<str>),
}
