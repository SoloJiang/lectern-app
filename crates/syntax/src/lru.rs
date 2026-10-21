//! The 8 MiB highlight and text LRU (docs/design/file_reader.md「LRU」,
//! ADR 0030 and ADR 0035's「`file_reader` 的 8 MB 高亮与文本 LRU」).
//!
//! Filled only by reading: spans, the window chain's start tables and texts
//! land here once their content's OID is known; the prefetch and the batch
//! windows put theirs in as `Background`. Every entry counts its `bytes()`
//! plus a fixed overhead, and
//! the sum never exceeds the limit: an insert evicts first the work done in
//! the background (prefetch, batch windows), then what the user opened,
//! oldest first. An entry larger than the limit is not kept. The documents
//! hold their own `Arc`s, so eviction never takes away what is on screen.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use lectern_services::project::BlobId;

use crate::spans::WindowSpans;
use crate::text::Text;
use crate::util::lock;

/// What an entry costs beyond its value: the key, the slot, the map and the
/// order entries, rounded up.
pub(crate) const OVERHEAD: u64 = 192;

/// The window start of a whole file's spans.
pub(crate) const WHOLE: u64 = u64::MAX;

/// What a content's spans are computed with: its OID and the language's
/// name, grammar and query versions (architecture rule 5).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct SpansOf {
    pub oid: BlobId,
    pub lang: &'static str,
    pub grammar: u64,
    pub query: u64,
}

impl SpansOf {
    pub(crate) fn new(oid: BlobId, lang: &crate::Lang) -> SpansOf {
        SpansOf {
            oid,
            lang: lang.static_name(),
            grammar: lang.grammar_version(),
            query: lang.query_version(),
        }
    }

    /// The key of the spans of the chain's window from `start` (or of the
    /// whole file: [`WHOLE`]).
    pub(crate) fn spans(self, start: u64) -> LruKey {
        LruKey::Spans {
            oid: self.oid,
            lang: self.lang,
            grammar: self.grammar,
            query: self.query,
            start,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) enum LruKey {
    /// A window's of the chain (by its start) or the whole file's
    /// ([`WHOLE`]) spans: exact.
    Spans {
        oid: BlobId,
        lang: &'static str,
        grammar: u64,
        query: u64,
        start: u64,
    },
    /// The first screen's window of a prefetched file larger than 1 MiB, by
    /// the line start of the target it was planned for: parsed alone from a
    /// heuristic start, so kept apart from the chain's windows.
    Screen { of: SpansOf, target_line_start: u64 },
    /// The window chain's start table.
    Starts(SpansOf),
    /// A text of at most 1 MiB.
    Text(BlobId),
}

/// The window chain's start table (docs/design/file_reader.md「档位与降级」):
/// the starts found from the file's beginning, ascending, the first 0.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Starts {
    pub starts: Box<[u64]>,
    /// The last window ends at the file's end.
    pub done: bool,
}

#[derive(Clone)]
pub(crate) enum LruValue {
    Spans(Arc<WindowSpans>),
    Starts(Arc<Starts>),
    Text(Arc<Text>),
}

impl LruValue {
    fn bytes(&self) -> u64 {
        match self {
            LruValue::Spans(s) => s.bytes() as u64,
            LruValue::Starts(s) => (size_of::<Starts>() + size_of_val::<[u64]>(&s.starts)) as u64,
            LruValue::Text(t) => t.bytes() as u64,
        }
    }
}

/// Which entries go first: `Background` before `Opened`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub(crate) enum Class {
    /// Prefetched, or computed by batch work.
    Background,
    /// Computed for a document the user opened.
    Opened,
}

struct Slot {
    value: LruValue,
    class: Class,
    stamp: u64,
    cost: u64,
}

#[derive(Default)]
struct Inner {
    map: HashMap<LruKey, Slot>,
    order: BTreeMap<(Class, u64), LruKey>,
    next: u64,
    total: u64,
}

pub(crate) struct Lru {
    limit: u64,
    inner: Mutex<Inner>,
}

impl Lru {
    pub(crate) fn new(limit: u64) -> Lru {
        Lru {
            limit,
            inner: Mutex::new(Inner::default()),
        }
    }

    /// The entry, marked as just used; an entry used by `class` `Opened`
    /// moves to that class.
    pub(crate) fn get(&self, key: &LruKey, class: Class) -> Option<LruValue> {
        let mut inner = lock(&self.inner);
        let inner = &mut *inner;
        let slot = inner.map.get_mut(key)?;
        inner.order.remove(&(slot.class, slot.stamp));
        slot.class = slot.class.max(class);
        slot.stamp = inner.next;
        inner.next += 1;
        inner.order.insert((slot.class, slot.stamp), key.clone());
        Some(slot.value.clone())
    }

    pub(crate) fn spans(&self, key: &LruKey, class: Class) -> Option<Arc<WindowSpans>> {
        match self.get(key, class)? {
            LruValue::Spans(s) => Some(s),
            LruValue::Starts(_) | LruValue::Text(_) => None,
        }
    }

    pub(crate) fn starts(&self, of: SpansOf, class: Class) -> Option<Arc<Starts>> {
        match self.get(&LruKey::Starts(of), class)? {
            LruValue::Starts(s) => Some(s),
            LruValue::Spans(_) | LruValue::Text(_) => None,
        }
    }

    pub(crate) fn text(&self, oid: BlobId, class: Class) -> Option<Arc<Text>> {
        match self.get(&LruKey::Text(oid), class)? {
            LruValue::Text(t) => Some(t),
            LruValue::Spans(_) | LruValue::Starts(_) => None,
        }
    }

    /// Inserts or replaces, then evicts down to the limit. A replaced entry
    /// keeps its class when that is the later one to go (background work
    /// computing again what the user opened does not demote it). The evicted
    /// values are dropped after the lock is released.
    pub(crate) fn insert(&self, key: LruKey, value: LruValue, class: Class) {
        let cost = value.bytes() + OVERHEAD;
        if cost > self.limit {
            return;
        }
        let mut evicted = Vec::new();
        let mut class = class;
        {
            let mut inner = lock(&self.inner);
            let inner = &mut *inner;
            if let Some(old) = inner.map.remove(&key) {
                inner.order.remove(&(old.class, old.stamp));
                inner.total -= old.cost;
                class = class.max(old.class);
                evicted.push(old.value);
            }
            while inner.total + cost > self.limit {
                let Some((_, victim)) = inner.order.pop_first() else {
                    break;
                };
                if let Some(slot) = inner.map.remove(&victim) {
                    inner.total -= slot.cost;
                    evicted.push(slot.value);
                }
            }
            let stamp = inner.next;
            inner.next += 1;
            inner.order.insert((class, stamp), key.clone());
            inner.map.insert(
                key,
                Slot {
                    value,
                    class,
                    stamp,
                    cost,
                },
            );
            inner.total += cost;
        }
        drop(evicted);
    }

    /// The bytes counted, at most the limit.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn total(&self) -> u64 {
        lock(&self.inner).total
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn len(&self) -> usize {
        lock(&self.inner).map.len()
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, key: &LruKey) -> bool {
        lock(&self.inner).map.contains_key(key)
    }

    /// The entry's class, without marking it used.
    #[cfg(test)]
    pub(crate) fn class(&self, key: &LruKey) -> Option<Class> {
        lock(&self.inner).map.get(key).map(|slot| slot.class)
    }

    /// The keys, in no order.
    #[cfg(test)]
    pub(crate) fn keys(&self) -> Vec<LruKey> {
        lock(&self.inner).map.keys().cloned().collect()
    }
}
