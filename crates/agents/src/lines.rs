//! 逐行读取 JSONL（docs/design/agents.md「性能」：64 KB 缓冲，单行上限 16 MB，任何时候只持有一行）。

use std::io::{self, BufRead, BufReader, Read};

/// 单行上限。更长的行边读边丢弃到下一个换行，计入 `oversized_lines`。
pub(crate) const MAX_LINE: usize = 16 << 20;
/// 读缓冲。
pub(crate) const BUFFER: usize = 64 << 10;
/// 游标里保存的、偏移之前的原样字节数。
pub(crate) const PREFIX: usize = 256;
/// 发现阶段只看在这个范围之内开始的行（会话头、第一个 prompt）。
pub(crate) const HEAD: u64 = 256 << 10;

pub(crate) enum Next {
    /// 一个完整的行，从这个偏移开始；内容（不含换行）用 [`Lines::line`] 取。
    Line(u64),
    /// 超过 [`MAX_LINE`] 的行，从这个偏移开始，已经丢弃到它的换行之后。
    Oversized(u64),
    /// 没有完整的行了：到了文件末尾，或者只剩一行还没写完。
    End,
}

/// 从某个偏移开始顺序读完整的行。偏移只停在换行之后，最后一行没写完时不读它。
pub(crate) struct Lines<R> {
    reader: BufReader<R>,
    pos: u64,
    line: Vec<u8>,
    tail: Tail,
}

impl<R: Read> Lines<R> {
    /// `inner` 已经定位到 `pos`；`prefix` 是 `pos` 之前最多 [`PREFIX`] 字节的原样副本。
    pub(crate) fn new(inner: R, pos: u64, prefix: &[u8]) -> Self {
        let mut tail = Tail::default();
        tail.push(prefix);
        Lines {
            reader: BufReader::with_capacity(BUFFER, inner),
            pos,
            line: Vec::new(),
            tail,
        }
    }

    /// 已经消费到的偏移（总在一个换行之后，或者是起点）。
    pub(crate) fn pos(&self) -> u64 {
        self.pos
    }

    /// 上一次 [`Next::Line`] 的内容，不含换行。
    pub(crate) fn line(&self) -> &[u8] {
        &self.line
    }

    /// [`Lines::pos`] 之前最多 [`PREFIX`] 字节的原样副本。
    pub(crate) fn prefix(&self) -> &[u8] {
        &self.tail.0
    }

    pub(crate) fn next(&mut self) -> io::Result<Next> {
        // 读过一行很长的之后不留着大缓冲。
        if self.line.capacity() > 1 << 20 {
            self.line = Vec::new();
        }
        self.line.clear();
        let limit = MAX_LINE as u64 + 1;
        let n = (&mut self.reader)
            .take(limit)
            .read_until(b'\n', &mut self.line)?;
        if n == 0 {
            return Ok(Next::End);
        }
        let start = self.pos;
        if self.line.last() == Some(&b'\n') {
            self.pos += n as u64;
            self.tail.push(&self.line);
            self.line.pop();
            return Ok(Next::Line(start));
        }
        if (n as u64) < limit {
            // 文件末尾还没写完的一行。
            return Ok(Next::End);
        }
        // 超长：丢弃到下一个换行；还没有换行时它是没写完的行，不越过。
        let mut skipped = n as u64;
        let mut tail = Tail::default();
        tail.push(&self.line);
        self.line = Vec::new();
        loop {
            let buf = self.reader.fill_buf()?;
            if buf.is_empty() {
                return Ok(Next::End);
            }
            if let Some(i) = buf.iter().position(|&b| b == b'\n') {
                tail.push(&buf[..=i]);
                self.reader.consume(i + 1);
                skipped += i as u64 + 1;
                break;
            }
            let len = buf.len();
            tail.push(buf);
            self.reader.consume(len);
            skipped += len as u64;
        }
        self.tail.push(&tail.0);
        self.pos = start + skipped;
        Ok(Next::Oversized(start))
    }

    /// [`Lines::pos`] 之后是否还有一个完整的行。会读走缓冲里的内容，之后不能再调用 [`Lines::next`]。
    pub(crate) fn has_line(mut self) -> io::Result<bool> {
        loop {
            let buf = self.reader.fill_buf()?;
            if buf.is_empty() {
                return Ok(false);
            }
            if buf.contains(&b'\n') {
                return Ok(true);
            }
            let len = buf.len();
            self.reader.consume(len);
        }
    }
}

/// 最近消费的最多 [`PREFIX`] 字节。
#[derive(Default)]
struct Tail(Vec<u8>);

impl Tail {
    fn push(&mut self, bytes: &[u8]) {
        if bytes.len() >= PREFIX {
            self.0.clear();
            self.0.extend_from_slice(&bytes[bytes.len() - PREFIX..]);
            return;
        }
        self.0.extend_from_slice(bytes);
        if self.0.len() > PREFIX {
            let cut = self.0.len() - PREFIX;
            self.0.drain(..cut);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Collected = (Vec<(u64, Vec<u8>)>, u64, Vec<u8>);

    fn collect(data: &[u8]) -> Collected {
        let mut lines = Lines::new(data, 0, b"");
        let mut out = Vec::new();
        loop {
            match lines.next().unwrap() {
                Next::Line(pos) => out.push((pos, lines.line().to_vec())),
                Next::Oversized(pos) => out.push((pos, b"<oversized>".to_vec())),
                Next::End => break,
            }
        }
        (out, lines.pos(), lines.prefix().to_vec())
    }

    #[test]
    fn lines_stop_before_an_unfinished_line() {
        let (lines, pos, prefix) = collect(b"a\n\nbc\nunfinished");
        assert_eq!(
            lines,
            [(0, b"a".to_vec()), (2, b"".to_vec()), (3, b"bc".to_vec())]
        );
        assert_eq!(pos, 6);
        assert_eq!(prefix, b"a\n\nbc\n");
    }

    #[test]
    fn lines_prefix_keeps_the_last_256_bytes() {
        let mut data = vec![b'x'; 300];
        data.push(b'\n');
        data.extend_from_slice(b"y\n");
        let (_, pos, prefix) = collect(&data);
        assert_eq!(pos, 303);
        assert_eq!(prefix.len(), PREFIX);
        assert_eq!(&prefix[..], &data[303 - PREFIX..303]);
    }

    #[test]
    fn lines_has_line_looks_past_the_buffer() {
        let mut data = b"a\n".to_vec();
        data.extend(std::iter::repeat_n(b'z', BUFFER * 2));
        let mut lines = Lines::new(&data[..], 0, b"");
        assert!(matches!(lines.next().unwrap(), Next::Line(0)));
        assert!(!lines.has_line().unwrap());

        data.push(b'\n');
        let mut lines = Lines::new(&data[..], 0, b"");
        assert!(matches!(lines.next().unwrap(), Next::Line(0)));
        assert!(lines.has_line().unwrap());
    }
}
