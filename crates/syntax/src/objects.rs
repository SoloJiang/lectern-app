//! The registry of object database content someone holds
//! (`Reader::share_object`, docs/design/file_reader.md「定位」, ADR 0042).
//!
//! The registry holds a strong `Arc` of each registration only while its
//! `ObjectShare` lives: dropping the handle unregisters it, so the registry
//! never makes content outlive the review that holds it. No `Weak`: the
//! count and the bytes of an `Arc<[u8]>` are one allocation, which a `Weak`
//! would keep alive too (ADR 0035).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use lectern_services::project::BlobId;
use lectern_services::reader::ObjectShare;

use crate::util::lock;

/// One OID's registrations: sequence number and content.
type Registrations = Vec<(u64, Arc<[u8]>)>;

#[derive(Default)]
pub(crate) struct Shares {
    map: Mutex<HashMap<BlobId, Registrations>>,
    next: AtomicU64,
}

impl Shares {
    /// Registers `bytes` as the content of `oid` until the handle drops.
    /// Never blocks.
    pub(crate) fn register(self: &Arc<Self>, oid: BlobId, bytes: Arc<[u8]>) -> ObjectShare {
        let seq = self.next.fetch_add(1, Ordering::Relaxed);
        lock(&self.map).entry(oid).or_default().push((seq, bytes));
        let registry: Weak<Shares> = Arc::downgrade(self);
        ObjectShare::new(move || {
            if let Some(registry) = registry.upgrade() {
                registry.release(oid, seq);
            }
        })
    }

    /// A registered content of `oid`.
    pub(crate) fn get(&self, oid: &BlobId) -> Option<Arc<[u8]>> {
        lock(&self.map)
            .get(oid)
            .and_then(|all| all.first())
            .map(|(_, bytes)| bytes.clone())
    }

    /// The registrations of `oid` (tests).
    #[cfg(test)]
    pub(crate) fn count(&self, oid: &BlobId) -> usize {
        lock(&self.map).get(oid).map_or(0, Vec::len)
    }

    /// The registrations in all (tests).
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        lock(&self.map).is_empty()
    }

    fn release(&self, oid: BlobId, seq: u64) {
        let released = {
            let mut map = lock(&self.map);
            let Some(all) = map.get_mut(&oid) else {
                return;
            };
            let released = all
                .iter()
                .position(|(s, _)| *s == seq)
                .map(|i| all.remove(i));
            if all.is_empty() {
                map.remove(&oid);
            }
            released
        };
        // The bytes go outside the lock.
        drop(released);
    }
}
