//! When to publish a frame (design 「帧的节奏」). Pure logic over a clock the
//! caller passes in, so the tests are deterministic.

use std::time::{Duration, Instant};

/// At most one frame per 8 ms while output continues.
pub(crate) const FRAME_INTERVAL: Duration = Duration::from_millis(8);
/// Synchronized output (mode 2026) holds frames back at most this long.
pub(crate) const SYNC_TIMEOUT: Duration = Duration::from_millis(200);

#[derive(Default, Debug)]
pub(crate) struct Pacer {
    last_frame: Option<Instant>,
    /// Output or input changed the screen since the last frame.
    pending: bool,
    /// When the program turned synchronized output on, if it is on.
    sync_since: Option<Instant>,
}

impl Pacer {
    /// The screen changed at `now`; `sync` is whether mode 2026 is on after
    /// the change.
    pub(crate) fn changed(&mut self, now: Instant, sync: bool) {
        self.pending = true;
        self.sync(now, sync);
    }

    /// Mode 2026 as seen at `now`.
    pub(crate) fn sync(&mut self, now: Instant, sync: bool) {
        match (sync, self.sync_since) {
            (true, None) => self.sync_since = Some(now),
            (false, Some(_)) => self.sync_since = None,
            _ => {}
        }
    }

    /// When the next frame is due: None when nothing is pending, `Some(None)`
    /// when it is due at once.
    pub(crate) fn due(&self) -> Option<Option<Instant>> {
        if !self.pending {
            return None;
        }
        let paced = self.last_frame.map(|last| last + FRAME_INTERVAL);
        let synced = self.sync_since.map(|since| since + SYNC_TIMEOUT);
        Some(match (paced, synced) {
            (Some(p), Some(s)) => Some(p.max(s)),
            (Some(t), None) | (None, Some(t)) => Some(t),
            (None, None) => None,
        })
    }

    /// Whether a frame should be published at `now`.
    pub(crate) fn is_due(&self, now: Instant) -> bool {
        self.due().is_some_and(|due| due.is_none_or(|at| at <= now))
    }

    /// A frame was published at `now`.
    pub(crate) fn published(&mut self, now: Instant) {
        self.pending = false;
        self.last_frame = Some(now);
        // A frame forced out by the timeout restarts the sync wait.
        if self.sync_since.is_some() {
            self.sync_since = Some(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(base: Instant, n: u64) -> Instant {
        base + Duration::from_millis(n)
    }

    #[test]
    fn pacing_first_output_after_idle_publishes_at_once() {
        let t0 = Instant::now();
        let mut p = Pacer::default();
        assert_eq!(p.due(), None);
        p.changed(t0, false);
        assert!(p.is_due(t0));
        p.published(t0);
        // Idle for a second, then one echo: no extra delay.
        p.changed(ms(t0, 1000), false);
        assert!(p.is_due(ms(t0, 1000)));
    }

    #[test]
    fn pacing_continuous_output_waits_8_ms_between_frames() {
        let t0 = Instant::now();
        let mut p = Pacer::default();
        p.changed(t0, false);
        p.published(t0);
        p.changed(ms(t0, 1), false);
        assert!(!p.is_due(ms(t0, 7)));
        assert_eq!(p.due(), Some(Some(ms(t0, 8))));
        assert!(p.is_due(ms(t0, 8)));
        p.published(ms(t0, 8));
        assert_eq!(p.due(), None);
    }

    #[test]
    fn pacing_sync_output_holds_frames_until_it_ends() {
        let t0 = Instant::now();
        let mut p = Pacer::default();
        p.changed(t0, true);
        assert!(!p.is_due(ms(t0, 100)));
        // The program turns mode 2026 off: publish at once.
        p.changed(ms(t0, 120), false);
        assert!(p.is_due(ms(t0, 120)));
    }

    #[test]
    fn pacing_sync_output_publishes_after_200_ms() {
        let t0 = Instant::now();
        let mut p = Pacer::default();
        p.changed(t0, true);
        p.changed(ms(t0, 50), true);
        assert!(!p.is_due(ms(t0, 199)));
        assert!(p.is_due(ms(t0, 200)));
        p.published(ms(t0, 200));
        p.changed(ms(t0, 210), true);
        assert!(!p.is_due(ms(t0, 399)));
        assert!(p.is_due(ms(t0, 400)));
    }
}
