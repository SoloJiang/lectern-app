//! 从文件末尾向前逐行读（docs/design/agents.md「数据流与线程」第 2 步「末尾」）。

use std::io::{self, Read, Seek, SeekFrom};

use crate::lines::{BUFFER, MAX_LINE};

/// 向前最多处理的行数。
const MAX_LINES: usize = 64;
/// 向前最多读的字节数（与单行上限相同）。
const MAX_TOTAL: u64 = MAX_LINE as u64;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TailEnd {
    /// 回调认出了决定性的记录。
    Found,
    /// 正在向前读的那一行超过单行上限。
    Oversized,
    /// 读到了行数或字节数上限，或者到了文件开头，都没有找到。
    Exhausted,
}

/// 从末尾（`len` 处）向前，对每个完整的行（不含换行，最后没写完的行除外）调用 `f`，
/// 直到 `f` 返回 true。
pub(crate) fn scan_back<R: Read + Seek>(
    file: &mut R,
    len: u64,
    mut f: impl FnMut(&[u8]) -> bool,
) -> io::Result<TailEnd> {
    // `data` 是文件里 [start, start + data.len()) 的内容。
    let mut start = len;
    let mut data: Vec<u8> = Vec::new();
    let mut total = 0u64;

    // 先找到最后一个换行，丢掉它之后还没写完的部分。
    loop {
        if let Some(i) = data.iter().rposition(|&b| b == b'\n') {
            data.truncate(i);
            break;
        }
        if start == 0 || total >= MAX_TOTAL {
            return Ok(TailEnd::Exhausted);
        }
        let n = grow(file, &mut start, &mut data, &mut total)?;
        if n == 0 {
            return Ok(TailEnd::Exhausted);
        }
    }

    let mut lines = 0;
    loop {
        match data.iter().rposition(|&b| b == b'\n') {
            Some(i) => {
                lines += 1;
                if f(&data[i + 1..]) {
                    return Ok(TailEnd::Found);
                }
                data.truncate(i);
            }
            None if start == 0 => {
                return Ok(if f(&data) {
                    TailEnd::Found
                } else {
                    TailEnd::Exhausted
                });
            }
            None => {
                if data.len() > MAX_LINE {
                    return Ok(TailEnd::Oversized);
                }
                if total >= MAX_TOTAL + MAX_LINE as u64 {
                    return Ok(TailEnd::Exhausted);
                }
                grow(file, &mut start, &mut data, &mut total)?;
                continue;
            }
        }
        if lines >= MAX_LINES || total >= MAX_TOTAL {
            return Ok(TailEnd::Exhausted);
        }
    }
}

/// 在 `data` 前面补读一块：至少 64 KB，不少于已有的长度（按倍数增长，避免反复搬动），
/// 不让当前这一行超过单行上限加一个字节。
fn grow<R: Read + Seek>(
    file: &mut R,
    start: &mut u64,
    data: &mut Vec<u8>,
    total: &mut u64,
) -> io::Result<usize> {
    let want = BUFFER
        .max(data.len())
        .min(MAX_LINE + 1 - data.len().min(MAX_LINE));
    let n = usize::try_from((*start).min(want as u64)).unwrap_or(want);
    let from = *start - n as u64;
    file.seek(SeekFrom::Start(from))?;
    let mut chunk = vec![0; n + data.len()];
    file.read_exact(&mut chunk[..n])?;
    chunk[n..].copy_from_slice(data);
    *data = chunk;
    *start = from;
    *total += n as u64;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn lines_back(data: &[u8]) -> (Vec<Vec<u8>>, TailEnd) {
        let mut seen = Vec::new();
        let end = scan_back(&mut Cursor::new(data), data.len() as u64, |line| {
            seen.push(line.to_vec());
            false
        })
        .unwrap();
        (seen, end)
    }

    #[test]
    fn tail_lines_skip_the_unfinished_line() {
        let (seen, end) = lines_back(b"a\nbb\nccc\nunfinished");
        assert_eq!(seen, [b"ccc".to_vec(), b"bb".to_vec(), b"a".to_vec()]);
        assert_eq!(end, TailEnd::Exhausted);
        assert_eq!(lines_back(b"").0, Vec::<Vec<u8>>::new());
        assert_eq!(lines_back(b"no newline").0, Vec::<Vec<u8>>::new());
    }

    #[test]
    fn tail_lines_span_chunks() {
        let mut data = b"first\n".to_vec();
        data.extend(std::iter::repeat_n(b'x', BUFFER * 3 + 17));
        data.extend_from_slice(b"\nlast\n");
        let (seen, _) = lines_back(&data);
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0], b"last");
        assert_eq!(seen[1].len(), BUFFER * 3 + 17);
        assert_eq!(seen[2], b"first");
    }

    #[test]
    fn tail_lines_stop_after_64() {
        let data: Vec<u8> = (0..100)
            .flat_map(|i| format!("{i}\n").into_bytes())
            .collect();
        let (seen, end) = lines_back(&data);
        assert_eq!(seen.len(), MAX_LINES);
        assert_eq!(seen[0], b"99");
        assert_eq!(end, TailEnd::Exhausted);
    }
}
