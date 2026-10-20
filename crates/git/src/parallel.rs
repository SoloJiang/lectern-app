//! Spreading one call's independent system calls over a few scoped threads
//! (ADR 0038). Only the comparison's stat of every tracked path uses it; the
//! untracked walk's directory listing stays serial (prefetching it on other
//! threads was slower). The threads are created inside the call and joined
//! before it returns;
//! results come back in input order, so the outcome never depends on the
//! scheduling.

use std::num::NonZeroUsize;

/// At most this many threads per call, the calling one included: git's
/// `core.preloadIndex` gains little past a handful on SSDs, and a call
/// shouldn't take the whole machine from the UI.
const MAX_THREADS: usize = 4;

/// How many threads a call may use: the CPUs, at most [`MAX_THREADS`].
pub(crate) fn threads() -> usize {
    std::thread::available_parallelism()
        .map_or(1, NonZeroUsize::get)
        .min(MAX_THREADS)
}

/// `f` of every item, in item order, over at most `threads` contiguous
/// chunks of at least `min_chunk` items: the calling thread does the first
/// chunk, scoped threads the others. Serial when there is only one chunk.
/// A panic in `f` propagates to the caller.
pub(crate) fn map<T: Sync, R: Send>(
    items: &[T],
    threads: usize,
    min_chunk: usize,
    f: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let chunks = threads
        .min(items.len() / min_chunk.max(1))
        .clamp(1, MAX_THREADS);
    if chunks == 1 {
        return items.iter().map(&f).collect();
    }
    let size = items.len().div_ceil(chunks);
    let f = &f;
    std::thread::scope(|scope| {
        let mut parts = items.chunks(size);
        let first = parts.next().unwrap_or_default();
        let handles: Vec<_> = parts
            .map(|part| scope.spawn(move || part.iter().map(f).collect::<Vec<R>>()))
            .collect();
        let mut out: Vec<R> = first.iter().map(f).collect();
        out.reserve(items.len() - out.len());
        for handle in handles {
            match handle.join() {
                Ok(part) => out.extend(part),
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallel_map_keeps_item_order() {
        let items: Vec<u32> = (0..10_000).collect();
        for threads in 1..=6 {
            for min_chunk in [1, 100, 3000, 20_000] {
                let out = map(&items, threads, min_chunk, |&i| i * 2);
                assert_eq!(out.len(), items.len());
                assert!(out.iter().enumerate().all(|(i, &x)| x == i as u32 * 2));
            }
        }
        assert!(map(&[] as &[u32], 4, 1, |&i| i).is_empty());
    }

    #[test]
    fn parallel_map_uses_other_threads_only_for_large_inputs() {
        let me = std::thread::current().id();
        let items: Vec<u32> = (0..8).collect();
        let small = map(&items, 4, 100, |_| std::thread::current().id());
        assert!(small.iter().all(|&id| id == me), "one chunk: serial");
        let large = map(&items, 4, 2, |_| std::thread::current().id());
        assert_eq!(large[0], me, "the first chunk runs on the caller");
        assert!(large.iter().any(|&id| id != me));
    }

    #[test]
    #[should_panic(expected = "boom")]
    fn parallel_map_propagates_a_panic() {
        let items: Vec<u32> = (0..100).collect();
        map(&items, 4, 1, |&i| assert!(i != 99, "boom"));
    }
}
