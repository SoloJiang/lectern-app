//! 「规范内容 OID 的批量核对」: the canonical OIDs of many worktree files,
//! one `canonical_oids` call (at most one parse of the index) per batch of
//! at most [`CANONICAL_BATCH_ITEMS`] items and [`CANONICAL_BATCH_BYTES`]
//! bytes. The overlay's re-read and the batch tiers' batches use it; a
//! single file on an interactive path still calls `canonical_oid`.

use lectern_services::project::{BlobId, RelPath};

use crate::git_access::{GitAccess, Tree};
use crate::util::log;

/// At most this many items in one `canonical_oids` call.
pub(crate) const CANONICAL_BATCH_ITEMS: usize = 256;
/// At most this many bytes in one call; an item larger than this is a
/// batch of its own.
pub(crate) const CANONICAL_BATCH_BYTES: usize = 2 << 20;

/// The canonical form of one item's bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Canonical {
    /// The blob OID `git add` would give the bytes at that path.
    Oid(BlobId),
    /// A clean or process filter applies: no canonical OID.
    Filtered,
    /// The conversion failed for this item: the caller treats the file as
    /// unreadable.
    Failed,
}

/// Items waiting for their batch's `canonical_oids`: each with the
/// caller's tag `T`, its path and its bytes `B`. The bytes stay in memory
/// until the batch is checked (at most [`CANONICAL_BATCH_BYTES`], or the one
/// larger item).
pub(crate) struct Pending<T, B> {
    items: Vec<(T, RelPath, B)>,
    bytes: usize,
}

impl<T, B: AsRef<[u8]>> Pending<T, B> {
    pub(crate) fn new() -> Self {
        Pending {
            items: Vec::new(),
            bytes: 0,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Whether an item of `len` bytes would take the batch over its limits:
    /// the batch is checked first ([`flush`](Self::flush)). An empty batch
    /// takes any item.
    pub(crate) fn full_for(&self, len: usize) -> bool {
        !self.items.is_empty()
            && (self.items.len() >= CANONICAL_BATCH_ITEMS
                || self.bytes + len > CANONICAL_BATCH_BYTES)
    }

    /// Adds an item; the caller has flushed when [`full_for`](Self::full_for)
    /// said so.
    pub(crate) fn push(&mut self, tag: T, path: RelPath, bytes: B) {
        self.bytes += bytes.as_ref().len();
        self.items.push((tag, path, bytes));
    }

    /// Checks the batch in one call and empties it: each tag with its
    /// result, in the order pushed.
    pub(crate) fn flush(&mut self, git: &dyn GitAccess, tree: &Tree) -> Vec<(T, Canonical)> {
        let items = std::mem::take(&mut self.items);
        self.bytes = 0;
        if items.is_empty() {
            return Vec::new();
        }
        let refs: Vec<(&str, &[u8])> = items
            .iter()
            .map(|(_, path, bytes)| (&**path, bytes.as_ref()))
            .collect();
        let results = check(git, tree, &refs);
        items
            .into_iter()
            .zip(results)
            .map(|((tag, _, _), c)| (tag, c))
            .collect()
    }
}

/// One batch's canonical OIDs, in order. The batch fails as a whole when
/// one item does (`Worktree::canonical_oids`): then each item is tried on
/// its own, so only the one that fails is `Failed`. A batch of one is not
/// tried again.
fn check(git: &dyn GitAccess, tree: &Tree, items: &[(&str, &[u8])]) -> Vec<Canonical> {
    let of = |oid: Option<BlobId>| oid.map_or(Canonical::Filtered, Canonical::Oid);
    let error = match git.canonical_oids(tree, items) {
        Ok(oids) if oids.len() == items.len() => return oids.into_iter().map(of).collect(),
        Ok(oids) => format!("{} OIDs for {} paths", oids.len(), items.len()),
        Err(error) => error.to_string(),
    };
    if let [(path, _)] = items {
        log(format_args!("converting {path} failed: {error}"));
        return vec![Canonical::Failed];
    }
    log(format_args!(
        "converting a batch of {} files failed ({error}): one by one",
        items.len()
    ));
    items
        .iter()
        .map(|(path, bytes)| match git.canonical_oid(tree, path, bytes) {
            Ok(oid) => of(oid),
            Err(error) => {
                log(format_args!("converting {path} failed: {error}"));
                Canonical::Failed
            }
        })
        .collect()
}
