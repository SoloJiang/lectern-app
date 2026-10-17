//! Plain folder projects: a bounded walk instead of the index, and manifest
//! reads through `SafeRoot` (docs/design/project.md「文件列表」).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use lectern_platform::{Read, SafeError, SafeRoot};
use lectern_services::project::{FileKind, FileList, ScanError};

/// The most entries a folder walk collects; above it the list is truncated.
pub(crate) const FOLDER_LIMIT: usize = 20_000;

/// Directories never walked into, besides those starting with `.`.
const SKIPPED_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "build",
    "dist",
    "out",
    "vendor",
    "venv",
    "__pycache__",
];

/// Walks `root` without following symbolic links, in byte order of names,
/// collecting at most `limit` files. Unreadable subdirectories are left
/// out; an unreadable root is an error.
pub(crate) fn walk(root: &Path, limit: usize, cancel: &AtomicBool) -> Result<FileList, ScanError> {
    let mut files = FileList::builder();
    let mut stack: Vec<(PathBuf, String)> = Vec::new();
    let top = read_sorted(root).map_err(|e| scan_error(&e))?;
    skip(&mut files, push_entries(&mut stack, root, "", top));
    let mut count = 0usize;
    while let Some((path, rel)) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let kind = meta.file_type();
        if kind.is_dir() {
            let name = rel.rsplit_once('/').map_or(rel.as_str(), |(_, n)| n);
            if name.starts_with('.') || SKIPPED_DIRS.contains(&name) {
                continue;
            }
            if let Ok(entries) = read_sorted(&path) {
                skip(&mut files, push_entries(&mut stack, &path, &rel, entries));
            }
            continue;
        }
        let file_kind = if kind.is_symlink() {
            FileKind::Symlink
        } else if kind.is_file() {
            if is_executable(&meta) {
                FileKind::Executable
            } else {
                FileKind::File
            }
        } else {
            // FIFOs, sockets, devices.
            continue;
        };
        if count == limit {
            files.set_truncated();
            break;
        }
        count += 1;
        files.push(&rel, file_kind);
    }
    Ok(files.build())
}

fn skip(files: &mut lectern_services::project::FileListBuilder, n: usize) {
    for _ in 0..n {
        files.skip();
    }
}

/// The names in one directory, sorted by bytes.
fn read_sorted(dir: &Path) -> io::Result<Vec<std::ffi::OsString>> {
    let mut names: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.file_name()))
        .collect();
    names.sort_by(|a, b| a.as_encoded_bytes().cmp(b.as_encoded_bytes()));
    Ok(names)
}

/// Pushes in reverse so the stack pops them in order. Names that are not
/// UTF-8 cannot be a `RelPath`: they are left out and counted.
fn push_entries(
    stack: &mut Vec<(PathBuf, String)>,
    dir: &Path,
    rel: &str,
    names: Vec<std::ffi::OsString>,
) -> usize {
    let mut skipped = 0;
    for name in names.into_iter().rev() {
        let Some(text) = name.to_str() else {
            skipped += 1;
            continue;
        };
        let child = if rel.is_empty() {
            text.to_owned()
        } else {
            format!("{rel}/{text}")
        };
        stack.push((dir.join(&name), child));
    }
    skipped
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_: &std::fs::Metadata) -> bool {
    false
}

pub(crate) fn scan_error(err: &io::Error) -> ScanError {
    match err.kind() {
        io::ErrorKind::NotFound => ScanError::RootMissing,
        _ => ScanError::Io(err.to_string().into()),
    }
}

/// Reads one manifest of a folder project through `SafeRoot`: no symbolic
/// link followed, nothing outside the root. Same contract as
/// `TreeAccess::read_file`, with the error already classified.
pub(crate) fn read_file(
    root: &SafeRoot,
    rel: &str,
    limit: u64,
) -> Result<Option<Vec<u8>>, ReadError> {
    let Some(path) = lectern_platform::RelPath::new(rel.as_bytes()) else {
        return Ok(None);
    };
    match root.read(path, limit) {
        Ok(Read::File { bytes, .. }) => Ok(Some(bytes)),
        Ok(Read::TooLarge { .. }) => Err(ReadError::TooLarge),
        Ok(Read::Symlink { .. } | Read::Unsupported { .. }) => Err(ReadError::Unreadable),
        Err(SafeError::NotFound) => Ok(None),
        Err(SafeError::PathThroughSymlink | SafeError::Io(_)) => Err(ReadError::Unreadable),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ReadError {
    TooLarge,
    Unreadable,
}
