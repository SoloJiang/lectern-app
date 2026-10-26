//! The pairing of a recording's per-frame moments (「逐帧记录」 in
//! docs/design/platform.md): the part of `frame::record` that needs no
//! Objective-C, so that it runs and is tested on every platform.
//!
//! A [`Store`] is allocated once and never freed: GPU and display handlers
//! can arrive after a recording stopped, and their blocks capture only the
//! generation and the slot number, so the memory they write must stay valid.
//! The recording thread (the main thread, or a test's thread) calls
//! `frame_started`, `drawable` and `commit`; Metal, Core Animation and
//! CoreVideo threads call `completed`, `presented` and `tick`. Every method
//! here only loads and stores atomics: no lock, no allocation, no panic.
//! `collect` runs once, after `stop`, and turns the slots into
//! [`FrameTimes`] with one [`Clock`] reading.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use super::{FrameProblems, FrameRecording, FrameTimes, PresentedAt, TimeSource, system_time_of};

#[cfg(test)]
mod tests;

/// Frames one recording can hold (「存储与生命周期」).
pub(super) const SLOTS: usize = 8192;
/// Display link ticks one recording keeps, the latest first to stay.
pub(super) const TICKS: usize = 16384;

/// `Slot::presented_source`: no presented handler yet.
const NOT_YET: u8 = 0;
const PRESENTED: u8 = 1;
const HANDLER: u8 = 2;

/// One frame. `generation` is written last when a frame takes the slot, so
/// a handler that finds its own generation there writes into its own frame.
#[derive(Default)]
struct Slot {
    generation: AtomicU32,
    /// `frame_started` of this frame, in clock ticks.
    started: AtomicU64,
    /// The `frame_started` before it (or the start of the recording).
    previous: AtomicU64,
    /// `GPUEndTime`, seconds on the media clock, as `f64` bits; 0 until the
    /// completed handler ran with a time.
    completed: AtomicU64,
    /// The presented handler's time, seconds on the media clock, as bits.
    presented: AtomicU64,
    /// `NOT_YET`, `PRESENTED` or `HANDLER`; stored after `presented`.
    presented_source: AtomicU8,
}

/// The memory of every recording of the process.
pub(super) struct Store {
    /// The current recording's generation; 0 before the first one, so that
    /// a slot never taken (generation 0) matches no recording.
    generation: AtomicU32,
    /// A recorder exists (from `begin` to `stop`).
    busy: AtomicBool,
    /// The hooks record (from `begin` to `stop`).
    active: AtomicBool,
    // The recording thread's state of the current recording.
    /// This frame handling's `frame_started`; 0 before the first one.
    current: AtomicU64,
    /// The one before it.
    previous: AtomicU64,
    /// This frame handling already took a drawable.
    drawn: AtomicBool,
    /// The slot (plus one) whose command buffer is the next `commit`; 0
    /// for none.
    pending_commit: AtomicU32,
    next_slot: AtomicU32,
    unmatched: AtomicU32,
    overflowed: AtomicU32,
    slots: Box<[Slot]>,
    ticks: Box<[AtomicU64]>,
    /// Ticks written in this recording; the latest `TICKS` of them are kept.
    tick_count: AtomicU64,
}

/// What a recording took when it stopped, for `settled` and `collect`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Taken {
    pub(super) generation: u32,
    /// Slots used, at most `SLOTS`.
    pub(super) frames: u32,
}

impl Store {
    pub(super) fn new() -> Store {
        Store {
            generation: AtomicU32::new(0),
            busy: AtomicBool::new(false),
            active: AtomicBool::new(false),
            current: AtomicU64::new(0),
            previous: AtomicU64::new(0),
            drawn: AtomicBool::new(false),
            pending_commit: AtomicU32::new(0),
            next_slot: AtomicU32::new(0),
            unmatched: AtomicU32::new(0),
            overflowed: AtomicU32::new(0),
            slots: (0..SLOTS).map(|_| Slot::default()).collect(),
            ticks: (0..TICKS).map(|_| AtomicU64::new(0)).collect(),
            tick_count: AtomicU64::new(0),
        }
    }

    /// Claims the store for one recorder: false when one exists.
    pub(super) fn claim(&self) -> bool {
        self.busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Gives up a claim that did not become a recording.
    pub(super) fn release(&self) {
        self.busy.store(false, Ordering::Release);
    }

    /// Starts a recording at `now` (clock ticks), after a `claim`: the first
    /// frame's "previous" start is this moment. Returns the generation.
    pub(super) fn begin(&self, now: u64) -> u32 {
        self.current.store(0, Ordering::Relaxed);
        self.previous.store(now, Ordering::Relaxed);
        self.drawn.store(false, Ordering::Relaxed);
        self.pending_commit.store(0, Ordering::Relaxed);
        self.next_slot.store(0, Ordering::Relaxed);
        self.unmatched.store(0, Ordering::Relaxed);
        self.overflowed.store(0, Ordering::Relaxed);
        self.tick_count.store(0, Ordering::Relaxed);
        let generation = self
            .generation
            .load(Ordering::Relaxed)
            .wrapping_add(1)
            .max(1);
        self.generation.store(generation, Ordering::Release);
        self.active.store(true, Ordering::Release);
        generation
    }

    /// The current recording's generation, for the handlers' blocks.
    pub(super) fn generation(&self) -> u32 {
        self.generation.load(Ordering::Acquire)
    }

    /// Whether the hooks record now.
    pub(super) fn active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    /// The recording thread: a frame handling starts at `now`.
    pub(super) fn frame_started(&self, now: u64) {
        let current = self.current.load(Ordering::Relaxed);
        if current != 0 {
            self.previous.store(current, Ordering::Relaxed);
        }
        self.current.store(now, Ordering::Relaxed);
        self.drawn.store(false, Ordering::Relaxed);
    }

    /// The recording thread took a drawable: the slot of its frame, or None
    /// when it belongs to no frame handling (`unmatched`) or the slots are
    /// used up (`overflowed`). The slot's command buffer is the next
    /// `commit`.
    pub(super) fn drawable(&self) -> Option<u32> {
        let current = self.current.load(Ordering::Relaxed);
        if current == 0 || self.drawn.swap(true, Ordering::Relaxed) {
            self.unmatched.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        let index = self.next_slot.fetch_add(1, Ordering::Relaxed);
        let Some(slot) = self.slots.get(index as usize) else {
            self.overflowed.fetch_add(1, Ordering::Relaxed);
            // Keeps `next_slot` from wrapping on a recording that never ends.
            self.next_slot.store(SLOTS as u32, Ordering::Relaxed);
            return None;
        };
        // A handler of an older recording that still holds this slot finds
        // no generation of its own from here on.
        slot.generation.store(0, Ordering::Release);
        slot.started.store(current, Ordering::Relaxed);
        slot.previous
            .store(self.previous.load(Ordering::Relaxed), Ordering::Relaxed);
        slot.completed.store(0, Ordering::Relaxed);
        slot.presented.store(0, Ordering::Relaxed);
        slot.presented_source.store(NOT_YET, Ordering::Relaxed);
        slot.generation
            .store(self.generation.load(Ordering::Relaxed), Ordering::Release);
        self.pending_commit.store(index + 1, Ordering::Relaxed);
        Some(index)
    }

    /// The recording thread commits a command buffer: the slot it belongs
    /// to, if it is the first commit after the slot's drawable.
    pub(super) fn commit(&self) -> Option<u32> {
        self.pending_commit
            .swap(0, Ordering::Relaxed)
            .checked_sub(1)
    }

    /// The slot of `generation` and `index`, if a frame of that recording
    /// holds it.
    fn slot(&self, generation: u32, index: u32) -> Option<&Slot> {
        let slot = self.slots.get(index as usize)?;
        (slot.generation.load(Ordering::Acquire) == generation).then_some(slot)
    }

    /// A completed handler: the command buffer's `GPUEndTime`. A time not
    /// greater than 0 (the buffer failed) leaves the frame incomplete.
    pub(super) fn completed(&self, generation: u32, index: u32, gpu_end: f64) {
        if gpu_end.is_nan() || gpu_end <= 0.0 {
            return;
        }
        if let Some(slot) = self.slot(generation, index) {
            slot.completed.store(gpu_end.to_bits(), Ordering::Release);
        }
    }

    /// A presented handler: `presentedTime`, and the media clock when it
    /// ran. The rule of the first frame (ADR 0052): a `presentedTime`
    /// greater than 0 is the time; otherwise the handler's moment is.
    pub(super) fn presented(&self, generation: u32, index: u32, presented: f64, handler_now: f64) {
        let Some(slot) = self.slot(generation, index) else {
            return;
        };
        let (time, source) = if presented > 0.0 {
            (presented, PRESENTED)
        } else {
            (handler_now, HANDLER)
        };
        slot.presented.store(time.to_bits(), Ordering::Relaxed);
        slot.presented_source.store(source, Ordering::Release);
    }

    /// A display link tick at `host` (clock ticks), while recording.
    pub(super) fn tick(&self, host: u64) {
        if !self.active() {
            return;
        }
        let n = self.tick_count.load(Ordering::Relaxed);
        self.ticks[(n % TICKS as u64) as usize].store(host, Ordering::Relaxed);
        self.tick_count.store(n + 1, Ordering::Release);
    }

    /// Ends the recording: later frames are not recorded, and the store can
    /// be claimed again. Returns what it took.
    pub(super) fn stop(&self) -> Taken {
        self.active.store(false, Ordering::Release);
        let taken = Taken {
            generation: self.generation.load(Ordering::Acquire),
            frames: self.next_slot.load(Ordering::Relaxed).min(SLOTS as u32),
        };
        self.busy.store(false, Ordering::Release);
        taken
    }

    fn slots_of(&self, taken: Taken) -> &[Slot] {
        &self.slots[..taken.frames as usize]
    }

    /// Every frame of the recording has its completion time and its
    /// presented handler.
    pub(super) fn settled(&self, taken: Taken) -> bool {
        self.slots_of(taken).iter().all(|slot| {
            slot.generation.load(Ordering::Acquire) == taken.generation
                && slot.completed.load(Ordering::Acquire) != 0
                && slot.presented_source.load(Ordering::Acquire) != NOT_YET
        })
    }

    /// The recording's frames on the system clock. A frame without a
    /// completion time, or whose slot a later recording took, is
    /// `incomplete` and left out.
    pub(super) fn collect(&self, taken: Taken, refresh: Duration, clock: &Clock) -> FrameRecording {
        let count = self.tick_count.load(Ordering::Acquire);
        let ticks: Vec<u64> = (count.saturating_sub(TICKS as u64)..count)
            .map(|n| self.ticks[(n % TICKS as u64) as usize].load(Ordering::Relaxed))
            .collect();
        let mut problems = FrameProblems {
            unmatched: self.unmatched.load(Ordering::Relaxed),
            overflowed: self.overflowed.load(Ordering::Relaxed),
            ..FrameProblems::default()
        };
        let mut frames = Vec::with_capacity(taken.frames as usize);
        for slot in self.slots_of(taken) {
            let completed = f64::from_bits(slot.completed.load(Ordering::Acquire));
            if slot.generation.load(Ordering::Acquire) != taken.generation || completed == 0.0 {
                problems.incomplete += 1;
                continue;
            }
            let started = slot.started.load(Ordering::Relaxed);
            let previous = slot.previous.load(Ordering::Relaxed);
            // The first tick after the previous start; the ticks are in the
            // order they came.
            let first = ticks.partition_point(|&tick| tick <= previous);
            let requested = match ticks.get(first) {
                Some(&tick) if tick <= started => tick,
                _ => {
                    problems.without_tick += 1;
                    started
                }
            };
            let presented = match slot.presented_source.load(Ordering::Acquire) {
                PRESENTED => Some(TimeSource::Presented),
                HANDLER => Some(TimeSource::Handler),
                _ => None,
            }
            .map(|source| PresentedAt {
                time: clock.time_of_seconds(f64::from_bits(slot.presented.load(Ordering::Relaxed))),
                source,
            });
            frames.push(FrameTimes {
                requested: clock.time_of_ticks(requested),
                started: clock.time_of_ticks(started),
                completed: clock.time_of_seconds(completed),
                presented,
            });
        }
        FrameRecording {
            refresh,
            frames,
            problems,
        }
    }
}

/// One reading of the tick clock (`mach_absolute_time`) and the system
/// clock, back to back, with the timebase that turns ticks into
/// nanoseconds (「时钟」). Media clock seconds (`CACurrentMediaTime`,
/// `presentedTime`, `GPUEndTime`) are the same clock in seconds.
#[derive(Clone, Copy, Debug)]
pub(super) struct Clock {
    numer: u32,
    denom: u32,
    now_ticks: u64,
    now: SystemTime,
}

impl Clock {
    /// A timebase of `numer / denom` nanoseconds per tick (`denom` 0 is
    /// taken as 1).
    pub(super) fn new(numer: u32, denom: u32, now_ticks: u64, now: SystemTime) -> Clock {
        Clock {
            numer,
            denom: denom.max(1),
            now_ticks,
            now,
        }
    }

    /// Ticks in nanoseconds.
    pub(super) fn nanos(&self, ticks: u64) -> u128 {
        u128::from(ticks) * u128::from(self.numer) / u128::from(self.denom)
    }

    /// The system time of a moment in ticks. Never panics: a span the
    /// system clock cannot represent gives `now`.
    pub(super) fn time_of_ticks(&self, ticks: u64) -> SystemTime {
        let now = self.nanos(self.now_ticks);
        let then = self.nanos(ticks);
        let span = |nanos: u128| {
            u64::try_from(nanos)
                .map(Duration::from_nanos)
                .unwrap_or(Duration::MAX)
        };
        let shifted = if then <= now {
            self.now.checked_sub(span(now - then))
        } else {
            self.now.checked_add(span(then - now))
        };
        shifted.unwrap_or(self.now)
    }

    /// The system time of a moment in media clock seconds.
    pub(super) fn time_of_seconds(&self, seconds: f64) -> SystemTime {
        let now = self.nanos(self.now_ticks) as f64 / 1e9;
        system_time_of(seconds, now, self.now)
    }
}
