//! The time source of the keepalive slots (`docs/design/kernel.md`, 静止;
//! ADR 0033).
//!
//! The interaction yield needs to know the time (a slot lasts 50 ms, the next
//! one starts 500 ms after the previous start) and to be woken when the next
//! slot may start. Tests replace the clock with `testing::ManualClock`.

use std::sync::{Condvar, Mutex};
use std::task::Waker;
use std::thread;
use std::time::{Duration, Instant};

use lectern_logging::{Value, warn};

use crate::util::lock;

/// A monotonic clock that can wake a task at a given instant.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
    /// Wakes `waker` once `now()` has reached `at`; at once if it already has.
    /// Must not call `waker` while holding a lock the woken code may take.
    fn wake_at(&self, at: Instant, waker: Waker);
}

/// The system's monotonic clock. Its timer thread starts on the first
/// `wake_at` and exits after [`TIMER_LINGER`] without timers, so nothing runs
/// while the yield switch is off.
#[derive(Clone, Copy, Default, Debug)]
pub struct SystemClock;

/// How long the timer thread waits for a new timer before it exits.
const TIMER_LINGER: Duration = Duration::from_secs(1);

const TIMER_THREAD: &str = "lectern-kernel-timer";

struct TimerState {
    due: Vec<(Instant, Waker)>,
    running: bool,
}

struct Timer {
    state: Mutex<TimerState>,
    changed: Condvar,
}

static TIMER: Timer = Timer {
    state: Mutex::new(TimerState {
        due: Vec::new(),
        running: false,
    }),
    changed: Condvar::new(),
};

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn wake_at(&self, at: Instant, waker: Waker) {
        if at <= Instant::now() {
            waker.wake();
            return;
        }
        let mut state = lock(&TIMER.state);
        state.due.push((at, waker));
        if state.running {
            TIMER.changed.notify_one();
            return;
        }
        state.running = true;
        if let Err(error) = thread::Builder::new()
            .name(TIMER_THREAD.into())
            .spawn(run_timer)
        {
            // Not woken: the waiting work continues when the switch turns
            // off. Waking now would re-arm the timer from inside this call.
            state.running = false;
            state.due.clear();
            warn!("cannot start the timer thread"; error = Value::io(&error));
        }
    }
}

fn run_timer() {
    let mut state = lock(&TIMER.state);
    loop {
        let now = Instant::now();
        let mut fire = Vec::new();
        state.due.retain(|(at, waker)| {
            if *at <= now {
                fire.push(waker.clone());
                false
            } else {
                true
            }
        });
        if !fire.is_empty() {
            drop(state);
            for waker in fire {
                waker.wake();
            }
            state = lock(&TIMER.state);
            continue;
        }
        let next = state.due.iter().map(|(at, _)| *at).min();
        let wait = next.map_or(TIMER_LINGER, |at| at - now);
        let (guard, result) = TIMER
            .changed
            .wait_timeout(state, wait)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state = guard;
        if next.is_none() && result.timed_out() && state.due.is_empty() {
            state.running = false;
            return;
        }
    }
}
