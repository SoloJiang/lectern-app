//! The per-file deadline watchdog (docs/design/code_intel.md「调度、批次与取消」).
//!
//! tree-sitter-tags takes a single cancellation flag, so deadlines, the
//! caller's cancellation, the kernel's pause switch and deactivation all reach
//! an extraction through that flag: each extraction in flight registers its
//! own flag with the watchdog, whose thread checks every 10 ms and sets it.
//! With nothing in flight the thread waits without a timeout.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// The name of the watchdog's dedicated thread.
pub const WATCHDOG_THREAD: &str = "code-intel-watchdog";

/// How often the watchdog checks the extractions in flight.
pub const CHECK_INTERVAL: Duration = Duration::from_millis(10);

/// The time source of deadlines. The watchdog and the extractions read the
/// same clock; tests use `testing::ManualClock`.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

/// The system's monotonic clock.
#[derive(Clone, Copy, Default, Debug)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Asked at every check whether the kernel requests a pause
/// (`BackgroundGuard::pause_requested`); must not block.
pub type PauseProbe = Arc<dyn Fn() -> bool + Send + Sync>;

/// Starts the watchdog's thread: called once, on the first registration,
/// with the loop to run there. The plugin starts it through
/// `PluginHandle::own_with` and `Executor::run_dedicated_gated`.
pub type Starter = Box<dyn FnOnce(WatchdogRun) + Send>;

/// Why a registered flag was set.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Trip {
    Deadline = 1,
    Cancelled = 2,
    Paused = 3,
    Stopped = 4,
}

/// One extraction in flight: the flag tree-sitter-tags reads, and what sets it.
pub(crate) struct Slot {
    flag: AtomicUsize,
    trip: AtomicU8,
    at: Instant,
    cancel: Option<Arc<AtomicBool>>,
    pause: Option<PauseProbe>,
}

impl Slot {
    /// The flag to pass to tree-sitter-tags.
    pub(crate) fn flag(&self) -> &AtomicUsize {
        &self.flag
    }

    /// Why the flag was set; None while it is clear.
    pub(crate) fn tripped(&self) -> Option<Trip> {
        match self.trip.load(Ordering::Acquire) {
            1 => Some(Trip::Deadline),
            2 => Some(Trip::Cancelled),
            3 => Some(Trip::Paused),
            4 => Some(Trip::Stopped),
            _ => None,
        }
    }

    /// Sets the flag; the first reason wins.
    fn trip(&self, why: Trip) {
        if self
            .trip
            .compare_exchange(0, why as u8, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.flag.store(1, Ordering::SeqCst);
        }
    }

    fn check(&self, now: Instant) {
        if self
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Acquire))
        {
            self.trip(Trip::Cancelled);
        } else if self.pause.as_ref().is_some_and(|p| p()) {
            self.trip(Trip::Paused);
        } else if now >= self.at {
            self.trip(Trip::Deadline);
        }
    }
}

struct State {
    stop: bool,
    next_id: u64,
    in_flight: Vec<(u64, Arc<Slot>)>,
    starter: Option<Starter>,
}

struct Shared {
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
    wake: Condvar,
    /// Loop iterations of the thread, for the tests.
    rounds: AtomicU64,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // The state stays consistent under a panic: every update is one step.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The deadline watchdog. `Clone` shares it. One per plugin activation: the
/// thread starts on the first registration and exits on [`stop`](Self::stop).
#[derive(Clone)]
pub struct Watchdog {
    shared: Arc<Shared>,
}

impl Watchdog {
    pub fn new(clock: Arc<dyn Clock>, starter: Starter) -> Watchdog {
        Watchdog {
            shared: Arc::new(Shared {
                clock,
                state: Mutex::new(State {
                    stop: false,
                    next_id: 0,
                    in_flight: Vec::new(),
                    starter: Some(starter),
                }),
                wake: Condvar::new(),
                rounds: AtomicU64::new(0),
            }),
        }
    }

    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.shared.clock
    }

    /// Stops the watchdog: sets the flag of every extraction in flight (they
    /// return `Cancelled`) and wakes the thread, which returns within one
    /// wake-up. Later registrations are cancelled at once. Idempotent.
    pub fn stop(&self) {
        let mut state = self.shared.lock();
        state.stop = true;
        state.starter = None;
        for (_, slot) in &state.in_flight {
            slot.trip(Trip::Stopped);
        }
        drop(state);
        self.shared.wake.notify_all();
    }

    /// A guard whose drop calls [`stop`](Self::stop). The plugin hands it to
    /// `PluginHandle::own`, so every deactivation stops this activation's
    /// watchdog.
    pub fn stop_guard(&self) -> WatchdogStop {
        WatchdogStop(self.clone())
    }

    /// Whether [`stop`](Self::stop) was called.
    pub fn is_stopped(&self) -> bool {
        self.shared.lock().stop
    }

    /// Iterations of the thread's loop so far (tests: no wake-ups while
    /// nothing is in flight).
    pub fn rounds(&self) -> u64 {
        self.shared.rounds.load(Ordering::Acquire)
    }

    /// The number of extractions in flight.
    pub fn in_flight(&self) -> usize {
        self.shared.lock().in_flight.len()
    }

    /// Registers an extraction: its flag is set when `at` passes, `cancel` is
    /// set, `pause` says so, or the watchdog stops. Starts the thread on the
    /// first registration. Deregistered on drop.
    pub(crate) fn register(
        &self,
        at: Instant,
        cancel: Option<Arc<AtomicBool>>,
        pause: Option<PauseProbe>,
    ) -> Registration {
        let slot = Arc::new(Slot {
            flag: AtomicUsize::new(0),
            trip: AtomicU8::new(0),
            at,
            cancel,
            pause,
        });
        let mut state = self.shared.lock();
        if state.stop {
            slot.trip(Trip::Stopped);
            return Registration {
                shared: None,
                id: 0,
                slot,
            };
        }
        let id = state.next_id;
        state.next_id += 1;
        state.in_flight.push((id, slot.clone()));
        let starter = state.starter.take();
        drop(state);
        self.shared.wake.notify_all();
        if let Some(starter) = starter {
            starter(WatchdogRun {
                shared: self.shared.clone(),
            });
        }
        Registration {
            shared: Some(self.shared.clone()),
            id,
            slot,
        }
    }
}

impl fmt::Debug for Watchdog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.shared.lock();
        f.debug_struct("Watchdog")
            .field("stop", &state.stop)
            .field("in_flight", &state.in_flight.len())
            .finish()
    }
}

/// The watchdog's thread body, handed to the [`Starter`].
pub struct WatchdogRun {
    shared: Arc<Shared>,
}

impl WatchdogRun {
    /// The watchdog this loop belongs to: the starter registers its
    /// [`stop`](Watchdog::stop) with `Executor::on_shutdown`.
    pub fn watchdog(&self) -> Watchdog {
        Watchdog {
            shared: self.shared.clone(),
        }
    }

    /// Runs until the watchdog stops: with extractions in flight, checks them
    /// every [`CHECK_INTERVAL`]; otherwise waits without a timeout.
    pub fn run(self) {
        let shared = &*self.shared;
        let mut state = shared.lock();
        loop {
            if state.stop {
                return;
            }
            if state.in_flight.is_empty() {
                state = shared
                    .wake
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            } else {
                let now = shared.clock.now();
                for (_, slot) in &state.in_flight {
                    slot.check(now);
                }
                state = shared
                    .wake
                    .wait_timeout(state, CHECK_INTERVAL)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
            shared.rounds.fetch_add(1, Ordering::AcqRel);
        }
    }
}

impl fmt::Debug for WatchdogRun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WatchdogRun")
    }
}

/// Stops the watchdog when dropped; see [`Watchdog::stop_guard`].
pub struct WatchdogStop(Watchdog);

impl Drop for WatchdogStop {
    fn drop(&mut self) {
        self.0.stop();
    }
}

impl fmt::Debug for WatchdogStop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WatchdogStop")
    }
}

/// An extraction's registration; deregisters on drop.
pub(crate) struct Registration {
    shared: Option<Arc<Shared>>,
    id: u64,
    slot: Arc<Slot>,
}

impl Registration {
    pub(crate) fn slot(&self) -> &Slot {
        &self.slot
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Some(shared) = &self.shared {
            let mut state = shared.lock();
            state.in_flight.retain(|(id, _)| *id != self.id);
        }
    }
}
