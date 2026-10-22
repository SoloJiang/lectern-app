//! The memory LRU of replayed intermediate versions
//! (docs/design/review.md「改动链」「归因的缓存」): a narrative step's rows
//! need the versions before and after it, which are not in the object
//! database. Keyed by the version's blob OID, so a version is replayed
//! once; at most 8 MiB of content.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use lectern_services::project::BlobId;

/// The byte limit of [`Versions`].
pub(crate) const MAX_BYTES: usize = 8 << 20;

#[derive(Debug)]
pub(crate) struct Versions {
    max: usize,
    bytes: usize,
    map: HashMap<BlobId, Arc<[u8]>>,
    /// The keys of `map`, each once, least recently used first.
    order: VecDeque<BlobId>,
}

impl Default for Versions {
    fn default() -> Versions {
        Versions::new(MAX_BYTES)
    }
}

impl Versions {
    pub(crate) fn new(max: usize) -> Versions {
        Versions {
            max,
            bytes: 0,
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    pub(crate) fn get(&mut self, oid: &BlobId) -> Option<Arc<[u8]>> {
        let found = self.map.get(oid).cloned()?;
        self.touch(*oid);
        Some(found)
    }

    /// Keeps `content` under `oid`; a version larger than the limit is not
    /// kept. Evicts the least recently used until it fits.
    pub(crate) fn put(&mut self, oid: BlobId, content: Arc<[u8]>) {
        if content.len() > self.max {
            return;
        }
        if let Some(old) = self.map.insert(oid, Arc::clone(&content)) {
            self.bytes -= old.len();
        }
        self.bytes += content.len();
        self.touch(oid);
        // `oid` is last in `order` and fits alone, so it is never evicted.
        while self.bytes > self.max {
            let Some(victim) = self.order.pop_front() else {
                break;
            };
            if let Some(old) = self.map.remove(&victim) {
                self.bytes -= old.len();
            }
        }
    }

    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    pub(crate) fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
        self.bytes = 0;
    }

    fn touch(&mut self, oid: BlobId) {
        self.order.retain(|o| *o != oid);
        self.order.push_back(oid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(n: u8) -> BlobId {
        BlobId::Sha1([n; 20])
    }

    #[test]
    fn rows_version_lru_stays_within_its_bytes() {
        let mut lru = Versions::new(10);
        lru.put(oid(1), Arc::from(&b"aaaa"[..]));
        lru.put(oid(2), Arc::from(&b"bbbb"[..]));
        assert!(lru.get(&oid(1)).is_some());
        lru.put(oid(3), Arc::from(&b"cccc"[..]));
        // 2 was the least recently used.
        assert!(lru.get(&oid(2)).is_none());
        assert!(lru.get(&oid(1)).is_some() && lru.get(&oid(3)).is_some());
        assert!(lru.bytes() <= 10);
        lru.put(oid(4), Arc::from(&b"0123456789x"[..]));
        assert!(lru.get(&oid(4)).is_none());
        assert_eq!(lru.bytes(), 8);
    }
}
