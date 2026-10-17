//! `.jsonl.zst` 的解码与解压副本（docs/design/agents.md「压缩会话的随机读取」）。
//!
//! Codex 把整个冷文件压成一个 zstd 帧，没有可以定位的帧边界。凡是要从某个偏移开始读的操作都用
//! `decoded_dir` 下的解压副本：第一次需要时完整解压（先写临时文件、完整后 `rename`，之后只读），
//! 键是来源 ID、会话 ID 和压缩文件的身份（设备号、inode、大小、修改时间）。全部副本合计不超过预算
//! （默认 1 GB），按最后访问时间（副本的修改时间，每次使用时更新）做字节 LRU，正在使用的副本不淘汰。

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use ruzstd::decoding::{FrameDecoder, StreamingDecoder};

use crate::lines::BUFFER;

/// 全部解压副本的默认预算。
pub(crate) const DEFAULT_BUDGET: u64 = 1 << 30;

/// 依次解码一个流里的全部 zstd 帧。Codex 只写一个帧；多帧的文件（被别的工具重新压缩过）同样能读。
pub(crate) struct ZstReader<R: BufRead> {
    decoder: Option<StreamingDecoder<R, FrameDecoder>>,
}

impl<R: BufRead> ZstReader<R> {
    pub(crate) fn new(inner: R) -> io::Result<Self> {
        let decoder = StreamingDecoder::new(inner).map_err(io::Error::other)?;
        Ok(ZstReader {
            decoder: Some(decoder),
        })
    }
}

impl<R: BufRead> Read for ZstReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let Some(decoder) = self.decoder.as_mut() else {
                return Ok(0);
            };
            let n = decoder.read(buf)?;
            if n > 0 || buf.is_empty() {
                return Ok(n);
            }
            // 这一帧结束了：后面还有字节就接着解码下一帧。
            if decoder.get_mut().fill_buf()?.is_empty() {
                self.decoder = None;
                return Ok(0);
            }
            let (source, frame) = self.decoder.take().expect("checked above").into_parts();
            self.decoder =
                Some(StreamingDecoder::new_with_decoder(source, frame).map_err(io::Error::other)?);
        }
    }
}

/// 打开一个 `.jsonl.zst` 的流式解码器（64 KB 的读缓冲）。
pub(crate) fn decoder(path: &Path) -> io::Result<ZstReader<BufReader<File>>> {
    ZstReader::new(BufReader::with_capacity(BUFFER, File::open(path)?))
}

/// 压缩文件的身份。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Identity {
    dev: u64,
    ino: u64,
    len: u64,
    mtime_ns: i128,
}

impl Identity {
    pub(crate) fn of(meta: &fs::Metadata) -> Identity {
        Identity {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.len(),
            mtime_ns: i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec()),
        }
    }
}

/// 解压副本的目录、预算和正在使用的副本。
pub(crate) struct Store {
    /// `None`：不建副本，总是从头解码（`testing::parse_file` 与 bench）。
    dir: Option<PathBuf>,
    budget: u64,
    in_use: Mutex<HashMap<PathBuf, usize>>,
    /// 为建副本做过的完整解码次数（测试检查一个归档只解压一次）。
    decodes: AtomicU64,
}

/// 正在使用的副本：持有期间不被淘汰。
pub(crate) struct InUse<'s> {
    store: &'s Store,
    pub(crate) path: PathBuf,
}

impl Drop for InUse<'_> {
    fn drop(&mut self) {
        let mut in_use = self.store.lock();
        if let Some(n) = in_use.get_mut(&self.path) {
            *n -= 1;
            if *n == 0 {
                in_use.remove(&self.path);
            }
        }
    }
}

/// 建副本失败（不是压缩文件本身读不到）：调用方退回从头解码。
pub(crate) enum CopyError {
    /// 压缩文件读不到（不存在、没有权限、解码失败）。
    Source(io::Error),
    /// 副本写不出来（目录不可写、磁盘满）。本 crate 还没有日志设施，原因不保留（见「与设计的差异」）。
    Copy,
}

impl Store {
    pub(crate) fn new(dir: Option<PathBuf>, budget: u64) -> Store {
        Store {
            dir,
            budget,
            in_use: Mutex::new(HashMap::new()),
            decodes: AtomicU64::new(0),
        }
    }

    pub(crate) fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<PathBuf, usize>> {
        self.in_use.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[cfg(test)]
    pub(crate) fn decodes(&self) -> u64 {
        self.decodes.load(Ordering::Relaxed)
    }

    fn hold(&self, path: PathBuf) -> InUse<'_> {
        *self.lock().entry(path.clone()).or_insert(0) += 1;
        InUse { store: self, path }
    }

    /// 来源的副本目录。
    fn source_dir(&self, source: &str) -> Option<PathBuf> {
        Some(self.dir.as_ref()?.join(sanitize(source)))
    }

    /// 这个会话的解压副本，没有就建一个。`session` 是会话 ID。
    pub(crate) fn copy(
        &self,
        source: &str,
        session: &str,
        zst: &Path,
    ) -> Result<InUse<'_>, CopyError> {
        let Some(dir) = self.source_dir(source) else {
            return Err(CopyError::Copy);
        };
        let meta = fs::metadata(zst).map_err(CopyError::Source)?;
        let id = Identity::of(&meta);
        let stem = sanitize(session);
        let name = format!(
            "{stem}-{:x}-{:x}-{:x}-{:x}.jsonl",
            id.dev, id.ino, id.len, id.mtime_ns
        );
        let path = dir.join(&name);
        let guard = self.hold(path.clone());
        if path.is_file() {
            touch(&path);
            return Ok(guard);
        }
        fs::create_dir_all(&dir).map_err(|_| CopyError::Copy)?;
        // 身份变了（重新压缩、被替换）：旧副本作废。
        self.remove_matching(&dir, &stem, &path);
        static TMP: AtomicU64 = AtomicU64::new(0);
        let tmp = dir.join(format!(
            ".{name}.tmp-{}-{}",
            std::process::id(),
            TMP.fetch_add(1, Ordering::Relaxed)
        ));
        let written = self.write_copy(zst, &tmp);
        if let Err(e) = written {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        fs::rename(&tmp, &path).map_err(|_| {
            let _ = fs::remove_file(&tmp);
            CopyError::Copy
        })?;
        self.decodes.fetch_add(1, Ordering::Relaxed);
        self.evict(&dir);
        Ok(guard)
    }

    fn write_copy(&self, zst: &Path, tmp: &Path) -> Result<(), CopyError> {
        let mut input = decoder(zst).map_err(CopyError::Source)?;
        let out = File::create(tmp).map_err(|_| CopyError::Copy)?;
        let mut out = io::BufWriter::with_capacity(BUFFER, out);
        let mut buf = vec![0; BUFFER];
        loop {
            let n = match input.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(CopyError::Source(e)),
            };
            out.write_all(&buf[..n]).map_err(|_| CopyError::Copy)?;
        }
        let out = out.into_inner().map_err(|_| CopyError::Copy)?;
        let mut perms = out.metadata().map_err(|_| CopyError::Copy)?.permissions();
        perms.set_readonly(true);
        out.set_permissions(perms).map_err(|_| CopyError::Copy)?;
        Ok(())
    }

    /// 删掉这个会话的其他副本（正在使用的除外）。
    fn remove_matching(&self, dir: &Path, stem: &str, keep: &Path) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        let prefix = format!("{stem}-");
        let in_use = self.lock();
        for entry in entries.flatten() {
            let path = entry.path();
            let matches = entry
                .file_name()
                .to_str()
                .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".jsonl"));
            if matches && path != keep && !in_use.contains_key(&path) {
                let _ = fs::remove_file(&path);
            }
        }
    }

    /// 删掉这个会话的全部副本（压缩文件消失时，第 4 步的发现调用）。
    pub(crate) fn remove(&self, source: &str, session: &str) {
        if let Some(dir) = self.source_dir(source) {
            self.remove_matching(&dir, &sanitize(session), Path::new(""));
        }
    }

    /// 合计超过预算时，按最后访问时间从旧到新删副本，正在使用的不删。
    fn evict(&self, dir: &Path) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        let mut copies: Vec<(SystemTime, u64, PathBuf)> = entries
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| !n.starts_with('.') && n.ends_with(".jsonl"))
            })
            .filter_map(|e| {
                let meta = e.metadata().ok()?;
                Some((meta.modified().unwrap_or(UNIX_EPOCH), meta.len(), e.path()))
            })
            .collect();
        let mut total: u64 = copies.iter().map(|c| c.1).sum();
        if total <= self.budget {
            return;
        }
        copies.sort();
        let in_use = self.lock();
        for (_, len, path) in copies {
            if total <= self.budget {
                break;
            }
            if in_use.contains_key(&path) {
                continue;
            }
            if fs::remove_file(&path).is_ok() {
                total -= len;
            }
        }
    }
}

/// 记下最后访问时间（副本的修改时间）。
fn touch(path: &Path) {
    if let Ok(f) = File::open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
}

/// 文件名里只留字母、数字、`-`、`_`、`.`。
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zst_reader_decodes_concatenated_frames() {
        use ruzstd::encoding::{CompressionLevel, compress_to_vec};
        let mut data = compress_to_vec(&b"first frame\n"[..], CompressionLevel::Fastest);
        data.extend(compress_to_vec(
            &b"second frame\n"[..],
            CompressionLevel::Fastest,
        ));
        let mut out = String::new();
        ZstReader::new(&data[..])
            .unwrap()
            .read_to_string(&mut out)
            .unwrap();
        assert_eq!(out, "first frame\nsecond frame\n");
        assert!(ZstReader::new(&b"not zstd"[..]).is_err());
    }

    #[test]
    fn decoded_names_are_sanitized() {
        assert_eq!(sanitize("session-codex"), "session-codex");
        assert_eq!(sanitize("a/b c"), "a_b_c");
    }
}
