//! 游标与批量读取（docs/design/agents.md「游标与批量读取」）。

use serde::{Deserialize, Serialize};

use super::{Record, SourceId};

/// 不透明，可序列化。消费方原样保存，不解读。
///
/// 外层由本模块规定：生成它的来源 ID、适配器版本、（解压后的）字节偏移。其余内容
/// （偏移之前的 256 字节副本、解析器状态）是提供者自己的 `state` 字节，本模块不解读。
/// 序列化为 `to_bytes` 的字节。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(into = "Vec<u8>", try_from = "Vec<u8>")]
pub struct Cursor(Option<Position>);

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Position {
    source: SourceId,
    adapter_version: u32,
    offset: u64,
    state: Box<[u8]>,
}

/// `to_bytes` 的格式版本，写在第一个字节。
const FORMAT_VERSION: u8 = 1;

impl Cursor {
    /// 文件开头。
    pub const START: Cursor = Cursor(None);

    /// 提供者用：在 `offset` 处、带自己状态的游标。
    pub fn new(
        source: SourceId,
        adapter_version: u32,
        offset: u64,
        state: impl Into<Box<[u8]>>,
    ) -> Cursor {
        Cursor(Some(Position {
            source,
            adapter_version,
            offset,
            state: state.into(),
        }))
    }

    pub fn is_start(&self) -> bool {
        self.0.is_none()
    }

    /// 生成它的来源；`START` 为 None。
    pub fn source(&self) -> Option<&SourceId> {
        self.0.as_ref().map(|p| &p.source)
    }

    /// 生成它的适配器版本；`START` 为 None。
    pub fn adapter_version(&self) -> Option<u32> {
        self.0.as_ref().map(|p| p.adapter_version)
    }

    /// （解压后的）字节偏移；`START` 为 0。
    pub fn offset(&self) -> u64 {
        self.0.as_ref().map_or(0, |p| p.offset)
    }

    /// 提供者自己的状态；`START` 为空。
    pub fn state(&self) -> &[u8] {
        self.0.as_ref().map_or(&[], |p| &p.state)
    }

    /// 格式：版本字节；`START` 到此为止，否则依次是来源 ID 的长度（u16 LE）与 UTF-8、
    /// 适配器版本（u32 LE）、偏移（u64 LE）、其余为 `state`。
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = vec![FORMAT_VERSION];
        let Some(p) = &self.0 else {
            return out;
        };
        let source = p.source.0.as_bytes();
        // `SourceId` 是插件 ID，远短于 64 KB；更长的截断后 `from_bytes` 读出的来源不同，
        // `read` 从头读。
        let len = u16::try_from(source.len()).unwrap_or(u16::MAX);
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&source[..usize::from(len)]);
        out.extend_from_slice(&p.adapter_version.to_le_bytes());
        out.extend_from_slice(&p.offset.to_le_bytes());
        out.extend_from_slice(&p.state);
        out
    }

    /// 序列化格式不认识时返回 None，调用方从 `START` 读。只检查格式，不检查来源 ID
    /// 和适配器版本：那由 `SessionSource::read` 判断。
    pub fn from_bytes(bytes: &[u8]) -> Option<Cursor> {
        let (&version, rest) = bytes.split_first()?;
        if version != FORMAT_VERSION {
            return None;
        }
        if rest.is_empty() {
            return Some(Cursor::START);
        }
        let (len, rest) = rest.split_first_chunk::<2>()?;
        let len = usize::from(u16::from_le_bytes(*len));
        let (source, rest) = rest.split_at_checked(len)?;
        let source = std::str::from_utf8(source).ok()?;
        let (adapter_version, rest) = rest.split_first_chunk::<4>()?;
        let (offset, state) = rest.split_first_chunk::<8>()?;
        Some(Cursor::new(
            SourceId(source.into()),
            u32::from_le_bytes(*adapter_version),
            u64::from_le_bytes(*offset),
            state,
        ))
    }
}

impl From<Cursor> for Vec<u8> {
    fn from(cursor: Cursor) -> Self {
        cursor.to_bytes()
    }
}

impl TryFrom<Vec<u8>> for Cursor {
    type Error = &'static str;

    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        Cursor::from_bytes(&bytes).ok_or("unrecognized cursor format")
    }
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Batch {
    /// 为 true 时记录从文件开头重新给出，回放可能跨多批。消费方把回放的记录写进这个
    /// `SessionKey` 的新一代数据，读到 `at_end` 为 true 的一批时在同一个事务里删除旧一代、
    /// 把新一代设为当前；回放期间再次 `reset` 时丢弃未完成的新一代，重新开始。
    pub reset: bool,
    pub records: Vec<Positioned<Record>>,
    pub cursor: Cursor,
    /// 游标之后已没有完整的行（包括只剩一行还没写完的情况）。为 false 时 `cursor`
    /// 一定严格大于调用时的 `from`。
    pub at_end: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Positioned<T> {
    /// 这一行在（解压后的）文件里的字节偏移。不同行的 pos 严格递增；同一行产生的多条
    /// 记录 pos 相同，用 `seq` 区分。记录的顺序是 (pos, seq)。
    pub pos: u64,
    /// 同一行内的序号，从 0 开始。
    pub seq: u16,
    /// 继承自父会话或分叉来源的前缀。
    pub inherited: bool,
    pub value: T,
}
