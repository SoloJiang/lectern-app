//! The directory sweep: the first open of a run deletes project partitions
//! unused for 90 days, then the oldest ones while all partition files exceed
//! `Budgets::total` ("淘汰" in the design).

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use lectern_logging::{Value, info, warn};

use crate::stats::PartitionKind;

/// Project partitions whose files are older than this are deleted.
pub(crate) const MAX_AGE: Duration = Duration::from_secs(90 * 24 * 60 * 60);

/// The files of a partition: `<stem>.sqlite` and its `-wal` and `-shm`.
pub(crate) const SUFFIXES: [&str; 3] = ["", "-wal", "-shm"];

/// A project partition found on disk.
struct Found {
    /// The `.sqlite` file.
    file: PathBuf,
    /// The latest modification time of its files.
    modified: SystemTime,
    bytes: u64,
}

/// Sweeps `root`. `keep` are the `.sqlite` files of open partitions; the app
/// partition's files count toward `total` but are never deleted. Failures to
/// list or delete are logged and skipped: the sweep only saves disk space.
pub(crate) fn sweep(root: &Path, now: SystemTime, total: u64, keep: &[PathBuf]) {
    let mut bytes: u64 = SUFFIXES
        .iter()
        .filter_map(|suffix| fs::metadata(with_suffix(&root.join("app.sqlite"), suffix)).ok())
        .map(|m| m.len())
        .sum();
    let mut found = Vec::new();
    for kind in [PartitionKind::Repo, PartitionKind::Folder] {
        list(&root.join(kind.dir()), &mut found);
    }
    bytes += found.iter().map(|p| p.bytes).sum::<u64>();
    found.retain(|p| !keep.contains(&p.file));
    // Oldest first: the expired ones, then as many as the total needs.
    found.sort_by_key(|p| p.modified);
    let expired = now.checked_sub(MAX_AGE);
    let (mut deleted, mut freed) = (0u32, 0u64);
    for p in found {
        let old = expired.is_some_and(|expired| p.modified < expired);
        if !old && bytes <= total {
            break;
        }
        match remove(&p.file) {
            Ok(()) => {
                bytes = bytes.saturating_sub(p.bytes);
                deleted += 1;
                freed += p.bytes;
            }
            Err(e) => {
                warn!("cannot delete a partition"; path = Value::path(&p.file), io = Value::io(&e));
            }
        }
    }
    if deleted > 0 {
        info!("partitions swept"; deleted = deleted, bytes = freed);
    }
}

/// The partitions in one of `repo/` and `folder/`. Other files are ignored.
fn list(dir: &Path, out: &mut Vec<Found>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return,
        Err(e) => {
            warn!("cannot list partitions"; path = Value::path(dir), io = Value::io(&e));
            return;
        }
    };
    let mut by_file: HashMap<PathBuf, (SystemTime, u64)> = HashMap::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let stem = name
            .strip_suffix("-wal")
            .or_else(|| name.strip_suffix("-shm"))
            .unwrap_or(name);
        if !stem.ends_with(".sqlite") {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let item = by_file
            .entry(dir.join(stem))
            .or_insert((SystemTime::UNIX_EPOCH, 0));
        item.0 = item.0.max(modified);
        item.1 += meta.len();
    }
    out.extend(by_file.into_iter().map(|(file, (modified, bytes))| Found {
        file,
        modified,
        bytes,
    }));
}

/// `file` with a suffix appended to its name.
pub(crate) fn with_suffix(file: &Path, suffix: &str) -> PathBuf {
    let mut name = file.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Deletes the three files of a partition, the `.sqlite` first.
pub(crate) fn remove(file: &Path) -> io::Result<()> {
    for suffix in SUFFIXES {
        match fs::remove_file(with_suffix(file, suffix)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
