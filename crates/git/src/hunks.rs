//! Line diffs, the size of their unified patch, and the hunk cache keyed by
//! blob OIDs (docs/design/git.md「缓存」). The diff is gix's imara-diff
//! (`gix::diff::blob`, Histogram; ADR 0024).

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use gix::diff::blob::{Algorithm, Diff, InternedInput, sources::byte_lines};

use crate::Oid;

/// One region of change, without context. Line numbers are 0-based in each
/// side's whole file; an empty range is a pure insertion or removal point.
///
/// `old_byte` and `new_byte` are the byte offsets of `old.start` and
/// `new.start` in that side's canonical content: the bytes of the side's
/// blob, which are also [`Text::bytes`](crate::Text::bytes) of
/// [`ChangeSet::texts`](crate::ChangeSet::texts). For an empty range it is
/// the offset of the line after the point (the content's length at the end).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Hunk {
    pub old: Range<u32>,
    pub new: Range<u32>,
    pub old_byte: u64,
    pub new_byte: u64,
}

/// A file's line diff.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FileDiff {
    /// Without context; line numbers cover both sides' whole files.
    pub hunks: Arc<[Hunk]>,
    pub added: u32,
    pub removed: u32,
}

impl FileDiff {
    /// No line changed (an exact rename, say).
    pub(crate) fn empty() -> FileDiff {
        FileDiff {
            hunks: Arc::from(Vec::new()),
            added: 0,
            removed: 0,
        }
    }
}

/// A computed diff, the bytes of its unified patch body (hunk headers and
/// lines with three lines of context, no file header), which the per-file
/// and total patch limits count, and the sizes of the two contents, which
/// the line-diff limit and `Detail::Binary` need. All of it depends only on
/// the two contents, so it is cached with the hunks and a hit reads neither
/// a blob nor its header.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Computed {
    pub diff: FileDiff,
    pub body_bytes: u64,
    /// Bytes of the old and the new content.
    pub sizes: [u64; 2],
}

/// Lines of context in the patch the limits measure (`git diff`'s default).
const CONTEXT: u32 = 3;
const NO_NEWLINE: &[u8] = b"\\ No newline at end of file\n";

/// The line diff of two contents, with the algorithm of the hunk cache
/// (Histogram, hunks slid to git's indent heuristic): the hunks, with the
/// byte offset of each side's first line, and the counts. Lines end after
/// `\n`; a last line without one is a line. None when either side is
/// longer than `max_bytes`. Pure: reads no object database and no cache,
/// any thread. `review` diffs the versions of its change chains with it
/// (docs/design/review.md「改动链」), which never are blobs.
///
/// ```
/// let diff = lectern_git::line_diff(b"a\nb\nc\n", b"a\nB\nc\n", 1024).unwrap();
/// assert_eq!((diff.added, diff.removed), (1, 1));
/// let hunk = &diff.hunks[0];
/// assert_eq!((hunk.old.clone(), hunk.new.clone()), (1..2, 1..2));
/// assert_eq!((hunk.old_byte, hunk.new_byte), (2, 2));
/// assert!(lectern_git::line_diff(b"a\n", b"b\n", 1).is_none());
/// ```
pub fn line_diff(old: &[u8], new: &[u8], max_bytes: u64) -> Option<FileDiff> {
    let fits = |side: &[u8]| u64::try_from(side.len()).is_ok_and(|len| len <= max_bytes);
    (fits(old) && fits(new)).then(|| compute(old, new).diff)
}

/// The Histogram line diff of `old` and `new`, with hunks slid to git's
/// indent heuristic. Lines end after `\n`; a last line without one is a
/// line (and differs from the same text with one), as in git.
pub(crate) fn compute(old: &[u8], new: &[u8]) -> Computed {
    let input = InternedInput::new(byte_lines(old), byte_lines(new));
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    diff.postprocess_lines(&input);
    let old_lines: Vec<&[u8]> = input.before.iter().map(|t| input.interner[*t]).collect();
    let new_lines: Vec<&[u8]> = input.after.iter().map(|t| input.interner[*t]).collect();
    // Hunks come in order on both sides, so one forward walk over each
    // side's lines gives every offset.
    let (mut old_at, mut new_at) = (Cursor::default(), Cursor::default());
    let hunks: Vec<Hunk> = diff
        .hunks()
        .map(|h| Hunk {
            old_byte: old_at.advance(&old_lines, h.before.start),
            new_byte: new_at.advance(&new_lines, h.after.start),
            old: h.before,
            new: h.after,
        })
        .collect();
    let body_bytes = patch_body_bytes(&old_lines, &new_lines, &hunks);
    Computed {
        diff: FileDiff {
            hunks: hunks.into(),
            added: diff.count_additions(),
            removed: diff.count_removals(),
        },
        body_bytes,
        sizes: [old.len() as u64, new.len() as u64],
    }
}

/// Every old line removed and every new line added, in one hunk: a new or
/// deleted file, and a type change (git shows a file ↔ symbolic link change
/// as a deletion and a creation, and counts it that way).
pub(crate) fn whole(old: &[u8], new: &[u8]) -> Computed {
    let old_lines: Vec<&[u8]> = byte_lines(old).collect();
    let new_lines: Vec<&[u8]> = byte_lines(new).collect();
    let (removed, added) = (count(&old_lines), count(&new_lines));
    let hunks = if removed == 0 && added == 0 {
        Vec::new()
    } else {
        vec![Hunk {
            old: 0..removed,
            new: 0..added,
            old_byte: 0,
            new_byte: 0,
        }]
    };
    let body_bytes = patch_body_bytes(&old_lines, &new_lines, &hunks);
    Computed {
        diff: FileDiff {
            hunks: hunks.into(),
            added,
            removed,
        },
        body_bytes,
        sizes: [old.len() as u64, new.len() as u64],
    }
}

/// A line and its byte offset, walking one side's lines forward.
#[derive(Default)]
struct Cursor {
    line: u32,
    byte: u64,
}

impl Cursor {
    /// The byte offset of line `to` (not before the cursor's line). The
    /// lines keep their `\n` (and any `\r` before it), so their lengths add
    /// up to the content's bytes.
    fn advance(&mut self, lines: &[&[u8]], to: u32) -> u64 {
        while self.line < to {
            self.byte += lines[self.line as usize].len() as u64;
            self.line += 1;
        }
        self.byte
    }
}

fn count(lines: &[&[u8]]) -> u32 {
    u32::try_from(lines.len()).unwrap_or(u32::MAX)
}

/// The unified patch body: `@@` headers, context, removed and added lines,
/// each line with its one-byte prefix, plus git's marker after a last line
/// without a newline. Hunks closer than twice the context merge, as in git.
fn patch_body_bytes(old: &[&[u8]], new: &[&[u8]], hunks: &[Hunk]) -> u64 {
    fn line(l: &[u8]) -> u64 {
        let marker = if l.ends_with(b"\n") {
            0
        } else {
            1 + NO_NEWLINE.len()
        };
        (1 + l.len() + marker) as u64
    }
    fn digits(mut n: u32) -> u64 {
        let mut d = 1;
        while n >= 10 {
            n /= 10;
            d += 1;
        }
        d
    }
    let old_len = count(old);
    let mut total = 0u64;
    let mut i = 0;
    while i < hunks.len() {
        let mut j = i;
        while j + 1 < hunks.len() && hunks[j + 1].old.start - hunks[j].old.end <= 2 * CONTEXT {
            j += 1;
        }
        let (first, last) = (&hunks[i], &hunks[j]);
        // The unchanged stretches before the first and after the last hunk
        // have the same length on both sides.
        let lead = first.old.start.min(CONTEXT);
        let trail = (old_len - last.old.end).min(CONTEXT);
        let old_range = first.old.start - lead..last.old.end + trail;
        let new_range = first.new.start - lead..last.new.end + trail;
        // "@@ -a,b +c,d @@\n"
        total += 12
            + digits(old_range.start + 1)
            + digits(old_range.len() as u32)
            + digits(new_range.start + 1)
            + digits(new_range.len() as u32);
        // Context and removed lines come from the old side, added from the new.
        total += old_range.map(|k| line(old[k as usize])).sum::<u64>();
        for hunk in &hunks[i..=j] {
            total += hunk.new.clone().map(|k| line(new[k as usize])).sum::<u64>();
        }
        i = j + 1;
    }
    total
}

/// Bumped when the cached data or its computation changes: 3 added the
/// hunks' byte offsets. When the hunks move into `cache`'s `git.diff`
/// namespace this is that namespace's version.
pub(crate) const CACHE_FORMAT: u16 = 3;
/// The in-process LRU's budget (docs/design/git.md「性能」).
pub(crate) const CACHE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum DiffAlgorithm {
    Histogram,
}

/// P1 has one whitespace mode: whitespace counts.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Whitespace {
    Exact,
}

/// The cache key. `Oid` carries its `HashKind`, so OIDs of different
/// algorithms never share an entry.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct HunkKey {
    format: u16,
    old: Oid,
    new: Oid,
    algorithm: DiffAlgorithm,
    whitespace: Whitespace,
}

impl HunkKey {
    /// The key of a line diff (Histogram, whitespace counts) of two blobs.
    pub(crate) fn lines(old: Oid, new: Oid) -> HunkKey {
        HunkKey {
            format: CACHE_FORMAT,
            old,
            new,
            algorithm: DiffAlgorithm::Histogram,
            whitespace: Whitespace::Exact,
        }
    }

    /// The same key under another format version: an entry written by an
    /// older build.
    #[cfg(test)]
    pub(crate) fn with_format(self, format: u16) -> HunkKey {
        HunkKey { format, ..self }
    }
}

/// A byte-bounded LRU of computed diffs. Only line numbers are stored, never
/// text: the text is always in the object database under the same OIDs.
pub(crate) struct HunkCache {
    entries: HashMap<HunkKey, Slot>,
    /// Use order: tick → key; the smallest tick is evicted first.
    order: BTreeMap<u64, HunkKey>,
    clock: u64,
    used: usize,
    capacity: usize,
}

struct Slot {
    value: Computed,
    tick: u64,
    cost: usize,
}

impl HunkCache {
    pub(crate) fn new(capacity: usize) -> HunkCache {
        HunkCache {
            entries: HashMap::new(),
            order: BTreeMap::new(),
            clock: 0,
            used: 0,
            capacity,
        }
    }

    /// What an entry costs against the budget: its hunks plus a fixed
    /// overhead for the key, the slot (the two sizes included) and the maps.
    fn cost(value: &Computed) -> usize {
        value.diff.hunks.len() * size_of::<Hunk>() + 208
    }

    pub(crate) fn get(&mut self, key: &HunkKey) -> Option<Computed> {
        self.clock += 1;
        let tick = self.clock;
        let slot = self.entries.get_mut(key)?;
        self.order.remove(&slot.tick);
        slot.tick = tick;
        self.order.insert(tick, *key);
        Some(slot.value.clone())
    }

    pub(crate) fn insert(&mut self, key: HunkKey, value: Computed) {
        let cost = HunkCache::cost(&value);
        if cost > self.capacity {
            return;
        }
        if let Some(old) = self.entries.remove(&key) {
            self.order.remove(&old.tick);
            self.used -= old.cost;
        }
        while self.used + cost > self.capacity {
            let Some((_, evicted)) = self.order.pop_first() else {
                break;
            };
            if let Some(slot) = self.entries.remove(&evicted) {
                self.used -= slot.cost;
            }
        }
        self.clock += 1;
        let tick = self.clock;
        self.order.insert(tick, key);
        self.entries.insert(key, Slot { value, tick, cost });
        self.used += cost;
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(crate) fn used(&self) -> usize {
        self.used
    }
}

/// The process-wide cache, created on first use (never before the first
/// frame: nothing in this crate runs then). Content-identical files in
/// different worktrees and repositories share entries. The lock is held
/// only for one lookup or insert.
fn global() -> &'static Mutex<HunkCache> {
    static CACHE: OnceLock<Mutex<HunkCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HunkCache::new(CACHE_BYTES)))
}

pub(crate) fn cached(key: &HunkKey) -> Option<Computed> {
    let hit = global()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(key);
    #[cfg(test)]
    hook::record(hit.is_some());
    hit
}

pub(crate) fn remember(key: HunkKey, value: Computed) {
    global()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(key, value);
}

/// Counting hook: hunk-cache lookups on this thread.
#[cfg(test)]
pub(crate) mod hook {
    use std::cell::Cell;

    thread_local! {
        static HITS: Cell<usize> = const { Cell::new(0) };
        static MISSES: Cell<usize> = const { Cell::new(0) };
    }

    pub(crate) fn record(hit: bool) {
        let counter = if hit { &HITS } else { &MISSES };
        counter.with(|c| c.set(c.get() + 1));
    }

    /// (hits, misses) so far on this thread.
    pub(crate) fn counts() -> (usize, usize) {
        (HITS.with(Cell::get), MISSES.with(Cell::get))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HashKind;

    fn oid(kind: HashKind, byte: u8) -> Oid {
        Oid::from_bytes(kind, &vec![byte; kind.byte_len()]).unwrap()
    }

    fn computed(hunks: usize) -> Computed {
        Computed {
            diff: FileDiff {
                hunks: (0..hunks as u32)
                    .map(|i| Hunk {
                        old: i..i + 1,
                        new: i..i + 1,
                        old_byte: u64::from(i) * 10,
                        new_byte: u64::from(i) * 12,
                    })
                    .collect(),
                added: hunks as u32,
                removed: hunks as u32,
            },
            body_bytes: 0,
            sizes: [0, 0],
        }
    }

    #[test]
    fn hunk_cache_hits_the_same_pair_and_misses_others() {
        let mut cache = HunkCache::new(CACHE_BYTES);
        let (a, b) = (oid(HashKind::Sha1, 1), oid(HashKind::Sha1, 2));
        cache.insert(HunkKey::lines(a, b), computed(2));
        assert_eq!(cache.get(&HunkKey::lines(a, b)), Some(computed(2)));
        // Direction matters: b → a is another diff.
        assert_eq!(cache.get(&HunkKey::lines(b, a)), None);
    }

    #[test]
    fn hunk_cache_keeps_the_byte_offsets() {
        let mut cache = HunkCache::new(CACHE_BYTES);
        let (a, b) = (oid(HashKind::Sha1, 3), oid(HashKind::Sha1, 4));
        let value = compute(b"a\r\nb\r\nc", b"a\r\nB\r\nc\n");
        cache.insert(HunkKey::lines(a, b), value.clone());
        let hit = cache.get(&HunkKey::lines(a, b)).unwrap();
        assert_eq!(hit, value);
        let offsets: Vec<_> = hit
            .diff
            .hunks
            .iter()
            .map(|h| (h.old_byte, h.new_byte))
            .collect();
        assert_eq!(offsets, [(3, 3)]);
    }

    #[test]
    fn hunk_cache_entry_of_an_older_format_is_not_read() {
        // An entry written before the hunks carried byte offsets (format 2)
        // has the same OIDs but another key: the new format misses it and
        // computes again instead of reading hunks without offsets.
        assert_eq!(CACHE_FORMAT, 3);
        let mut cache = HunkCache::new(CACHE_BYTES);
        let (a, b) = (oid(HashKind::Sha1, 5), oid(HashKind::Sha1, 6));
        let old = HunkKey::lines(a, b).with_format(2);
        cache.insert(old, computed(2));
        assert_eq!(cache.get(&HunkKey::lines(a, b)), None);
        assert_eq!(cache.get(&old), Some(computed(2)));
        assert_ne!(old, HunkKey::lines(a, b));
    }

    #[test]
    fn hunk_offsets_count_every_byte_of_the_lines_before() {
        // CRLF lines count both bytes; a last line without `\n` is a line.
        let c = compute(b"one\r\ntwo\r\nthree", b"one\r\n2\r\nthree\r\nfour");
        let got: Vec<_> = c
            .diff
            .hunks
            .iter()
            .map(|h| (h.old.clone(), h.new.clone(), h.old_byte, h.new_byte))
            .collect();
        assert_eq!(got, [(1..3, 1..4, 5, 5)]);
        // Several hunks: each side's offset of its own first line.
        let old = b"a\nb\nc\nd\ne\nf\ng\nh\n";
        let new = b"a\nBB\nc\nd\ne\nf\ng\nh\nnew\n";
        let c = compute(old, new);
        let got: Vec<_> = c
            .diff
            .hunks
            .iter()
            .map(|h| (h.old.clone(), h.new.clone(), h.old_byte, h.new_byte))
            .collect();
        // The insertion at the end points past the last line on the old side.
        assert_eq!(got, [(1..2, 1..2, 2, 2), (8..8, 8..9, 16, 17)]);
        // A whole-file hunk starts at 0 on both sides.
        let w = whole(b"x\n", b"");
        assert_eq!((w.diff.hunks[0].old_byte, w.diff.hunks[0].new_byte), (0, 0));
    }

    #[test]
    fn line_diff_size_limit_applies_to_either_side() {
        let (old, new) = (b"a\nb\n", b"a\nB\nc\n");
        let expected = compute(old, new).diff;
        // Exactly the limit is fine; one byte over on either side is None.
        assert_eq!(line_diff(old, new, 6), Some(expected.clone()));
        assert_eq!(line_diff(old, new, 5), None);
        assert_eq!(line_diff(new, old, 5), None);
        assert_eq!(line_diff(b"", b"", 0), Some(FileDiff::empty()));
        assert_eq!(line_diff(old, old, u64::MAX), Some(FileDiff::empty()));
        assert_eq!(line_diff(old, new, u64::MAX), Some(expected));
    }

    #[test]
    fn line_diff_hunks_carry_both_sides_byte_offsets() {
        // CRLF lines, a last line without `\n`, an insertion at the end.
        let d = line_diff(
            b"one\r\ntwo\r\nthree",
            b"one\r\n2\r\nthree\r\nfour",
            u64::MAX,
        )
        .unwrap();
        let got: Vec<_> = d
            .hunks
            .iter()
            .map(|h| (h.old.clone(), h.new.clone(), h.old_byte, h.new_byte))
            .collect();
        assert_eq!(got, [(1..3, 1..4, 5, 5)]);
        assert_eq!((d.added, d.removed), (3, 2));
        // From nothing: one hunk of every line, at 0 on both sides.
        let d = line_diff(b"", b"x\ny\n", u64::MAX).unwrap();
        assert_eq!(
            (d.hunks[0].old.clone(), d.hunks[0].new.clone()),
            (0..0, 0..2)
        );
        assert_eq!((d.hunks[0].old_byte, d.hunks[0].new_byte), (0, 0));
    }

    #[test]
    fn hunk_cache_key_separates_hash_kinds() {
        let mut cache = HunkCache::new(CACHE_BYTES);
        cache.insert(
            HunkKey::lines(oid(HashKind::Sha1, 1), oid(HashKind::Sha1, 2)),
            computed(1),
        );
        // The same leading bytes as SHA-256 OIDs.
        let mut long = [0u8; 32];
        long[..20].copy_from_slice(&[1; 20]);
        let old = Oid::from_bytes(HashKind::Sha256, &long).unwrap();
        long[..20].copy_from_slice(&[2; 20]);
        let new = Oid::from_bytes(HashKind::Sha256, &long).unwrap();
        assert_eq!(cache.get(&HunkKey::lines(old, new)), None);
        assert_eq!(
            cache.get(&HunkKey::lines(
                oid(HashKind::Sha256, 1),
                oid(HashKind::Sha256, 2)
            )),
            None
        );
    }

    #[test]
    fn hunk_cache_evicts_least_recently_used_within_its_budget() {
        let one = HunkCache::cost(&computed(10));
        let mut cache = HunkCache::new(one * 3);
        let key = |n| HunkKey::lines(oid(HashKind::Sha1, n), oid(HashKind::Sha1, 100));
        for n in 0..3 {
            cache.insert(key(n), computed(10));
        }
        // Touch 0, so 1 is now the least recently used.
        assert!(cache.get(&key(0)).is_some());
        cache.insert(key(3), computed(10));
        assert_eq!(cache.len(), 3);
        assert!(cache.used() <= one * 3);
        assert!(cache.get(&key(1)).is_none());
        for n in [0, 2, 3] {
            assert!(cache.get(&key(n)).is_some(), "{n}");
        }
        // Re-inserting a key replaces it without double counting.
        cache.insert(key(3), computed(10));
        assert_eq!(cache.used(), one * 3);
        // An entry bigger than the whole budget is not kept.
        cache.insert(key(9), computed(1000));
        assert!(cache.get(&key(9)).is_none());
    }

    #[test]
    fn truncate_patch_body_counts_context_and_markers() {
        // One change in the middle of 10 lines: 3 lines of context each side.
        let old: Vec<u8> = (0..10)
            .flat_map(|i| format!("l{i}\n").into_bytes())
            .collect();
        let new = String::from_utf8(old.clone())
            .unwrap()
            .replace("l5\n", "L5\n");
        let c = compute(&old, new.as_bytes());
        assert_eq!(c.diff.hunks.len(), 1);
        // "@@ -3,7 +3,7 @@\n" is 16 bytes; 7 old lines and 1 new line of 4 bytes each.
        assert_eq!(c.body_bytes, 16 + 8 * 4);
        // A last line without a newline gets git's marker.
        let c = whole(b"", b"x");
        assert_eq!(c.body_bytes, 16 + 2 + 1 + NO_NEWLINE.len() as u64);
        assert_eq!((c.diff.added, c.diff.removed), (1, 0));
        // Two changes 6 lines apart share one hunk header, 7 apart don't.
        let old: Vec<u8> = (0..30)
            .flat_map(|i| format!("l{i:02}\n").into_bytes())
            .collect();
        let text = String::from_utf8(old.clone()).unwrap();
        let near = text.replace("l05\n", "x\n").replace("l12\n", "y\n");
        let far = text.replace("l05\n", "x\n").replace("l13\n", "y\n");
        let near = compute(&old, near.as_bytes()).body_bytes;
        let far = compute(&old, far.as_bytes()).body_bytes;
        // Merged: "@@ -3,14 +3,14 @@\n" (18), 14 old lines of 5 bytes, 2 new of 3.
        assert_eq!(near, 18 + 14 * 5 + 2 * 3);
        // Split: "@@ -3,7 +3,7 @@\n" (16) and "@@ -11,7 +11,7 @@\n" (18), 14 old, 2 new.
        assert_eq!(far, 16 + 18 + 14 * 5 + 2 * 3);
    }
}
