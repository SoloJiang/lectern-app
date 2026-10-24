//! The recent projects list (docs/design/welcome.md「最近项目」): the entries,
//! the pure operations on them and the file format. The file itself is read
//! and written in [`store`], from background tasks only.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use lectern_services::project::{ProjectKey, ProjectSnapshot};

pub(crate) mod store;

/// The `version` this build reads and writes.
pub(crate) const VERSION: u32 = 1;
/// The most entries the list keeps.
pub(crate) const MAX_ENTRIES: usize = 10;

/// One recent project.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RecentEntry {
    /// What the next open hands to `WorkspaceCommand::OpenProject`: the
    /// current worktree's root plus `opened_at` (so a project opened in a
    /// subdirectory or a linked worktree comes back to the same place); the
    /// common directory of a `ProjectKey::Git` without a current worktree
    /// (a bare repository). Canonical.
    pub path: PathBuf,
    /// `ProjectSnapshot::key`. The discovery filter and the session counts on
    /// the row compare by it, so sessions in the repository's other
    /// worktrees count on this entry.
    pub key: ProjectKey,
    /// `ProjectSnapshot::name`.
    pub name: Arc<str>,
    /// UTC milliseconds.
    pub last_opened: i64,
    /// The last open failed (`NotFound` / `NotADirectory`); memory only, set
    /// back to false when an open succeeds.
    #[serde(skip)]
    pub missing: bool,
    /// Sessions and running sessions in this project, from the latest
    /// [`discover::build`](crate::discover::build); memory only, 0 while
    /// discovery is not active.
    #[serde(skip)]
    pub sessions: u32,
    #[serde(skip)]
    pub running: u32,
}

/// The `recent.json` content.
#[derive(Serialize, Deserialize)]
struct RecentFile {
    version: u32,
    entries: Vec<RecentEntry>,
}

/// Only the version, read first so that a file of another version is not
/// held to this version's entries.
#[derive(Deserialize)]
struct Header {
    version: u32,
}

/// Why a file did not give a list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DecodeError {
    /// Not JSON of the expected shape.
    Json,
    /// A `version` this build does not know.
    Version(u32),
}

/// Puts `entry` first. An older entry with the same path goes away; the list
/// keeps [`MAX_ENTRIES`], dropping the oldest. The entry's `missing` is
/// false: it was just opened.
pub(crate) fn push_front(entries: &mut Vec<RecentEntry>, mut entry: RecentEntry) {
    entry.missing = false;
    entries.retain(|e| e.path != entry.path);
    entries.insert(0, entry);
    entries.truncate(MAX_ENTRIES);
}

/// Removes the entry at `path`; false when there is none.
pub(crate) fn remove(entries: &mut Vec<RecentEntry>, path: &Path) -> bool {
    let before = entries.len();
    entries.retain(|e| e.path != path);
    entries.len() != before
}

/// Reads a file's content. The list keeps its order, drops later entries with
/// a path already seen, and stops at [`MAX_ENTRIES`]. Unknown fields are
/// ignored.
pub(crate) fn decode(bytes: &[u8]) -> Result<Vec<RecentEntry>, DecodeError> {
    let header: Header = serde_json::from_slice(bytes).map_err(|_| DecodeError::Json)?;
    if header.version != VERSION {
        return Err(DecodeError::Version(header.version));
    }
    let file: RecentFile = serde_json::from_slice(bytes).map_err(|_| DecodeError::Json)?;
    let mut entries: Vec<RecentEntry> = Vec::with_capacity(file.entries.len().min(MAX_ENTRIES));
    for entry in file.entries {
        if entries.len() == MAX_ENTRIES {
            break;
        }
        if entries.iter().all(|e| e.path != entry.path) {
            entries.push(entry);
        }
    }
    Ok(entries)
}

/// The file content for `entries`, and how many entries were left out
/// because a path in them is not UTF-8 (the JSON form cannot hold it; the
/// entry stays in memory).
pub(crate) fn encode(entries: &[RecentEntry]) -> (Vec<u8>, usize) {
    let writable: Vec<RecentEntry> = entries.iter().filter(|e| encodable(e)).cloned().collect();
    let skipped = entries.len() - writable.len();
    let file = RecentFile {
        version: VERSION,
        entries: writable,
    };
    // A derived `Serialize` over strings, numbers and UTF-8 paths cannot fail.
    let bytes = serde_json::to_vec_pretty(&file).unwrap_or_default();
    (bytes, skipped)
}

fn encodable(entry: &RecentEntry) -> bool {
    let key_ok = match &entry.key {
        ProjectKey::Git { common_dir } => common_dir.to_str().is_some(),
        ProjectKey::Folder { root } => root.to_str().is_some(),
    };
    key_ok && entry.path.to_str().is_some()
}

/// The entry for a project that was just opened. `path` is the current
/// worktree's root plus the opened subdirectory; without a current worktree
/// (a bare repository) it is the common directory; for a folder project
/// without a scan it is the folder.
pub(crate) fn entry_from_snapshot(snapshot: &ProjectSnapshot, now: i64) -> RecentEntry {
    let path = match (&snapshot.active, &snapshot.key) {
        (Some(active), _) if active.opened_at.is_empty() => active.root.to_path_buf(),
        (Some(active), _) => active.root.join(&*active.opened_at),
        (None, ProjectKey::Git { common_dir }) => common_dir.to_path_buf(),
        (None, ProjectKey::Folder { root }) => root.to_path_buf(),
    };
    RecentEntry {
        path,
        key: snapshot.key.clone(),
        name: snapshot.name.clone(),
        last_opened: now,
        missing: false,
        sessions: 0,
        running: 0,
    }
}
