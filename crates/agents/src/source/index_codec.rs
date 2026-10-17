//! `agents.turns` 的值：轮次索引的紧凑二进制编码。
//!
//! 打开一个大会话时要从 `cache` 读出整份索引（「性能」：命中加解析追加部分 < 10 ms），JSON 解码太慢，
//! 所以用字段序列：整数是 LEB128 变长（时间戳是 8 字节小端）；字符串在结构里只记长度，内容依次放在开头的
//! 一个文本区里，解码时整个文本区只校验一次 UTF-8。工具名和工作目录在一份索引内共用同一个 `Arc`。
//! 任何一处不对都返回 None（缓存未命中，重新解析）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lectern_services::session::{
    ContentRef, Cursor, Format, SessionKey, SourceId, Timestamp, ToolCall, ToolCallId, ToolKind,
    ToolStatus, Turn, TurnId, TurnIndex, TurnItem,
};

/// 编码格式的版本，与命名空间版本一起变。
const FORMAT: u8 = 1;

/// 一份索引里共用 `Arc` 的不同工具名与工作目录最多这么多个（线性查找，通常只有十几个）。
const MAX_SHARED: usize = 64;

pub(crate) fn encode(index: &TurnIndex, out: &mut Vec<u8>) {
    let mut w = W {
        out: Vec::new(),
        text: String::new(),
    };
    w.str(&index.session.source.0);
    w.str(&index.session.id);
    w.bytes(&index.cursor.to_bytes());
    w.u8(match index.format {
        Format::Known => 0,
        Format::Partial => 1,
        Format::Unrecognized => 2,
    });
    w.uint(index.turns.len() as u64);
    for turn in index.turns.iter() {
        w.str(&turn.id.0);
        w.uint(u64::from(turn.ordinal));
        w.time(turn.started_at);
        w.opt_time(turn.ended_at);
        w.opt_ref(turn.prompt.as_ref());
        w.str(&turn.prompt_preview);
        w.uint(turn.items.len() as u64);
        for item in turn.items.iter() {
            match item {
                TurnItem::AgentText(r) => {
                    w.u8(0);
                    w.cref(r);
                }
                TurnItem::Injected(r) => {
                    w.u8(1);
                    w.cref(r);
                }
                TurnItem::Tool(call) => {
                    w.u8(2);
                    w.tool(call);
                }
            }
        }
    }
    let W {
        out: structure,
        text,
    } = w;
    out.push(FORMAT);
    let mut head = W {
        out: std::mem::take(out),
        text: String::new(),
    };
    head.uint(text.len() as u64);
    *out = head.out;
    out.extend_from_slice(text.as_bytes());
    out.extend_from_slice(&structure);
}

pub(crate) fn decode(bytes: &[u8]) -> Option<TurnIndex> {
    let mut r = R {
        bytes,
        text: "",
        names: Vec::new(),
    };
    if r.u8()? != FORMAT {
        return None;
    }
    let text = r.bytes()?;
    r.text = std::str::from_utf8(text).ok()?;
    let source = SourceId(r.str()?);
    let id = r.str()?;
    let cursor = Cursor::from_bytes(r.bytes()?)?;
    let format = match r.u8()? {
        0 => Format::Known,
        1 => Format::Partial,
        2 => Format::Unrecognized,
        _ => return None,
    };
    let count = r.len()?;
    let mut turns = Vec::with_capacity(count);
    for _ in 0..count {
        let id = TurnId(r.str()?);
        let ordinal = u32::try_from(r.uint()?).ok()?;
        let started_at = r.time()?;
        let ended_at = r.opt_time()?;
        let prompt = r.opt_ref()?;
        let prompt_preview = r.str()?;
        let n = r.len()?;
        let mut items = Vec::with_capacity(n);
        for _ in 0..n {
            items.push(match r.u8()? {
                0 => TurnItem::AgentText(r.cref()?),
                1 => TurnItem::Injected(r.cref()?),
                2 => TurnItem::Tool(r.tool()?),
                _ => return None,
            });
        }
        turns.push(Turn {
            id,
            ordinal,
            started_at,
            ended_at,
            prompt,
            prompt_preview,
            items: items.into(),
        });
    }
    if !r.bytes.is_empty() || !r.text.is_empty() {
        return None;
    }
    Some(TurnIndex {
        session: SessionKey { source, id },
        cursor,
        turns: turns.into(),
        format,
    })
}

struct W {
    out: Vec<u8>,
    text: String,
}

impl W {
    fn u8(&mut self, v: u8) {
        self.out.push(v);
    }

    fn uint(&mut self, mut v: u64) {
        loop {
            let low = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                self.out.push(low);
                return;
            }
            self.out.push(low | 0x80);
        }
    }

    fn bytes(&mut self, b: &[u8]) {
        self.uint(b.len() as u64);
        self.out.extend_from_slice(b);
    }

    fn str(&mut self, s: &str) {
        self.uint(s.len() as u64);
        self.text.push_str(s);
    }

    fn path(&mut self, p: &Path) {
        // 日志里的路径来自 JSON 字符串，总是 UTF-8。
        self.str(p.to_str().unwrap_or_default());
    }

    fn opt_str(&mut self, s: Option<&str>) {
        match s {
            None => self.u8(0),
            Some(s) => {
                self.u8(1);
                self.str(s);
            }
        }
    }

    fn time(&mut self, t: Timestamp) {
        self.out.extend_from_slice(&t.0.to_le_bytes());
    }

    fn opt_time(&mut self, t: Option<Timestamp>) {
        match t {
            None => self.u8(0),
            Some(t) => {
                self.u8(1);
                self.time(t);
            }
        }
    }

    fn cref(&mut self, r: &ContentRef) {
        self.uint(r.pos());
        self.uint(u64::from(r.line_len()));
        self.uint(u64::from(r.part()));
    }

    fn opt_ref(&mut self, r: Option<&ContentRef>) {
        match r {
            None => self.u8(0),
            Some(r) => {
                self.u8(1);
                self.cref(r);
            }
        }
    }

    fn paths(&mut self, paths: &[PathBuf]) {
        self.uint(paths.len() as u64);
        for p in paths {
            self.path(p);
        }
    }

    fn tool(&mut self, call: &ToolCall) {
        self.str(&call.id.0);
        self.str(&call.name);
        match &call.kind {
            ToolKind::Edit { paths } => {
                self.u8(0);
                self.paths(paths);
            }
            ToolKind::Execute {
                command,
                workdir,
                exec_session,
                workdir_exact,
            } => {
                self.u8(1);
                self.str(command);
                self.path(workdir);
                self.opt_str(exec_session.as_deref());
                self.u8(u8::from(*workdir_exact));
            }
            ToolKind::Read { paths } => {
                self.u8(2);
                self.paths(paths);
            }
            ToolKind::Search { paths } => {
                self.u8(3);
                self.paths(paths);
            }
            ToolKind::SpawnAgent => self.u8(4),
            ToolKind::Other => self.u8(5),
        }
        self.time(call.started_at);
        match call.finished {
            None => self.u8(0),
            Some((at, status)) => {
                self.u8(1);
                self.time(at);
                self.u8(match status {
                    ToolStatus::Completed => 0,
                    ToolStatus::Failed => 1,
                    ToolStatus::Denied => 2,
                    ToolStatus::Interrupted => 3,
                });
            }
        }
        self.cref(&call.input);
        self.opt_ref(call.result.as_ref());
    }
}

struct R<'a> {
    bytes: &'a [u8],
    /// 文本区里还没读到的部分。
    text: &'a str,
    /// 工具名与工作目录：同一份索引里反复出现，共用一个 `Arc`。
    names: Vec<(&'a str, Arc<str>)>,
}

impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.bytes.split_at_checked(n)?;
        self.bytes = rest;
        Some(head)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn uint(&mut self) -> Option<u64> {
        let mut v: u64 = 0;
        for shift in (0..64).step_by(7) {
            let b = self.u8()?;
            v |= u64::from(b & 0x7f).checked_shl(shift)?;
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }

    /// 一个元素个数；不可能比剩下的字节还多。
    fn len(&mut self) -> Option<usize> {
        let n = usize::try_from(self.uint()?).ok()?;
        (n <= self.bytes.len()).then_some(n)
    }

    fn bytes(&mut self) -> Option<&'a [u8]> {
        let n = usize::try_from(self.uint()?).ok()?;
        self.take(n)
    }

    /// 文本区里的下一个字符串（结构里记着它的长度）。
    fn text(&mut self) -> Option<&'a str> {
        let n = usize::try_from(self.uint()?).ok()?;
        let s = self.text.get(..n)?;
        self.text = &self.text[n..];
        Some(s)
    }

    fn str(&mut self) -> Option<Arc<str>> {
        Some(self.text()?.into())
    }

    fn shared(&mut self) -> Option<Arc<str>> {
        let b = self.text()?;
        if let Some((_, s)) = self.names.iter().find(|(k, _)| *k == b) {
            return Some(s.clone());
        }
        let s: Arc<str> = b.into();
        if self.names.len() < MAX_SHARED {
            self.names.push((b, s.clone()));
        }
        Some(s)
    }

    fn opt_str(&mut self) -> Option<Option<Arc<str>>> {
        match self.u8()? {
            0 => Some(None),
            1 => Some(Some(self.str()?)),
            _ => None,
        }
    }

    fn time(&mut self) -> Option<Timestamp> {
        Some(Timestamp(i64::from_le_bytes(
            self.take(8)?.try_into().ok()?,
        )))
    }

    fn opt_time(&mut self) -> Option<Option<Timestamp>> {
        match self.u8()? {
            0 => Some(None),
            1 => Some(Some(self.time()?)),
            _ => None,
        }
    }

    fn cref(&mut self) -> Option<ContentRef> {
        let pos = self.uint()?;
        let len = u32::try_from(self.uint()?).ok()?;
        let part = u16::try_from(self.uint()?).ok()?;
        Some(ContentRef::new(pos, len, part))
    }

    fn opt_ref(&mut self) -> Option<Option<ContentRef>> {
        match self.u8()? {
            0 => Some(None),
            1 => Some(Some(self.cref()?)),
            _ => None,
        }
    }

    fn paths(&mut self) -> Option<Arc<[PathBuf]>> {
        let n = self.len()?;
        let mut paths = Vec::with_capacity(n);
        for _ in 0..n {
            paths.push(PathBuf::from(self.text()?));
        }
        Some(paths.into())
    }

    fn tool(&mut self) -> Option<ToolCall> {
        let id = ToolCallId(self.str()?);
        let name = self.shared()?;
        let kind = match self.u8()? {
            0 => ToolKind::Edit {
                paths: self.paths()?,
            },
            1 => {
                let command = self.str()?;
                let workdir: Arc<Path> = Path::new(&*self.shared()?).into();
                let exec_session = self.opt_str()?;
                let workdir_exact = match self.u8()? {
                    0 => false,
                    1 => true,
                    _ => return None,
                };
                ToolKind::Execute {
                    command,
                    workdir,
                    exec_session,
                    workdir_exact,
                }
            }
            2 => ToolKind::Read {
                paths: self.paths()?,
            },
            3 => ToolKind::Search {
                paths: self.paths()?,
            },
            4 => ToolKind::SpawnAgent,
            5 => ToolKind::Other,
            _ => return None,
        };
        let started_at = self.time()?;
        let finished = match self.u8()? {
            0 => None,
            1 => {
                let at = self.time()?;
                let status = match self.u8()? {
                    0 => ToolStatus::Completed,
                    1 => ToolStatus::Failed,
                    2 => ToolStatus::Denied,
                    3 => ToolStatus::Interrupted,
                    _ => return None,
                };
                Some((at, status))
            }
            _ => return None,
        };
        let input = self.cref()?;
        let result = self.opt_ref()?;
        Some(ToolCall {
            id,
            name,
            kind,
            started_at,
            finished,
            input,
            result,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::ClaudeCode;
    use crate::codex::Codex;

    /// Every fixture's index round-trips; every truncation is rejected.
    #[test]
    fn index_cache_encoding_round_trips_the_fixtures() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let claude = ClaudeCode::new(SourceId(crate::claude::SOURCE_ID.into()));
        let codex = Codex::without_decoded_copies(SourceId(crate::codex::SOURCE_ID.into()));
        let mut indexes = Vec::new();
        for (_, rel) in crate::claude::tests::fixture_files() {
            let path = crate::claude::tests::project_dir().join(rel);
            indexes.push(claude.turns(&claude.session_file(&path).unwrap()).unwrap());
        }
        for (_, rel) in crate::codex::tests::fixture_files() {
            let path = root.join("codex").join(rel);
            indexes.push(codex.turns(&codex.session_file(&path).unwrap()).unwrap());
        }
        let mut kinds = std::collections::HashSet::new();
        for index in &indexes {
            for item in index.turns.iter().flat_map(|t| t.items.iter()) {
                if let TurnItem::Tool(call) = item {
                    kinds.insert(std::mem::discriminant(&call.kind));
                }
            }
            let mut out = Vec::new();
            encode(index, &mut out);
            assert_eq!(decode(&out).as_ref(), Some(index));
            for cut in [0, 1, out.len() / 2, out.len() - 1] {
                assert_eq!(decode(&out[..cut]), None, "cut at {cut}");
            }
            let mut longer = out.clone();
            longer.push(0);
            assert_eq!(decode(&longer), None, "trailing bytes");
        }
        assert!(kinds.len() >= 5, "the fixtures cover the tool kinds");
    }
}
