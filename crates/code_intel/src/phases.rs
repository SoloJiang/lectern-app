//! Per-phase timers and counters of the query path, for the breakdown
//! benchmark `code_intel/goto_indexed_phases` (docs/design/code_intel.md
//! 「实际验证」). Only with `test-support`, and only while [`enable`]d; in
//! a production build every call is empty and compiles away.

#[cfg(any(test, feature = "test-support"))]
mod imp {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    static ENABLED: AtomicBool = AtomicBool::new(false);
    static LOG: Mutex<Log> = Mutex::new(Log {
        phases: Vec::new(),
        counts: Vec::new(),
    });

    /// What was recorded since the last [`take`]: per phase its total time,
    /// per counter its total, in first-seen order.
    #[derive(Clone, Default, Debug)]
    pub struct Log {
        pub phases: Vec<(&'static str, Duration)>,
        pub counts: Vec<(&'static str, u64)>,
    }

    /// Starts or stops recording (benchmarks only).
    pub fn enable(on: bool) {
        ENABLED.store(on, Ordering::Release);
    }

    /// Everything recorded so far, cleared.
    pub fn take() -> Log {
        std::mem::take(
            &mut *LOG
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    fn add<T: Copy + std::ops::AddAssign>(
        list: &mut Vec<(&'static str, T)>,
        name: &'static str,
        v: T,
    ) {
        match list.iter_mut().find(|(n, _)| *n == name) {
            Some((_, total)) => *total += v,
            None => list.push((name, v)),
        }
    }

    /// A phase: its time from now until the guard drops.
    pub struct Span(Option<(&'static str, Instant)>);

    pub fn span(name: &'static str) -> Span {
        Span(
            ENABLED
                .load(Ordering::Acquire)
                .then(|| (name, Instant::now())),
        )
    }

    impl Drop for Span {
        fn drop(&mut self) {
            if let Some((name, start)) = self.0.take() {
                let took = start.elapsed();
                let mut log = LOG
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                add(&mut log.phases, name, took);
            }
        }
    }

    /// Adds `n` to counter `name`.
    pub fn count(name: &'static str, n: u64) {
        if ENABLED.load(Ordering::Acquire) {
            let mut log = LOG
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            add(&mut log.counts, name, n);
        }
    }
}

#[cfg(not(any(test, feature = "test-support")))]
mod imp {
    pub struct Span;

    #[inline(always)]
    pub fn span(_: &'static str) -> Span {
        Span
    }

    #[inline(always)]
    pub fn count(_: &'static str, _: u64) {}
}

pub use imp::*;
