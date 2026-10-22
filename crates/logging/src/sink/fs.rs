//! Every file operation of the sink, so that tests can see which thread
//! runs it (docs/design/logging.md「测试」「UI 线程上没有 IO」).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::hooks::file_op;

/// Creates `dir` and its parents; new directories are 0700.
pub(crate) fn create_dir_all(dir: &Path) -> io::Result<()> {
    file_op("create_dir_all");
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

/// Creates a new directory (export); fails when it exists.
pub(crate) fn create_dir(dir: &Path) -> io::Result<()> {
    file_op("create_dir");
    fs::create_dir(dir)
}

/// Opens `path` for appending, creating it as 0600.
pub(crate) fn open_append(path: &Path) -> io::Result<File> {
    file_op("open");
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

pub(crate) fn write(w: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    file_op("write");
    w.write_all(bytes)
}

pub(crate) fn flush(w: &mut impl Write) -> io::Result<()> {
    file_op("flush");
    w.flush()
}

pub(crate) fn sync(file: &File) -> io::Result<()> {
    file_op("sync");
    file.sync_data()
}

/// A regular file in a directory.
pub(crate) struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub modified: Option<SystemTime>,
}

/// The regular files in `dir`.
pub(crate) fn list(dir: &Path) -> io::Result<Vec<Entry>> {
    file_op("read_dir");
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        file_op("metadata");
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        if let Some(name) = entry.file_name().to_str() {
            out.push(Entry {
                name: name.to_owned(),
                path: entry.path(),
                bytes: meta.len(),
                modified: meta.modified().ok(),
            });
        }
    }
    Ok(out)
}

pub(crate) fn remove_file(path: &Path) -> io::Result<()> {
    file_op("remove");
    fs::remove_file(path)
}

/// The first and last `n` bytes of a file (the same bytes when it is short).
pub(crate) fn head_and_tail(path: &Path, n: u64) -> io::Result<(Vec<u8>, Vec<u8>)> {
    file_op("read");
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut head = Vec::new();
    (&mut file).take(n).read_to_end(&mut head)?;
    let mut tail = Vec::new();
    file.seek(SeekFrom::Start(len.saturating_sub(n)))?;
    file.take(n).read_to_end(&mut tail)?;
    Ok((head, tail))
}

pub(crate) fn read_to_string(path: &Path) -> io::Result<String> {
    file_op("read");
    fs::read_to_string(path)
}

/// Writes `bytes` to a temporary file next to `path`, then renames it.
pub(crate) fn write_replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    file_op("write_replace");
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = open_truncate(&tmp)?;
        file.write_all(bytes)?;
        file.sync_data()?;
    }
    fs::rename(&tmp, path)
}

fn open_truncate(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

/// Writes a new file (the export's manifest).
pub(crate) fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    file_op("write_new");
    open_truncate(path)?.write_all(bytes)
}

pub(crate) fn copy(from: &Path, to: &Path) -> io::Result<u64> {
    file_op("copy");
    fs::copy(from, to)
}

pub(crate) fn exists(path: &Path) -> bool {
    file_op("metadata");
    fs::symlink_metadata(path).is_ok()
}
