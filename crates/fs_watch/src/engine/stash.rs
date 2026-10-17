//! Paths held while the filter is stale (docs/design/fs_watch.md「路径分类」).

use std::collections::BTreeMap;

use crate::backend::Flags;
use crate::path::parent;

/// At most this many entries; beyond that entries are merged into their
/// directories, deepest first.
pub(crate) const LIMIT: usize = 4096;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Held {
    /// The event's flags (`DIR` for merged directories).
    pub(crate) flags: Flags,
    /// Held while no filter had loaded yet: the reload triggers are checked
    /// when the first filter is in.
    pub(crate) fresh: bool,
    /// A directory made of merged entries: stands for everything below it.
    pub(crate) merged: bool,
}

#[derive(Default)]
pub(crate) struct Stash {
    entries: BTreeMap<Vec<u8>, Held>,
    /// Merging reached the worktree root and still exceeded the limit.
    pub(crate) rescan: bool,
}

impl Stash {
    pub(crate) fn insert(&mut self, path: &[u8], flags: Flags, fresh: bool) {
        if self.rescan || self.covered(path) {
            return;
        }
        let held = self.entries.entry(path.to_vec()).or_insert(Held {
            flags: Flags::default(),
            fresh,
            merged: false,
        });
        held.flags |= flags;
        held.fresh |= fresh;
        if self.entries.len() > LIMIT {
            self.merge();
        }
    }

    /// Whether a merged directory above `path` already holds it.
    fn covered(&self, path: &[u8]) -> bool {
        let mut end = path.len();
        while let Some(dir) = parent(&path[..end]) {
            if self.entries.get(dir).is_some_and(|held| held.merged) {
                return true;
            }
            end = dir.len();
        }
        false
    }

    /// Merges entries into their directories, deepest first and the largest
    /// group first, until the stash fits; entries at the top level that still
    /// do not fit turn the stash into a rescan.
    fn merge(&mut self) {
        let depth = |path: &[u8]| path.iter().filter(|&&b| b == b'/').count();
        while self.entries.len() > LIMIT {
            let deepest = self
                .entries
                .keys()
                .map(|path| depth(path))
                .max()
                .unwrap_or(0);
            if deepest == 0 {
                self.entries.clear();
                self.rescan = true;
                return;
            }
            let mut groups: BTreeMap<Vec<u8>, Vec<Vec<u8>>> = BTreeMap::new();
            for path in self.entries.keys().filter(|path| depth(path) == deepest) {
                let dir = parent(path).unwrap_or_default().to_vec();
                groups.entry(dir).or_default().push(path.clone());
            }
            let mut groups: Vec<(Vec<u8>, Vec<Vec<u8>>)> = groups.into_iter().collect();
            groups.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
            // Groups of one only shrink the stash once they are a level up,
            // so they are merged only when the larger groups were not enough.
            let mut singles = Vec::new();
            for (dir, paths) in groups {
                if self.entries.len() <= LIMIT {
                    return;
                }
                if paths.len() == 1 {
                    singles.push((dir, paths));
                } else {
                    self.merge_into(dir, paths);
                }
            }
            if self.entries.len() > LIMIT {
                for (dir, paths) in singles {
                    self.merge_into(dir, paths);
                }
            }
        }
    }

    fn merge_into(&mut self, dir: Vec<u8>, paths: Vec<Vec<u8>>) {
        let mut fresh = false;
        for path in paths {
            if let Some(held) = self.entries.remove(&path) {
                fresh |= held.fresh;
            }
        }
        let held = self.entries.entry(dir).or_insert(Held {
            flags: Flags::DIR,
            fresh,
            merged: true,
        });
        held.flags |= Flags::DIR;
        held.fresh |= fresh;
        held.merged = true;
    }

    pub(crate) fn take(&mut self) -> (Vec<(Vec<u8>, Held)>, bool) {
        let stash = std::mem::take(self);
        (stash.entries.into_iter().collect(), stash.rescan)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_stale_stash_merges_deepest_directories_first() {
        let mut stash = Stash::default();
        stash.insert(b"src/main.rs", Flags::FILE, false);
        for i in 0..5000 {
            stash.insert(
                format!("node_modules/p{i}/index.js").as_bytes(),
                Flags::FILE,
                false,
            );
        }
        assert!(stash.len() <= LIMIT);
        assert!(!stash.rescan);
        let (entries, rescan) = stash.take();
        assert!(!rescan);
        let paths: Vec<String> = entries
            .iter()
            .map(|(path, held)| format!("{} merged={}", String::from_utf8_lossy(path), held.merged))
            .collect();
        assert_eq!(
            paths,
            ["node_modules merged=true", "src/main.rs merged=false"]
        );
    }

    #[test]
    fn filter_stale_stash_beyond_the_root_becomes_a_rescan() {
        let mut stash = Stash::default();
        for i in 0..=LIMIT {
            stash.insert(format!("f{i}").as_bytes(), Flags::FILE, false);
        }
        assert!(stash.rescan);
    }
}
