//! The two clocks of a record: `up` (monotonic, microseconds since the
//! logger was installed) and `t` (system time). Tests replace both with
//! `ManualClock`.

use std::sync::OnceLock;
use std::time::Instant;
#[cfg(any(feature = "sink", feature = "test-support"))]
use std::time::SystemTime;

static START: OnceLock<Instant> = OnceLock::new();

/// The origin of `up`; the first call wins.
#[cfg(any(feature = "sink", feature = "test-support"))]
pub(crate) fn mark_start() {
    START.get_or_init(Instant::now);
}

/// Microseconds since [`mark_start`] (0 before it).
#[inline]
pub(crate) fn up_micros() -> u64 {
    #[cfg(all(test, feature = "sink"))]
    if let Some(up) = manual::up() {
        return up;
    }
    match START.get() {
        Some(start) => {
            // Not `as_micros()`: its u128 division costs as much as the
            // clock read on the rate limit's path.
            let up = start.elapsed();
            up.as_secs().saturating_mul(1_000_000) + u64::from(up.subsec_micros())
        }
        None => 0,
    }
}

/// The rate-limit window (`up / window_us`) for a call that may be left
/// out. Reading the monotonic clock costs more than the rest of a limited
/// call, so each thread keeps the window it last read with the CPU's
/// counter at that moment, and reads the clock again only once the counter
/// has moved 2^20 ticks (0.3 ms at 3 GHz, 44 ms at the 24 MHz of Apple
/// silicon). A window boundary is therefore seen at most that late on a
/// thread. Without a counter (other architectures) it always reads the
/// clock.
#[inline]
pub(crate) fn window(window_us: u64) -> u64 {
    const FRESH: u64 = 1 << 20;
    #[cfg(all(test, feature = "sink"))]
    if manual::up().is_some() {
        return up_micros() / window_us;
    }
    let Some(now) = ticks() else {
        return up_micros() / window_us;
    };
    let cached = WINDOW.try_with(|w| {
        let (at, window) = w.get();
        (at != 0 && now.wrapping_sub(at) < FRESH).then_some(window)
    });
    if let Ok(Some(window)) = cached {
        return window;
    }
    let window = up_micros() / window_us;
    let _ = WINDOW.try_with(|w| w.set((now, window)));
    window
}

thread_local! {
    /// (counter, window) of this thread's last clock read for [`window`].
    static WINDOW: std::cell::Cell<(u64, u64)> = const { std::cell::Cell::new((0, 0)) };
}

/// The CPU's cycle or generic-timer counter: only compared with itself on
/// the same thread, to bound how old a cached window is. A counter that
/// went backwards (another core) reads as old.
#[cfg(target_arch = "x86_64")]
#[inline]
fn ticks() -> Option<u64> {
    // SAFETY: `rdtsc` has no preconditions and touches no memory.
    Some(unsafe { std::arch::x86_64::_rdtsc() })
}

#[cfg(target_arch = "aarch64")]
#[inline]
fn ticks() -> Option<u64> {
    let v: u64;
    // SAFETY: reading the virtual counter register is allowed at EL0 on
    // macOS and Linux and has no side effects.
    unsafe {
        std::arch::asm!("mrs {}, cntvct_el0", out(reg) v, options(nomem, nostack, preserves_flags))
    };
    Some(v)
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
#[inline]
fn ticks() -> Option<u64> {
    None
}

/// The system time written as `t` and used for file names and expiry.
#[cfg(any(feature = "sink", feature = "test-support"))]
#[inline]
pub(crate) fn now() -> SystemTime {
    #[cfg(all(test, feature = "sink"))]
    if let Some(now) = manual::now() {
        return now;
    }
    SystemTime::now()
}

#[cfg(all(test, feature = "sink"))]
pub(crate) mod manual {
    //! `ManualClock`: `up` and `t` advance only when a test says so.

    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    static ON: AtomicBool = AtomicBool::new(false);
    static UP: AtomicU64 = AtomicU64::new(0);
    /// `t` at `up = 0`, microseconds since the epoch.
    static BASE: AtomicU64 = AtomicU64::new(0);

    pub(crate) fn up() -> Option<u64> {
        ON.load(Ordering::Acquire)
            .then(|| UP.load(Ordering::Acquire))
    }

    pub(crate) fn now() -> Option<SystemTime> {
        up().map(|up| UNIX_EPOCH + Duration::from_micros(BASE.load(Ordering::Acquire) + up))
    }

    /// Replaces both clocks until dropped; `up` starts at 0, `t` at `base`.
    pub(crate) struct ManualClock;

    impl ManualClock {
        pub(crate) fn install(base: SystemTime) -> ManualClock {
            let micros = base
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros();
            BASE.store(micros as u64, Ordering::Release);
            UP.store(0, Ordering::Release);
            ON.store(true, Ordering::Release);
            ManualClock
        }

        pub(crate) fn advance(&self, by: Duration) {
            UP.fetch_add(by.as_micros() as u64, Ordering::AcqRel);
        }
    }

    impl Drop for ManualClock {
        fn drop(&mut self) {
            ON.store(false, Ordering::Release);
        }
    }
}
