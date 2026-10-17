//! 轮次索引与压缩会话摘要的缓存：内存里的字节 LRU，和 `cache` 应用级分库里的两个命名空间
//! （docs/design/agents.md「数据流与线程」第 7 步、「压缩会话的随机读取」；docs/design/cache.md 的命名空间表）。

use std::collections::HashMap;
use std::mem::size_of;
use std::sync::Arc;

use lectern_cache::{Compression, Entry, Key, Namespace};
use lectern_services::session::{SessionKey, SessionSummary, ToolKind, TurnIndex, TurnItem};
use serde::{Deserialize, Serialize};

use super::discover::FileId;
use crate::read::Summarized;
use crate::stats::ParseStats;

/// 内存里的轮次索引合计上限（「性能」）。
pub(crate) const MEMORY_BUDGET: usize = 8 << 20;

/// `agents.turns`：键是来源 ID、会话 ID、适配器版本；值是索引（含生成它时的游标），二进制编码见
/// `index_codec`。命名空间版本即索引格式版本：编码变化时加一。不压缩：打开会话时读它，解压的时间不划算。
pub(crate) struct CachedIndex(pub(crate) TurnIndex);

impl Entry for CachedIndex {
    const NAMESPACE: Namespace = Namespace {
        name: "agents.turns",
        version: 1,
        compression: Compression::None,
    };

    fn encode(&self, out: &mut Vec<u8>) {
        super::index_codec::encode(&self.0, out);
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        super::index_codec::decode(bytes).map(CachedIndex)
    }
}

pub(crate) fn index_key(session: &SessionKey, adapter_version: u32) -> Key {
    Key::new()
        .str(&session.source.0)
        .str(&session.id)
        .u32(adapter_version)
}

/// `agents.summary`：压缩会话补齐后的摘要与计数，以及继承边界依赖的来源会话。键是来源 ID、会话 ID、
/// 压缩文件的身份和适配器版本；命名空间版本即摘要格式版本。`place` 存为 `Pending`，读出后重新归位；
/// 由路径决定的字段和依赖来源会话的字段读出后也不照用（见 `Engine::summarize`）。
#[derive(Serialize, Deserialize)]
pub(crate) struct CachedSummary {
    pub(crate) summary: SessionSummary,
    unknown_types: Vec<(Arc<str>, u32)>,
    bad_lines: u32,
    oversized_lines: u32,
    lineage: Option<Arc<str>>,
}

impl CachedSummary {
    pub(crate) fn new(s: &Summarized) -> CachedSummary {
        let mut summary = s.summary.clone();
        summary.place = lectern_services::session::Place::Pending;
        CachedSummary {
            summary,
            unknown_types: s.stats.unknown_types.clone(),
            bad_lines: s.stats.bad_lines,
            oversized_lines: s.stats.oversized_lines,
            lineage: s.lineage.clone(),
        }
    }

    pub(crate) fn into_parts(self) -> Summarized {
        let stats = ParseStats {
            unknown_types: self.unknown_types,
            bad_lines: self.bad_lines,
            oversized_lines: self.oversized_lines,
            unknown_tools: Vec::new(),
        };
        Summarized {
            summary: self.summary,
            stats,
            lineage: self.lineage,
        }
    }
}

impl Entry for CachedSummary {
    const NAMESPACE: Namespace = Namespace {
        name: "agents.summary",
        version: 2,
        compression: Compression::None,
    };

    fn encode(&self, out: &mut Vec<u8>) {
        let _ = serde_json::to_writer(out, self);
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}

pub(crate) fn summary_key(session: &SessionKey, id: &FileId, adapter_version: u32) -> Key {
    Key::new()
        .str(&session.source.0)
        .str(&session.id)
        .u64(id.dev)
        .u64(id.ino)
        .u64(id.len)
        .bytes(&id.mtime_ns.to_be_bytes())
        .u32(adapter_version)
}

/// 内存里的轮次索引，按估算的字节数做 LRU。
#[derive(Default)]
pub(crate) struct Indexes {
    entries: HashMap<SessionKey, Held>,
    bytes: usize,
    tick: u64,
}

pub(crate) struct Held {
    pub(crate) index: Arc<TurnIndex>,
    /// 建索引时会话文件的身份：没有变化时 `turns` 只复制 `Arc`。
    pub(crate) file: Option<FileId>,
    bytes: usize,
    used: u64,
}

impl Indexes {
    pub(crate) fn get(&mut self, key: &SessionKey) -> Option<(Arc<TurnIndex>, Option<FileId>)> {
        self.tick += 1;
        let held = self.entries.get_mut(key)?;
        held.used = self.tick;
        Some((held.index.clone(), held.file))
    }

    /// 放进一个索引，按 LRU 淘汰到预算之内。比整个预算还大的索引不留在内存里（调用方用完就释放，
    /// 下次从 `cache` 读），同一会话的旧索引也一并去掉。
    pub(crate) fn put(&mut self, index: Arc<TurnIndex>, file: Option<FileId>) {
        self.tick += 1;
        let bytes = estimate(&index);
        let key = index.session.clone();
        if bytes > MEMORY_BUDGET {
            self.remove(&key);
            return;
        }
        if let Some(old) = self.entries.insert(
            key.clone(),
            Held {
                index,
                file,
                bytes,
                used: self.tick,
            },
        ) {
            self.bytes -= old.bytes;
        }
        self.bytes += bytes;
        while self.bytes > MEMORY_BUDGET && self.entries.len() > 1 {
            let Some(oldest) = self
                .entries
                .iter()
                .filter(|(k, _)| **k != key)
                .min_by_key(|(_, h)| h.used)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some(old) = self.entries.remove(&oldest) {
                self.bytes -= old.bytes;
            }
        }
    }

    pub(crate) fn remove(&mut self, key: &SessionKey) {
        if let Some(old) = self.entries.remove(key) {
            self.bytes -= old.bytes;
        }
    }

    #[cfg(test)]
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, key: &SessionKey) -> bool {
        self.entries.contains_key(key)
    }
}

/// 索引在内存里大约占多少字节：结构本身加上它拥有的字符串和路径。
pub(crate) fn estimate(index: &TurnIndex) -> usize {
    let mut n = size_of::<TurnIndex>() + index.cursor.state().len() + index.session.id.len();
    for turn in index.turns.iter() {
        n += size_of::<lectern_services::session::Turn>()
            + turn.id.0.len()
            + turn.prompt_preview.len();
        for item in turn.items.iter() {
            n += size_of::<TurnItem>();
            if let TurnItem::Tool(call) = item {
                n += call.id.0.len() + call.name.len();
                n += match &call.kind {
                    ToolKind::Edit { paths }
                    | ToolKind::Read { paths }
                    | ToolKind::Search { paths } => paths
                        .iter()
                        .map(|p| p.as_os_str().len() + 24)
                        .sum::<usize>(),
                    ToolKind::Execute {
                        command,
                        workdir,
                        exec_session,
                        ..
                    } => {
                        command.len()
                            + workdir.as_os_str().len()
                            + exec_session.as_ref().map_or(0, |s| s.len())
                    }
                    ToolKind::SpawnAgent | ToolKind::Other => 0,
                };
            }
        }
    }
    n
}
