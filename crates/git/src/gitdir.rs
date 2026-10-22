//! Reads of files in git directories (`HEAD`, `commondir`, `gitdir`,
//! `locked`, `config`, the index trailer, the end of `logs/HEAD`). These
//! are git's administrative files, not worktree content, and are read with
//! the standard library; worktree files go only through `SafeRoot`
//! (docs/design/git.md「安全读取」).
//! This module is the only place in the crate that opens files or lists
//! directories by path (besides `testing`).

use std::fs::File;
use std::io;
use std::path::Path;

/// The whole file; None when it doesn't exist.
pub(crate) fn read(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// The directory's entries; None when it doesn't exist.
pub(crate) fn read_dir(path: &Path) -> io::Result<Option<std::fs::ReadDir>> {
    match std::fs::read_dir(path) {
        Ok(entries) => Ok(Some(entries)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// The open file; None when it doesn't exist.
pub(crate) fn open(path: &Path) -> io::Result<Option<File>> {
    match File::open(path) {
        Ok(file) => Ok(Some(file)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}
