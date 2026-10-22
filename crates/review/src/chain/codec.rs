//! `review.attribution` (docs/design/review.md「归因的缓存」,
//! docs/design/cache.md「命名空间」): a file's chain in the repository
//! partition, keyed by the hash algorithm, the base side's OID, the worktree
//! side's OID and the turn-history digest.
//!
//! The value is binary, LEB128 integers throughout:
//!
//! ```text
//! steps    n, then per step: outcome (u8), version (OID)
//! groups   n, then per group: its steps (n, indices), has diff (u8),
//!          and if so: added, removed, survival (u8), hunks (n, then six
//!          integers each: old start/end, new start/end, old/new byte)
//! detail   0 lines: aligned (u8), worktree runs (n, then tag (u8),
//!                   value unless none, length), base runs (same)
//!          1 file-only: reason (u8), matched (0, or 1 and the step)
//! OID      0 none, 1 and 20 bytes, 2 and 32 bytes
//! ```
//!
//! Line sources are run-length encoded: consecutive base lines, or lines of
//! one step, are one run. Decoding checks every index against the lengths
//! it read; anything off, trailing bytes included, is a miss.

use lectern_cache::{Compression, Entry, Key, Namespace, Partition};
use lectern_git::HashKind;
use lectern_services::changes::{DiffHunk, SideIdentity};
use lectern_services::project::BlobId;
use lectern_services::review::Survival;

use super::{
    Chain, Detail, Fate, GroupDiff, GroupResult, Limit, Lines, MAX_VERSION_BYTES, Outcome, Source,
    StepResult,
};

/// More lines than a version within the limit can have: a run length
/// summing above it is corrupt, and is not allocated.
const MAX_LINES: u64 = MAX_VERSION_BYTES + 1;

/// A chain as the cache stores it.
pub(crate) struct CachedChain(pub Chain);

impl Entry for CachedChain {
    /// The version is the chain's algorithm and this encoding.
    const NAMESPACE: Namespace = Namespace {
        name: "review.attribution",
        version: 1,
        compression: Compression::Zstd,
    };

    fn encode(&self, out: &mut Vec<u8>) {
        encode(&self.0, out);
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        decode(bytes).map(CachedChain)
    }
}

/// The key of a file's chain. None when the worktree side has no blob
/// identity (`Blob` or `NotRetained`): such chains are not cached. A
/// worktree side that is absent (a deletion) is empty, as an absent base.
pub(crate) fn key(
    hash: HashKind,
    base: Option<BlobId>,
    worktree: Option<&SideIdentity>,
    digest: &[u8; 32],
) -> Option<Key> {
    let worktree: &[u8] = match worktree {
        None => &[],
        Some(SideIdentity::Blob(oid) | SideIdentity::NotRetained(oid)) => oid.as_bytes(),
        Some(
            SideIdentity::Unhashed { .. }
            | SideIdentity::Filtered(_)
            | SideIdentity::Unavailable(_),
        ) => {
            return None;
        }
    };
    let hash = match hash {
        HashKind::Sha1 => "sha1",
        HashKind::Sha256 => "sha256",
    };
    Some(
        Key::new()
            .str(hash)
            .bytes(base.as_ref().map_or(&[][..], BlobId::as_bytes))
            .bytes(worktree)
            .bytes(digest),
    )
}

/// The chain under `key` from `partition`, or else `compute`'s, written
/// back when it may be cached. A hit does not call `compute`, which is
/// where the caller loads the steps' edits. Blocking (the cache's IO):
/// executor tasks only.
pub(crate) fn cached(
    partition: &Partition,
    key: Option<&Key>,
    compute: impl FnOnce() -> Chain,
) -> Chain {
    if let Some(CachedChain(chain)) = key.and_then(|k| partition.get::<CachedChain>(k)) {
        return chain;
    }
    let chain = compute();
    if let Some(key) = key
        && chain.cacheable()
    {
        // A failed write only means computing it again next time.
        let _ = partition.put(key, &CachedChain(chain.clone()));
    }
    chain
}

pub(crate) fn encode(chain: &Chain, out: &mut Vec<u8>) {
    uint(out, chain.steps.len() as u64);
    for s in chain.steps.iter() {
        out.push(match s.outcome {
            Outcome::Applied => 0,
            Outcome::Failed => 1,
            Outcome::NotCompleted => 2,
            Outcome::Unaligned => 3,
        });
        oid(out, s.version.as_ref());
    }
    uint(out, chain.groups.len() as u64);
    for g in chain.groups.iter() {
        uint(out, g.steps.len() as u64);
        for &k in g.steps.iter() {
            uint(out, u64::from(k));
        }
        match &g.diff {
            None => out.push(0),
            Some(d) => {
                out.push(1);
                uint(out, u64::from(d.added));
                uint(out, u64::from(d.removed));
                out.push(match d.survival {
                    Survival::All => 0,
                    Survival::Partly => 1,
                    Survival::None => 2,
                });
                uint(out, d.hunks.len() as u64);
                for h in d.hunks.iter() {
                    for v in [
                        u64::from(h.old.start),
                        u64::from(h.old.end),
                        u64::from(h.new.start),
                        u64::from(h.new.end),
                        h.old_byte,
                        h.new_byte,
                    ] {
                        uint(out, v);
                    }
                }
            }
        }
    }
    match &chain.detail {
        Detail::Lines(lines) => {
            out.push(0);
            out.push(u8::from(lines.aligned));
            let sources = runs(&lines.worktree, |a, b| match (a, b) {
                (Source::Base(x), Source::Base(y)) => y == x + 1,
                (Source::Step(x), Source::Step(y)) => x == y,
                (Source::None, Source::None) => true,
                _ => false,
            });
            uint(out, sources.len() as u64);
            for (s, len) in sources {
                match s {
                    Source::None => out.push(0),
                    Source::Base(b) => {
                        out.push(1);
                        uint(out, u64::from(b));
                    }
                    Source::Step(k) => {
                        out.push(2);
                        uint(out, u64::from(k));
                    }
                }
                uint(out, len);
            }
            let fates = runs(&lines.base, |a, b| a == b);
            uint(out, fates.len() as u64);
            for (f, len) in fates {
                match f {
                    Fate::Kept => out.push(0),
                    Fate::Deleted(k) => {
                        out.push(1);
                        uint(out, u64::from(k));
                    }
                    Fate::Outside => out.push(2),
                }
                uint(out, len);
            }
        }
        Detail::FileOnly { reason, matched } => {
            out.push(1);
            out.push(match reason {
                Limit::Steps => 0,
                Limit::Size => 1,
                Limit::Sides => 2,
            });
            match matched {
                None => out.push(0),
                Some(k) => {
                    out.push(1);
                    uint(out, u64::from(*k));
                }
            }
        }
    }
}

pub(crate) fn decode(bytes: &[u8]) -> Option<Chain> {
    let mut r = Reader { bytes, at: 0 };
    let n = r.len()?;
    let mut steps = Vec::with_capacity(n);
    for _ in 0..n {
        let outcome = match r.byte()? {
            0 => Outcome::Applied,
            1 => Outcome::Failed,
            2 => Outcome::NotCompleted,
            3 => Outcome::Unaligned,
            _ => return None,
        };
        steps.push(StepResult {
            outcome,
            version: r.oid()?,
        });
    }
    let step = |r: &mut Reader<'_>| -> Option<u32> {
        let k = r.u32()?;
        ((k as usize) < n).then_some(k)
    };
    let groups_n = r.len()?;
    let mut groups = Vec::with_capacity(groups_n);
    for _ in 0..groups_n {
        let m = r.len()?;
        let mut members = Vec::with_capacity(m);
        for _ in 0..m {
            members.push(step(&mut r)?);
        }
        let diff = match r.byte()? {
            0 => None,
            1 => {
                let added = r.u32()?;
                let removed = r.u32()?;
                let survival = match r.byte()? {
                    0 => Survival::All,
                    1 => Survival::Partly,
                    2 => Survival::None,
                    _ => return None,
                };
                let h = r.len()?;
                let mut hunks = Vec::with_capacity(h);
                for _ in 0..h {
                    let (os, oe, ns, ne) = (r.u32()?, r.u32()?, r.u32()?, r.u32()?);
                    if os > oe || ns > ne {
                        return None;
                    }
                    hunks.push(DiffHunk {
                        old: os..oe,
                        new: ns..ne,
                        old_byte: r.uint()?,
                        new_byte: r.uint()?,
                    });
                }
                Some(GroupDiff {
                    hunks: hunks.into(),
                    added,
                    removed,
                    survival,
                })
            }
            _ => return None,
        };
        groups.push(GroupResult {
            steps: members.into(),
            diff,
        });
    }
    let detail = match r.byte()? {
        0 => {
            let aligned = match r.byte()? {
                0 => false,
                1 => true,
                _ => return None,
            };
            let mut worktree = Vec::new();
            for _ in 0..r.len()? {
                let source = match r.byte()? {
                    0 => Source::None,
                    1 => Source::Base(r.u32()?),
                    2 => Source::Step(step(&mut r)?),
                    _ => return None,
                };
                let len = r.u32()?;
                if worktree.len() as u64 + u64::from(len) > MAX_LINES {
                    return None;
                }
                for i in 0..len {
                    worktree.push(match source {
                        Source::Base(b) => Source::Base(b.checked_add(i)?),
                        other => other,
                    });
                }
            }
            let mut base = Vec::new();
            for _ in 0..r.len()? {
                let fate = match r.byte()? {
                    0 => Fate::Kept,
                    1 => Fate::Deleted(step(&mut r)?),
                    2 => Fate::Outside,
                    _ => return None,
                };
                let len = r.u32()?;
                if base.len() as u64 + u64::from(len) > MAX_LINES {
                    return None;
                }
                base.extend(std::iter::repeat_n(fate, len as usize));
            }
            // Worktree lines name base lines that exist.
            let base_n = base.len() as u32;
            if worktree
                .iter()
                .any(|s| matches!(s, Source::Base(b) if *b >= base_n))
            {
                return None;
            }
            Detail::Lines(Lines {
                worktree: worktree.into(),
                base: base.into(),
                aligned,
            })
        }
        1 => {
            let reason = match r.byte()? {
                0 => Limit::Steps,
                1 => Limit::Size,
                2 => Limit::Sides,
                _ => return None,
            };
            let matched = match r.byte()? {
                0 => None,
                1 => Some(step(&mut r)?),
                _ => return None,
            };
            Detail::FileOnly { reason, matched }
        }
        _ => return None,
    };
    (r.at == bytes.len()).then(|| Chain {
        steps: steps.into(),
        groups: groups.into(),
        detail,
    })
}

/// Runs of `items`: (first item, length), `next(a, b)` telling whether `b`
/// continues a run whose last item is `a`.
fn runs<T: Copy>(items: &[T], next: impl Fn(T, T) -> bool) -> Vec<(T, u64)> {
    let mut out: Vec<(T, u64)> = Vec::new();
    let mut last: Option<T> = None;
    for &item in items {
        match (out.last_mut(), last) {
            (Some(run), Some(prev)) if next(prev, item) => run.1 += 1,
            _ => out.push((item, 1)),
        }
        last = Some(item);
    }
    out
}

/// LEB128. The other cache values of this crate (`claims`, `cutoff`) use
/// the same integers.
pub(crate) fn uint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let low = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(low);
            return;
        }
        out.push(low | 0x80);
    }
}

/// Length-prefixed bytes.
pub(crate) fn bytes(out: &mut Vec<u8>, b: &[u8]) {
    uint(out, b.len() as u64);
    out.extend_from_slice(b);
}

pub(crate) fn oid(out: &mut Vec<u8>, oid: Option<&BlobId>) {
    match oid {
        None => out.push(0),
        Some(BlobId::Sha1(b)) => {
            out.push(1);
            out.extend_from_slice(b);
        }
        Some(BlobId::Sha256(b)) => {
            out.push(2);
            out.extend_from_slice(b);
        }
    }
}

pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Reader<'a> {
        Reader { bytes, at: 0 }
    }

    /// Everything was read.
    pub(crate) fn done(&self) -> bool {
        self.at == self.bytes.len()
    }

    /// Length-prefixed bytes (`bytes`).
    pub(crate) fn bytes(&mut self) -> Option<&'a [u8]> {
        let n = self.len()?;
        self.take(n)
    }

    pub(crate) fn byte(&mut self) -> Option<u8> {
        let b = *self.bytes.get(self.at)?;
        self.at += 1;
        Some(b)
    }

    pub(crate) fn uint(&mut self) -> Option<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            v |= u64::from(b & 0x7f).checked_shl(shift)?;
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }

    pub(crate) fn u32(&mut self) -> Option<u32> {
        u32::try_from(self.uint()?).ok()
    }

    /// A count of items still to read: never more than the bytes left, so
    /// a corrupt value cannot make a huge allocation.
    pub(crate) fn len(&mut self) -> Option<usize> {
        let n = usize::try_from(self.uint()?).ok()?;
        (n <= self.bytes.len() - self.at).then_some(n)
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let b = self.bytes.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(b)
    }

    pub(crate) fn oid(&mut self) -> Option<Option<BlobId>> {
        match self.byte()? {
            0 => Some(None),
            1 => Some(Some(BlobId::Sha1(self.take(20)?.try_into().ok()?))),
            2 => Some(Some(BlobId::Sha256(self.take(32)?.try_into().ok()?))),
            _ => None,
        }
    }
}
