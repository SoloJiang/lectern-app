//! The manifest parse cache and the sub-project cache of inactive worktrees
//! (docs/design/project.md「数据流与线程」).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use lectern_git::Oid;
use lectern_services::project::{ManifestKind, Package};

use crate::ManifestResult;

/// Parse results by manifest kind and blob OID, shared by every project and
/// worktree; least recently used entries go first once the manifests'
/// bytes pass `limit`. `TooLarge` and `Unreadable` have no content to hash
/// and are never stored here.
pub(crate) struct ParseCache {
    limit: usize,
    bytes: usize,
    tick: u64,
    entries: HashMap<(ManifestKind, Oid), Slot>,
    order: BTreeMap<u64, (ManifestKind, Oid)>,
}

struct Slot {
    result: ManifestResult,
    size: usize,
    tick: u64,
}

/// 2 MB (ADR 0017「内存的具体约束」).
pub(crate) const PARSE_CACHE_BYTES: usize = 2 * 1024 * 1024;

impl ParseCache {
    pub(crate) fn new(limit: usize) -> ParseCache {
        ParseCache {
            limit,
            bytes: 0,
            tick: 0,
            entries: HashMap::new(),
            order: BTreeMap::new(),
        }
    }

    pub(crate) fn get(&mut self, kind: ManifestKind, oid: Oid) -> Option<ManifestResult> {
        self.tick += 1;
        let tick = self.tick;
        let slot = self.entries.get_mut(&(kind, oid))?;
        self.order.remove(&slot.tick);
        slot.tick = tick;
        self.order.insert(tick, (kind, oid));
        Some(slot.result.clone())
    }

    /// `size` is the manifest's length in bytes, the measure of the limit.
    pub(crate) fn insert(
        &mut self,
        kind: ManifestKind,
        oid: Oid,
        result: ManifestResult,
        size: usize,
    ) {
        if size > self.limit {
            return;
        }
        self.tick += 1;
        let tick = self.tick;
        if let Some(old) = self
            .entries
            .insert((kind, oid), Slot { result, size, tick })
        {
            self.order.remove(&old.tick);
            self.bytes -= old.size;
        }
        self.order.insert(tick, (kind, oid));
        self.bytes += size;
        while self.bytes > self.limit {
            let Some((_, key)) = self.order.pop_first() else {
                break;
            };
            if let Some(slot) = self.entries.remove(&key) {
                self.bytes -= slot.size;
            }
        }
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.bytes = 0;
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

/// The sub-projects of one inactive worktree, as `resolve_package` last
/// computed them. No file list is kept.
pub(crate) struct OtherTree {
    pub checksum: Option<Oid>,
    pub packages: Arc<[Package]>,
    /// Every manifest read for it, with its `lstat` at the time.
    pub prints: Vec<(Box<str>, Option<Print>)>,
}

/// A manifest's `lstat` fingerprint; None for a file that was not there.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Print {
    size: u64,
    modified: Option<SystemTime>,
    inode: u64,
}

impl Print {
    /// `lstat`: symbolic links are not followed.
    pub(crate) fn of(path: &Path) -> Option<Print> {
        let meta = std::fs::symlink_metadata(path).ok()?;
        Some(Print {
            size: meta.len(),
            modified: meta.modified().ok(),
            inode: inode(&meta),
        })
    }
}

#[cfg(unix)]
fn inode(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.ino()
}

#[cfg(not(unix))]
fn inode(_: &std::fs::Metadata) -> u64 {
    0
}

pub(crate) type OtherSlot = Arc<Mutex<Option<OtherTree>>>;

/// At most `OTHER_TREES` worktrees, least recently used first out. One
/// slot per worktree git directory; a caller holds the slot's lock while it
/// computes, so concurrent requests for one worktree compute once.
#[derive(Default)]
pub(crate) struct OtherTrees {
    slots: Vec<(PathBuf, OtherSlot)>,
}

pub(crate) const OTHER_TREES: usize = 16;

impl OtherTrees {
    pub(crate) fn slot(&mut self, git_dir: &Path) -> OtherSlot {
        if let Some(i) = self.slots.iter().position(|(dir, _)| dir == git_dir) {
            let entry = self.slots.remove(i);
            let slot = entry.1.clone();
            self.slots.push(entry);
            return slot;
        }
        if self.slots.len() == OTHER_TREES {
            self.slots.remove(0);
        }
        let slot = OtherSlot::default();
        self.slots.push((git_dir.to_path_buf(), slot.clone()));
        slot
    }

    /// Drops the slots of one repository's worktrees.
    pub(crate) fn forget(&mut self, common_dir: &Path) {
        self.slots.retain(|(dir, _)| !dir.starts_with(common_dir));
    }

    pub(crate) fn clear(&mut self) {
        self.slots.clear();
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use lectern_git::{HashKind, Oid};
    use lectern_services::project::ManifestKind;

    use super::{OTHER_TREES, OtherTrees, ParseCache};

    fn oid(n: u8) -> Oid {
        Oid::for_blob(HashKind::Sha1, &[n])
    }

    #[test]
    fn open_parse_cache_evicts_the_least_recently_used_by_bytes() {
        let mut cache = ParseCache::new(10);
        let parsed = Ok(Arc::default());
        cache.insert(ManifestKind::PackageJson, oid(1), parsed.clone(), 4);
        cache.insert(ManifestKind::PackageJson, oid(2), parsed.clone(), 4);
        // Touch 1, so 2 is the oldest.
        assert!(cache.get(ManifestKind::PackageJson, oid(1)).is_some());
        cache.insert(ManifestKind::PackageJson, oid(3), parsed.clone(), 4);
        assert_eq!(cache.len(), 2);
        assert!(cache.get(ManifestKind::PackageJson, oid(2)).is_none());
        assert!(cache.get(ManifestKind::PackageJson, oid(1)).is_some());
        // The kind is part of the key; larger than the limit is not kept.
        assert!(cache.get(ManifestKind::CargoToml, oid(1)).is_none());
        cache.insert(ManifestKind::CargoToml, oid(4), parsed, 11);
        assert!(cache.get(ManifestKind::CargoToml, oid(4)).is_none());
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn resolve_package_cache_keeps_the_last_sixteen_worktrees() {
        let mut trees = OtherTrees::default();
        let first = trees.slot(Path::new("/r/.git/worktrees/0"));
        for i in 1..=OTHER_TREES {
            trees.slot(Path::new(&format!("/r/.git/worktrees/{i}")));
        }
        assert_eq!(trees.len(), OTHER_TREES);
        // 0 was dropped: asking again gives a new, empty slot.
        let again = trees.slot(Path::new("/r/.git/worktrees/0"));
        assert!(!Arc::ptr_eq(&first, &again));
        trees.forget(Path::new("/r/.git"));
        assert_eq!(trees.len(), 0);
    }
}
