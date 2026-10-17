//! 游标 `state` 字节的编码（docs/design/agents.md「游标与批量读取」）。
//!
//! `services` 编码游标的外层（来源 ID、适配器版本、偏移）；`state` 是本 crate 自己的字节：
//! 格式版本、偏移之前最多 256 字节的原样副本、适配器的解析器状态。

use std::path::Path;
use std::sync::Arc;

/// `state` 的格式版本。改动解析器状态的布局时同时升适配器版本，旧游标从头读。
const STATE_FORMAT: u8 = 1;

/// 拆开 `state`：前缀副本和解析器状态。格式不认识时返回 None。
pub(crate) fn split_state(state: &[u8]) -> Option<(&[u8], &[u8])> {
    let (&format, rest) = state.split_first()?;
    if format != STATE_FORMAT {
        return None;
    }
    let (len, rest) = rest.split_first_chunk::<2>()?;
    rest.split_at_checked(usize::from(u16::from_le_bytes(*len)))
}

/// 拼出 `state`。
pub(crate) fn join_state(prefix: &[u8], parser: &[u8]) -> Vec<u8> {
    let len = u16::try_from(prefix.len()).expect("the prefix copy is at most 256 bytes");
    let mut out = Vec::with_capacity(3 + prefix.len() + parser.len());
    out.push(STATE_FORMAT);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(prefix);
    out.extend_from_slice(parser);
    out
}

/// 解析器状态的写入端。
#[derive(Default)]
pub(crate) struct Writer(Vec<u8>);

impl Writer {
    pub(crate) fn bool(&mut self, v: bool) -> &mut Self {
        self.0.push(u8::from(v));
        self
    }

    pub(crate) fn i64(&mut self, v: Option<i64>) -> &mut Self {
        match v {
            None => self.0.push(0),
            Some(v) => {
                self.0.push(1);
                self.0.extend_from_slice(&v.to_le_bytes());
            }
        }
        self
    }

    pub(crate) fn str(&mut self, v: Option<&str>) -> &mut Self {
        match v {
            None => self.0.push(0),
            Some(v) => {
                self.0.push(1);
                let len = u32::try_from(v.len()).unwrap_or(u32::MAX);
                self.0.extend_from_slice(&len.to_le_bytes());
                self.0.extend_from_slice(&v.as_bytes()[..len as usize]);
            }
        }
        self
    }

    pub(crate) fn path(&mut self, v: Option<&Path>) -> &mut Self {
        // 日志里的路径来自 JSON 字符串，总是 UTF-8。
        self.str(v.map(|p| p.to_str().unwrap_or_default()))
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        self.0
    }
}

/// 解析器状态的读取端。任何一处不对都返回 None，调用方从头读。
pub(crate) struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Reader(bytes)
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(head)
    }

    pub(crate) fn bool(&mut self) -> Option<bool> {
        match self.take(1)?[0] {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }

    pub(crate) fn i64(&mut self) -> Option<Option<i64>> {
        if !self.bool()? {
            return Some(None);
        }
        Some(Some(i64::from_le_bytes(self.take(8)?.try_into().ok()?)))
    }

    pub(crate) fn str(&mut self) -> Option<Option<Arc<str>>> {
        if !self.bool()? {
            return Some(None);
        }
        let len = u32::from_le_bytes(self.take(4)?.try_into().ok()?);
        let bytes = self.take(usize::try_from(len).ok()?)?;
        Some(Some(std::str::from_utf8(bytes).ok()?.into()))
    }

    pub(crate) fn path(&mut self) -> Option<Option<Arc<Path>>> {
        Some(self.str()?.map(|s| Arc::from(Path::new(&*s))))
    }

    /// 全部读完才算有效。
    pub(crate) fn end(self) -> Option<()> {
        self.0.is_empty().then_some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_round_trips() {
        let mut w = Writer::default();
        w.bool(true)
            .i64(None)
            .i64(Some(-5))
            .str(Some("héllo"))
            .str(None)
            .path(Some(Path::new("/Users/dev/fixture")));
        let parser = w.finish();
        let state = join_state(b"abc\n", &parser);
        let (prefix, rest) = split_state(&state).unwrap();
        assert_eq!(prefix, b"abc\n");
        let mut r = Reader::new(rest);
        assert_eq!(r.bool(), Some(true));
        assert_eq!(r.i64(), Some(None));
        assert_eq!(r.i64(), Some(Some(-5)));
        assert_eq!(r.str(), Some(Some("héllo".into())));
        assert_eq!(r.str(), Some(None));
        assert_eq!(
            r.path(),
            Some(Some(Arc::from(Path::new("/Users/dev/fixture"))))
        );
        assert_eq!(r.end(), Some(()));
    }

    #[test]
    fn codec_rejects_malformed() {
        assert_eq!(split_state(&[]), None);
        assert_eq!(split_state(&[9, 0, 0]), None);
        assert_eq!(split_state(&[STATE_FORMAT, 5, 0, 1]), None);
        assert_eq!(Reader::new(&[2]).bool(), None);
        assert_eq!(Reader::new(&[1, 9, 0, 0, 0, b'a']).str(), None);
        assert_eq!(Reader::new(&[1, 1, 0, 0, 0, 0xff]).str(), None);
        assert_eq!(Reader::new(&[0]).end(), None);
    }
}
