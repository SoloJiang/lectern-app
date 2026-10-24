//! `recent.json` on disk (docs/design/welcome.md「数据流与线程」第 1、2 步). The
//! only file with filesystem calls in this crate; its functions run in
//! `Background` tasks, never on the UI thread.

use std::io::Write as _;
use std::path::Path;

use super::{DecodeError, RecentEntry, decode};

/// The file's name in the data directory.
pub(crate) const FILE: &str = "recent.json";
const TEMP: &str = "recent.json.tmp";

/// What reading the file gave.
#[derive(Debug)]
pub(crate) enum Read {
    /// There is no file: the first run.
    Missing,
    Loaded(Vec<RecentEntry>),
    /// The file is there but gave no list. The next write replaces it.
    Unreadable(Unreadable),
}

#[derive(Debug)]
pub(crate) enum Unreadable {
    /// The read failed.
    Io(std::io::Error),
    /// Not JSON of the expected shape.
    Json,
    /// A `version` this build does not know.
    Version(u32),
}

/// The step of a write that failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum WriteStep {
    CreateDir,
    Create,
    Write,
    Rename,
}

impl WriteStep {
    /// The value of the `step` log field.
    pub(crate) fn name(self) -> &'static str {
        match self {
            WriteStep::CreateDir => "create_dir",
            WriteStep::Create => "create",
            WriteStep::Write => "write",
            WriteStep::Rename => "rename",
        }
    }
}

/// Reads `dir/recent.json`.
pub(crate) fn read(dir: &Path) -> Read {
    match std::fs::read(dir.join(FILE)) {
        Ok(bytes) => match decode(&bytes) {
            Ok(entries) => Read::Loaded(entries),
            Err(DecodeError::Json) => Read::Unreadable(Unreadable::Json),
            Err(DecodeError::Version(version)) => Read::Unreadable(Unreadable::Version(version)),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Read::Missing,
        Err(error) => Read::Unreadable(Unreadable::Io(error)),
    }
}

/// Writes `bytes` as `dir/recent.json`: a temporary file next to it, then a
/// rename. Creates `dir` when it is missing.
pub(crate) fn write(dir: &Path, bytes: &[u8]) -> Result<(), (WriteStep, std::io::Error)> {
    std::fs::create_dir_all(dir).map_err(|e| (WriteStep::CreateDir, e))?;
    let temp = dir.join(TEMP);
    let mut file = std::fs::File::create(&temp).map_err(|e| (WriteStep::Create, e))?;
    file.write_all(bytes).map_err(|e| (WriteStep::Write, e))?;
    drop(file);
    std::fs::rename(&temp, dir.join(FILE)).map_err(|e| (WriteStep::Rename, e))
}

/// File helpers for the tests of this crate, which may not touch the
/// filesystem anywhere else.
#[cfg(test)]
pub(crate) mod testing {
    use std::path::Path;

    use super::FILE;

    /// Puts `bytes` in the data directory as the file.
    pub(crate) fn seed(dir: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(FILE), bytes).unwrap();
    }

    /// The file's content, None when there is none.
    pub(crate) fn contents(dir: &Path) -> Option<Vec<u8>> {
        std::fs::read(dir.join(FILE)).ok()
    }

    /// Makes `dir` a regular file, so that creating the directory fails.
    pub(crate) fn block_dir(dir: &Path) {
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(dir, b"not a directory").unwrap();
    }

    /// Whether the temporary file is left behind.
    pub(crate) fn temp_exists(dir: &Path) -> bool {
        dir.join(super::TEMP).exists()
    }
}
