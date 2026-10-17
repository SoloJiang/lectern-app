//! Rename detection between deletions and additions, as git's
//! `diffcore-rename.c` does it for `git diff -M` (docs/design/git.md「工作区
//! 比较」第 5 步): exact renames first, without any limit; then same-basename
//! pairs at 75 %; then, unless sources × destinations exceed the square of
//! `diff.renameLimit`, the full similarity matrix at 50 %. Similarity is
//! git's estimate (`diffcore-delta.c`: chunks of up to 64 bytes ending at a
//! newline, hashed and counted), so scores and pairings match git's.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

use crate::{GitError, Mode, Oid};

/// git's similarity scale: 60000 is identical.
pub(crate) const MAX_SCORE: u32 = 60_000;
/// `-M` without a value: 50 %.
const MINIMUM_SCORE: u32 = 30_000;
/// The basename pass asks for halfway between the minimum and identical.
const BASENAME_SCORE: u32 = MINIMUM_SCORE + (MAX_SCORE - MINIMUM_SCORE) / 2;
/// git keeps this many best sources per destination in the matrix.
const CANDIDATES_PER_DEST: usize = 4;
/// git looks at no more than this many identical sources per destination.
const EXACT_ALTERNATIVES: usize = 100;
/// `diffcore-delta.c`'s hash modulus.
const HASHBASE: u32 = 107_927;

/// A deletion (source) or addition (destination).
pub(crate) struct Candidate<'a> {
    pub path: &'a [u8],
    pub mode: Mode,
    pub oid: Oid,
}

/// A detected rename: indices into the sources and destinations, and git's
/// score (0..=60000).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Pair {
    pub source: usize,
    pub dest: usize,
    pub score: u32,
}

impl Pair {
    /// The percentage `git diff --raw` shows (`R087`).
    pub(crate) fn similarity(&self) -> u8 {
        u8::try_from(self.score * 100 / MAX_SCORE).unwrap_or(100)
    }
}

/// A file's content as the similarity estimate sees it: its size and the
/// byte count of each chunk hash, sorted by hash.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Signature {
    size: u64,
    spans: Vec<(u32, u64)>,
}

impl Signature {
    /// `diffcore-delta.c`'s `hash_chars`. In text (not binary) content a CR
    /// right before an LF is skipped.
    pub(crate) fn new(data: &[u8], is_text: bool) -> Signature {
        let mut chunks: Vec<(u32, u64)> = Vec::new();
        let (mut accum1, mut accum2, mut n) = (0u32, 0u32, 0u64);
        let mut i = 0;
        while i < data.len() {
            let c = data[i];
            i += 1;
            if is_text && c == b'\r' && data.get(i) == Some(&b'\n') {
                continue;
            }
            let old1 = accum1;
            accum1 = (accum1 << 7) ^ (accum2 >> 25);
            accum2 = (accum2 << 7) ^ (old1 >> 25);
            accum1 = accum1.wrapping_add(u32::from(c));
            n += 1;
            if n < 64 && c != b'\n' {
                continue;
            }
            chunks.push((accum1.wrapping_add(accum2.wrapping_mul(0x61)) % HASHBASE, n));
            (accum1, accum2, n) = (0, 0, 0);
        }
        if n > 0 {
            chunks.push((accum1.wrapping_add(accum2.wrapping_mul(0x61)) % HASHBASE, n));
        }
        chunks.sort_unstable_by_key(|c| c.0);
        let mut spans: Vec<(u32, u64)> = Vec::with_capacity(chunks.len());
        for (hash, count) in chunks {
            match spans.last_mut() {
                Some(last) if last.0 == hash => last.1 += count,
                _ => spans.push((hash, count)),
            }
        }
        Signature {
            size: data.len() as u64,
            spans,
        }
    }

    /// Bytes of `self` that also appear in `dst` (`diffcore_count_changes`'s
    /// `src_copied`).
    fn copied_into(&self, dst: &Signature) -> u64 {
        let (mut s, mut d, mut copied) = (0, 0, 0);
        while s < self.spans.len() && d < dst.spans.len() {
            match self.spans[s].0.cmp(&dst.spans[d].0) {
                Ordering::Less => s += 1,
                Ordering::Greater => d += 1,
                Ordering::Equal => {
                    copied += self.spans[s].1.min(dst.spans[d].1);
                    s += 1;
                    d += 1;
                }
            }
        }
        copied
    }
}

fn is_regular(mode: Mode) -> bool {
    matches!(mode, Mode::File | Mode::Executable)
}

fn basename(path: &[u8]) -> &[u8] {
    path.rsplit(|&b| b == b'/').next().unwrap_or(path)
}

/// `diffcore-rename.c`'s `estimate_similarity`: 0 unless both are regular
/// files whose contents could be read.
fn estimate(
    src: &Candidate<'_>,
    src_sig: Option<&Signature>,
    dst: &Candidate<'_>,
    dst_sig: Option<&Signature>,
    minimum: u32,
) -> u32 {
    if !is_regular(src.mode) || !is_regular(dst.mode) {
        return 0;
    }
    let (Some(src_sig), Some(dst_sig)) = (src_sig, dst_sig) else {
        return 0;
    };
    let max = src_sig.size.max(dst_sig.size);
    let delta = max - src_sig.size.min(dst_sig.size);
    // Size changes this drastic can't reach the minimum.
    if max * u64::from(MAX_SCORE - minimum) < delta * u64::from(MAX_SCORE) {
        return 0;
    }
    if dst_sig.size == 0 {
        return 0;
    }
    let copied = src_sig.copied_into(dst_sig);
    u32::try_from(copied * u64::from(MAX_SCORE) / max).unwrap_or(MAX_SCORE)
}

/// Loads the signature of a source (`true`) or destination (`false`) by
/// index; None when it can't be compared (not readable, over the size
/// limit). Only regular files are asked for.
pub(crate) type LoadSignature<'a> =
    dyn FnMut(bool, usize) -> Result<Option<Signature>, GitError> + 'a;

/// Finds renames among `sources` (deletions) and `dests` (additions), both in
/// path order. `limit` is `diff.renameLimit`; `None` or 0 means unlimited.
pub(crate) fn detect(
    sources: &[Candidate<'_>],
    dests: &[Candidate<'_>],
    limit: Option<u64>,
    load: &mut LoadSignature<'_>,
    cancel: &AtomicBool,
) -> Result<Vec<Pair>, GitError> {
    let mut pairs = Vec::new();
    if sources.is_empty() || dests.is_empty() {
        return Ok(pairs);
    }
    let mut used = vec![false; sources.len()];
    let mut renamed = vec![false; dests.len()];

    // 1. Exact renames: same OID; non-regular files also need the same mode.
    // Among identical sources, an unused one with the same basename wins,
    // else the first in path order.
    let mut by_oid: HashMap<Oid, Vec<usize>> = HashMap::new();
    for (i, src) in sources.iter().enumerate() {
        by_oid.entry(src.oid).or_default().push(i);
    }
    for (d, dst) in dests.iter().enumerate() {
        let Some(candidates) = by_oid.get(&dst.oid) else {
            continue;
        };
        let mut best: Option<(usize, u8)> = None;
        let mut left = EXACT_ALTERNATIVES;
        for &s in candidates {
            let src = &sources[s];
            if (!is_regular(src.mode) || !is_regular(dst.mode)) && src.mode != dst.mode {
                continue;
            }
            if used[s] {
                continue;
            }
            let score = 1 + u8::from(basename(src.path) == basename(dst.path));
            if best.is_none_or(|(_, b)| score > b) {
                best = Some((s, score));
                if score == 2 {
                    break;
                }
            }
            left -= 1;
            if left == 0 {
                break;
            }
        }
        if let Some((s, _)) = best {
            used[s] = true;
            renamed[d] = true;
            pairs.push(Pair {
                source: s,
                dest: d,
                score: MAX_SCORE,
            });
        }
    }

    // Shared, not cloned: the similarity matrix asks for each signature once
    // per pair, up to `renameLimit`² times.
    let mut signatures: HashMap<(bool, usize), Option<Rc<Signature>>> = HashMap::new();
    let mut signature = |is_source: bool, i: usize| -> Result<Option<Rc<Signature>>, GitError> {
        let mode = if is_source {
            sources[i].mode
        } else {
            dests[i].mode
        };
        if !is_regular(mode) {
            return Ok(None);
        }
        if let Some(sig) = signatures.get(&(is_source, i)) {
            return Ok(sig.clone());
        }
        let sig = load(is_source, i)?.map(Rc::new);
        signatures.insert((is_source, i), sig.clone());
        Ok(sig)
    };

    // 2. Basenames unique among the remaining sources and among the
    // remaining destinations pair up when they are similar enough.
    fn unique<'p>(
        items: impl Iterator<Item = (usize, &'p [u8])>,
    ) -> HashMap<&'p [u8], Option<usize>> {
        let mut map: HashMap<&[u8], Option<usize>> = HashMap::new();
        for (i, path) in items {
            map.entry(basename(path))
                .and_modify(|e| *e = None)
                .or_insert(Some(i));
        }
        map
    }
    let src_names = unique(
        sources
            .iter()
            .enumerate()
            .filter(|(i, _)| !used[*i])
            .map(|(i, c)| (i, c.path)),
    );
    let dst_names = unique(
        dests
            .iter()
            .enumerate()
            .filter(|(i, _)| !renamed[*i])
            .map(|(i, c)| (i, c.path)),
    );
    let mut basename_pairs: Vec<(usize, usize)> = src_names
        .iter()
        .filter_map(|(name, s)| Some(((*s)?, dst_names.get(name).copied().flatten()?)))
        .collect();
    // Each pair is independent (both sides are unique); sort for determinism.
    basename_pairs.sort_unstable();
    for (s, d) in basename_pairs {
        if cancel.load(AtomicOrdering::Relaxed) {
            return Err(GitError::Cancelled);
        }
        let src_sig = signature(true, s)?;
        let dst_sig = signature(false, d)?;
        let score = estimate(
            &sources[s],
            src_sig.as_deref(),
            &dests[d],
            dst_sig.as_deref(),
            BASENAME_SCORE,
        );
        if score >= BASENAME_SCORE {
            used[s] = true;
            renamed[d] = true;
            pairs.push(Pair {
                source: s,
                dest: d,
                score,
            });
        }
    }

    // 3. The matrix, unless it is too big (`too_many_rename_candidates`).
    let remaining_sources: Vec<usize> = (0..sources.len()).filter(|&i| !used[i]).collect();
    let remaining_dests: Vec<usize> = (0..dests.len()).filter(|&i| !renamed[i]).collect();
    if remaining_sources.is_empty() || remaining_dests.is_empty() {
        return Ok(pairs);
    }
    if let Some(limit) = limit.filter(|&l| l > 0) {
        let cells = (remaining_sources.len() as u128) * (remaining_dests.len() as u128);
        if cells > u128::from(limit) * u128::from(limit) {
            return Ok(pairs);
        }
    }
    #[derive(Clone, Copy)]
    struct Slot {
        dest: Option<usize>,
        source: usize,
        score: u32,
        same_name: bool,
    }
    // `score_compare`: unused slots last, then higher score, then same basename.
    fn compare(a: &Slot, b: &Slot) -> Ordering {
        match (a.dest, b.dest) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            _ => b.score.cmp(&a.score).then(b.same_name.cmp(&a.same_name)),
        }
    }
    let empty = Slot {
        dest: None,
        source: 0,
        score: 0,
        same_name: false,
    };
    let mut matrix: Vec<Slot> = Vec::with_capacity(remaining_dests.len() * CANDIDATES_PER_DEST);
    for &d in &remaining_dests {
        if cancel.load(AtomicOrdering::Relaxed) {
            return Err(GitError::Cancelled);
        }
        let mut best = [empty; CANDIDATES_PER_DEST];
        let dst_sig = signature(false, d)?;
        for &s in &remaining_sources {
            let src_sig = signature(true, s)?;
            let candidate = Slot {
                dest: Some(d),
                source: s,
                score: estimate(
                    &sources[s],
                    src_sig.as_deref(),
                    &dests[d],
                    dst_sig.as_deref(),
                    MINIMUM_SCORE,
                ),
                same_name: basename(sources[s].path) == basename(dests[d].path),
            };
            // `record_if_better`: replace the worst slot if this one is better.
            let mut worst = 0;
            for i in 1..CANDIDATES_PER_DEST {
                if compare(&best[i], &best[worst]) == Ordering::Greater {
                    worst = i;
                }
            }
            if compare(&best[worst], &candidate) == Ordering::Greater {
                best[worst] = candidate;
            }
        }
        matrix.extend_from_slice(&best);
    }
    // Stable, like git's STABLE_QSORT.
    matrix.sort_by(compare);
    for slot in matrix {
        let Some(d) = slot.dest else {
            break;
        };
        if slot.score < MINIMUM_SCORE {
            break;
        }
        if renamed[d] || used[slot.source] {
            continue;
        }
        renamed[d] = true;
        used[slot.source] = true;
        pairs.push(Pair {
            source: slot.source,
            dest: d,
            score: slot.score,
        });
    }
    Ok(pairs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HashKind;

    fn cand<'a>(path: &'a str, mode: Mode, content: &[u8]) -> (Candidate<'a>, Vec<u8>) {
        (
            Candidate {
                path: path.as_bytes(),
                mode,
                oid: Oid::for_blob(HashKind::Sha1, content),
            },
            content.to_vec(),
        )
    }

    fn run(
        sources: &[(Candidate<'_>, Vec<u8>)],
        dests: &[(Candidate<'_>, Vec<u8>)],
        limit: Option<u64>,
    ) -> Vec<(usize, usize, u8)> {
        let s: Vec<Candidate<'_>> = sources
            .iter()
            .map(|(c, _)| Candidate {
                path: c.path,
                mode: c.mode,
                oid: c.oid,
            })
            .collect();
        let d: Vec<Candidate<'_>> = dests
            .iter()
            .map(|(c, _)| Candidate {
                path: c.path,
                mode: c.mode,
                oid: c.oid,
            })
            .collect();
        let mut load = |is_source: bool, i: usize| {
            let data = if is_source {
                &sources[i].1
            } else {
                &dests[i].1
            };
            Ok(Some(Signature::new(data, !data.contains(&0))))
        };
        let mut pairs: Vec<(usize, usize, u8)> =
            detect(&s, &d, limit, &mut load, &AtomicBool::new(false))
                .unwrap()
                .iter()
                .map(|p| (p.source, p.dest, p.similarity()))
                .collect();
        pairs.sort_unstable();
        pairs
    }

    fn lines(prefix: &str, range: std::ops::RangeInclusive<u32>) -> Vec<u8> {
        range
            .flat_map(|i| format!("{prefix} {i}\n").into_bytes())
            .collect()
    }

    #[test]
    fn rename_signature_skips_cr_before_lf_only_in_text() {
        let lf = Signature::new(b"a\nb\n", true);
        assert_eq!(Signature::new(b"a\r\nb\r\n", true).spans, lf.spans);
        assert_ne!(Signature::new(b"a\r\nb\r\n", false).spans, lf.spans);
        // A lone CR is content.
        assert_ne!(
            Signature::new(b"a\rb\n", true).spans,
            Signature::new(b"ab\n", true).spans
        );
        // Chunks end at 64 bytes without a newline.
        let long = Signature::new(&[b'x'; 130], true);
        assert_eq!(long.spans.iter().map(|s| s.1).sum::<u64>(), 130);
        assert_eq!(
            long.spans.len(),
            2,
            "two 64-byte chunks share a hash, plus 2 bytes"
        );
    }

    #[test]
    fn rename_similarity_matches_git_scores() {
        // Scores git 2.43 printed for these pairs (`R094` etc. in the
        // `renames` fixture): 8 of 10 lines kept plus two new ones.
        let old = lines("hi line", 1..=10);
        let mut new = lines("hi line", 1..=8);
        new.extend_from_slice(b"x\ny\n");
        let sources = [cand("old/hi.txt", Mode::File, &old)];
        let dests = [cand("new/hi.txt", Mode::File, &new)];
        assert_eq!(run(&sources, &dests, None), [(0, 0, 79)]);
        // A 60 % prefix still pairs: git's size filter compares with the larger side.
        let old: Vec<u8> = (1..=10)
            .flat_map(|i| format!("line {i:05}\n").into_bytes())
            .collect();
        let new: Vec<u8> = (1..=6)
            .flat_map(|i| format!("line {i:05}\n").into_bytes())
            .collect();
        let sources = [cand("src", Mode::File, &old)];
        let dests = [cand("dst", Mode::File, &new)];
        assert_eq!(run(&sources, &dests, None), [(0, 0, 60)]);
    }

    #[test]
    fn rename_exact_prefers_the_same_basename_then_path_order() {
        let sources = [
            cand("old/a.txt", Mode::File, b"same\n"),
            cand("old/b.txt", Mode::File, b"same\n"),
        ];
        let dests = [
            cand("new/b.txt", Mode::File, b"same\n"),
            cand("new/z.txt", Mode::File, b"same\n"),
        ];
        assert_eq!(run(&sources, &dests, None), [(0, 1, 100), (1, 0, 100)]);
    }

    #[test]
    fn rename_exact_needs_the_same_mode_unless_both_are_regular() {
        let sources = [
            cand("a", Mode::File, b"x"),
            cand("l", Mode::Symlink, b"target"),
        ];
        let dests = [
            cand("b", Mode::Executable, b"x"),
            cand("m", Mode::File, b"target"),
        ];
        assert_eq!(run(&sources, &dests, None), [(0, 0, 100)]);
    }

    #[test]
    fn rename_symlinks_and_gitlinks_pair_only_exactly() {
        let sources = [cand("l", Mode::Symlink, b"first/target")];
        let dests = [cand("m", Mode::Symlink, b"first/targets")];
        assert_eq!(run(&sources, &dests, None), []);
    }

    #[test]
    fn rename_basename_pass_runs_before_the_limit() {
        // Same basenames, 94 % similar: found even with the matrix limited away.
        let mut sources = Vec::new();
        let mut dests = Vec::new();
        let names = ["one.txt", "two.txt", "three.txt"];
        let old: Vec<Vec<u8>> = names.iter().map(|n| lines(n, 1..=20)).collect();
        let new: Vec<Vec<u8>> = old
            .iter()
            .map(|o| {
                String::from_utf8(o.clone())
                    .unwrap()
                    .replacen(" 5\n", " FIVE\n", 1)
                    .into_bytes()
            })
            .collect();
        let paths_old: Vec<String> = names.iter().map(|n| format!("a/{n}")).collect();
        let paths_new: Vec<String> = names.iter().map(|n| format!("b/{n}")).collect();
        for i in 0..3 {
            sources.push(cand(&paths_old[i], Mode::File, &old[i]));
            dests.push(cand(&paths_new[i], Mode::File, &new[i]));
        }
        let found = run(&sources, &dests, Some(1));
        assert_eq!(found.len(), 3);
        assert!(found.iter().all(|&(s, d, _)| s == d));
    }

    #[test]
    fn rename_matrix_is_skipped_over_the_limit_squared() {
        let names: Vec<(String, String)> = (0..3)
            .map(|i| (format!("c/x{i}"), format!("d/y{i}")))
            .collect();
        let old: Vec<Vec<u8>> = (0..3)
            .map(|i| lines(&format!("x{i} row"), 1..=20))
            .collect();
        let new: Vec<Vec<u8>> = old
            .iter()
            .map(|o| {
                String::from_utf8(o.clone())
                    .unwrap()
                    .replacen(" 5\n", " FIVE\n", 1)
                    .into_bytes()
            })
            .collect();
        let sources: Vec<_> = (0..3)
            .map(|i| cand(&names[i].0, Mode::File, &old[i]))
            .collect();
        let dests: Vec<_> = (0..3)
            .map(|i| cand(&names[i].1, Mode::File, &new[i]))
            .collect();
        // 3 × 3 = 9 cells: over 2² = 4, within 3² = 9; 0 is unlimited.
        assert_eq!(run(&sources, &dests, Some(2)), []);
        assert_eq!(run(&sources, &dests, Some(3)).len(), 3);
        assert_eq!(run(&sources, &dests, Some(0)).len(), 3);
        assert_eq!(run(&sources, &dests, None).len(), 3);
    }

    #[test]
    fn rename_is_cancellable() {
        let old = lines("x", 1..=20);
        let new = lines("x", 1..=19);
        let sources = [cand("a", Mode::File, &old)];
        let dests = [cand("b", Mode::File, &new)];
        let s = [Candidate {
            path: sources[0].0.path,
            mode: Mode::File,
            oid: sources[0].0.oid,
        }];
        let d = [Candidate {
            path: dests[0].0.path,
            mode: Mode::File,
            oid: dests[0].0.oid,
        }];
        let mut load = |_: bool, _: usize| Ok(None);
        let cancelled = detect(&s, &d, None, &mut load, &AtomicBool::new(true));
        assert!(matches!(cancelled, Err(GitError::Cancelled)));
    }
}
