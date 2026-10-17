//! The per-worktree cache of step 6 of the classification: whether a
//! directory, together with every directory above it, lets its contents
//! through.

use std::collections::HashMap;

/// At most this many directories per worktree state.
pub(crate) const CAPACITY: usize = 1024;

/// A two-generation cache: lookups hit the current generation, then the
/// previous one (promoting the entry); when the current one is full it
/// becomes the previous one. Recently used entries survive, at most
/// `CAPACITY` entries are kept, and every operation is O(1).
#[derive(Default)]
pub(crate) struct DirCache {
    current: HashMap<Box<[u8]>, bool>,
    previous: HashMap<Box<[u8]>, bool>,
}

impl DirCache {
    /// Whether paths under `dir` pass step 6; None when not cached.
    pub(crate) fn get(&mut self, dir: &[u8]) -> Option<bool> {
        if let Some(&passes) = self.current.get(dir) {
            return Some(passes);
        }
        let passes = self.previous.remove(dir)?;
        self.insert(dir, passes);
        Some(passes)
    }

    pub(crate) fn insert(&mut self, dir: &[u8], passes: bool) {
        if self.current.len() >= CAPACITY / 2 {
            self.previous = std::mem::take(&mut self.current);
        }
        self.current.insert(dir.into(), passes);
    }

    pub(crate) fn clear(&mut self) {
        self.current.clear();
        self.previous.clear();
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.current.len() + self.previous.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_cache_keeps_recent_entries_within_capacity() {
        let mut cache = DirCache::default();
        cache.insert(b"hot", true);
        for i in 0..5000 {
            cache.insert(format!("d{i}").as_bytes(), false);
            // `hot` is used all the time and stays.
            assert_eq!(cache.get(b"hot"), Some(true));
            assert!(cache.len() <= CAPACITY);
        }
        assert_eq!(cache.get(b"d0"), None);
        assert_eq!(cache.get(b"d4999"), Some(false));
    }
}
