//! ⌘K 的符号名字表（docs/design/code_intel.md「服务接口」「名字表」）。

use std::fmt;
use std::sync::Arc;

use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};

/// `CodeIntel::names` 的结果。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Names {
    Ready(Arc<NameTable>),
    Building,
    /// 项目没有打开，或还没有任何可用的索引。
    Unavailable,
}

/// 当前 worktree 的定义名，去重（区分大小写）、按字节序排序。连续缓冲加结束
/// 偏移，与 `FileList` 的布局相同。统一为 NFC 由构造方（`code_intel`）在 push
/// 之前完成。
///
/// 序列化为 `{ generation, entries: [[名字, 种类位集], …] }`。反序列化经
/// `NameTableBuilder`，来自其他进程的乱序、重复条目按构造器的规则排序、合并。
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(from = "Wire")]
pub struct NameTable {
    generation: u64,
    buf: Box<str>,
    ends: Box<[u32]>,
    kinds: Box<[u16]>,
}

impl NameTable {
    pub fn builder(generation: u64) -> NameTableBuilder {
        NameTableBuilder {
            generation,
            entries: Vec::new(),
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn len(&self) -> usize {
        self.ends.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    pub fn get(&self, i: usize) -> Option<&str> {
        let end = *self.ends.get(i)? as usize;
        let start = if i == 0 { 0 } else { self.ends[i - 1] as usize };
        Some(&self.buf[start..end])
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> + '_ {
        (0..self.len()).filter_map(|i| self.get(i))
    }

    /// 这个名字出现过的 `DefKind` 的位集（位序号是 `DefKind` 的声明顺序）。
    /// 下标越界时为 0。
    pub fn kinds(&self, i: usize) -> u16 {
        self.kinds.get(i).copied().unwrap_or(0)
    }
}

impl Serialize for NameTable {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        struct Entries<'a>(&'a NameTable);
        impl Serialize for Entries<'_> {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let table = self.0;
                serializer.collect_seq(table.iter().enumerate().map(|(i, n)| (n, table.kinds(i))))
            }
        }
        let mut s = serializer.serialize_struct("NameTable", 2)?;
        s.serialize_field("generation", &self.generation)?;
        s.serialize_field("entries", &Entries(self))?;
        s.end()
    }
}

/// `NameTable` 的序列化形式。
#[derive(Deserialize)]
struct Wire {
    generation: u64,
    entries: Vec<(Box<str>, u16)>,
}

impl From<Wire> for NameTable {
    fn from(wire: Wire) -> Self {
        let mut b = NameTable::builder(wire.generation);
        for (name, kinds) in &wire.entries {
            b.push(name, *kinds);
        }
        b.build()
    }
}

impl fmt::Debug for NameTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        struct Entries<'a>(&'a NameTable);
        impl fmt::Debug for Entries<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let table = self.0;
                f.debug_list()
                    .entries(table.iter().enumerate().map(|(i, n)| (n, table.kinds(i))))
                    .finish()
            }
        }
        f.debug_struct("NameTable")
            .field("generation", &self.generation)
            .field("entries", &Entries(self))
            .finish()
    }
}

/// 收集 `NameTable` 的条目。`build` 按字节序排序；同名的条目合并为一条，种类位集
/// 取并集。名字不做规范化。
pub struct NameTableBuilder {
    generation: u64,
    entries: Vec<(Box<str>, u16)>,
}

impl NameTableBuilder {
    /// 一个定义名和它的种类位集（`1 << kind as u16`，或几个种类的并集）。
    pub fn push(&mut self, name: &str, kinds: u16) {
        self.entries.push((name.into(), kinds));
    }

    /// 总长度超出 `u32` 偏移的名字不收入。
    pub fn build(mut self) -> NameTable {
        self.entries.sort_by(|a, b| a.0.cmp(&b.0));
        self.entries.dedup_by(|b, a| {
            let same = a.0 == b.0;
            if same {
                a.1 |= b.1;
            }
            same
        });
        let mut buf = String::new();
        let mut ends = Vec::with_capacity(self.entries.len());
        let mut kinds = Vec::with_capacity(self.entries.len());
        for (name, k) in self.entries {
            let Ok(end) = u32::try_from(buf.len() + name.len()) else {
                continue;
            };
            buf.push_str(&name);
            ends.push(end);
            kinds.push(k);
        }
        NameTable {
            generation: self.generation,
            buf: buf.into_boxed_str(),
            ends: ends.into_boxed_slice(),
            kinds: kinds.into_boxed_slice(),
        }
    }
}
