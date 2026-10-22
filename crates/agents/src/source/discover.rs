//! 枚举会话目录与文件身份（docs/design/agents.md「数据流与线程」第 2 步）。

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 会话目录之下最多走这么多层（Claude Code 的子 agent 在 `<编码后的路径>/<sessionId>/subagents/workflows/<id>/`）。
const MAX_DEPTH: usize = 8;

/// 文件的身份：设备号、inode、大小、修改时间。追加、截断、替换都会改变它。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct FileId {
    pub(crate) dev: u64,
    pub(crate) ino: u64,
    pub(crate) len: u64,
    pub(crate) mtime_ns: i128,
}

impl FileId {
    pub(crate) fn of(meta: &fs::Metadata) -> FileId {
        FileId {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.len(),
            mtime_ns: i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec()),
        }
    }

    pub(crate) fn stat(path: &Path) -> io::Result<FileId> {
        fs::metadata(path).map(|m| FileId::of(&m))
    }

    /// 同一个文件（没有被替换）。
    pub(crate) fn same_file(&self, other: &FileId) -> bool {
        self.dev == other.dev && self.ino == other.ino
    }
}

/// 一次枚举的结果：目录下的普通文件（不跟随符号链接，只列目录项，不读文件），以及读不了的目录。
#[derive(Default)]
pub(crate) struct Walk {
    pub(crate) files: Vec<PathBuf>,
    pub(crate) unreadable: Vec<Arc<Path>>,
}

/// 递归列出 `dir` 之下名字被 `wanted` 接受的普通文件。`dir` 不存在时结果为空，不算读不了。
pub(crate) fn walk(dir: &Path, wanted: &dyn Fn(&Path) -> bool) -> Walk {
    let mut out = Walk::default();
    walk_into(dir, 0, wanted, &mut out);
    out
}

fn walk_into(dir: &Path, depth: usize, wanted: &dyn Fn(&Path) -> bool, out: &mut Walk) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return;
        }
        Err(e) => {
            lectern_logging::warn!(
                "session directory unreadable; the others are listed";
                path = lectern_logging::Value::path(dir),
                io = lectern_logging::Value::io(&e),
            );
            out.unreadable.push(dir.into());
            return;
        }
    };
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            if depth + 1 < MAX_DEPTH {
                walk_into(&path, depth + 1, wanted, out);
            }
        } else if kind.is_file() && wanted(&path) {
            out.files.push(path);
        }
    }
}
