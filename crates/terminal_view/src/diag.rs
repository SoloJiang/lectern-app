//! What the runtime log keeps of this view (docs/design/terminal_view.md
//! 「日志与诊断」): counts and durations at the verbose level. Never the text
//! of the screen, the title, the selection, what is pasted, copied or typed,
//! the composition of an input method, the clipboard or a colour.

use std::time::{Duration, Instant};

use lectern_logging::debug;
use lectern_services::terminal::{GridSize, TerminalId};

/// At most one `terminal view frames` record each second.
const REPORT_EVERY: Duration = Duration::from_secs(1);

/// The frames drawn since the last record.
#[derive(Default)]
pub(crate) struct FrameStats {
    frames: u32,
    rows_laid_out: u32,
    glyphs: u32,
    max_prepaint: Duration,
    max_paint: Duration,
    started: Option<Instant>,
}

impl FrameStats {
    pub(crate) fn record_prepaint(&mut self, laid_out: u32, glyphs: u32, took: Duration) {
        self.frames += 1;
        self.rows_laid_out += laid_out;
        self.glyphs = self.glyphs.max(glyphs);
        self.max_prepaint = self.max_prepaint.max(took);
    }

    /// Records the paint of a frame, and writes the record when a second
    /// has passed since the first frame of the period.
    pub(crate) fn record_paint(&mut self, term: TerminalId, took: Duration) {
        self.record_paint_at(term, took, Instant::now());
    }

    /// `record_paint` at the time `now`.
    pub(crate) fn record_paint_at(&mut self, term: TerminalId, took: Duration, now: Instant) {
        self.max_paint = self.max_paint.max(took);
        let started = *self.started.get_or_insert(now);
        if now.duration_since(started) < REPORT_EVERY {
            return;
        }
        debug!("terminal view frames";
            term = term.0,
            frames = self.frames,
            rows_laid_out = self.rows_laid_out,
            glyphs = self.glyphs,
            max_prepaint_us = micros(self.max_prepaint),
            max_paint_us = micros(self.max_paint));
        *self = FrameStats {
            started: Some(now),
            ..FrameStats::default()
        };
    }
}

fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

/// The grid was resized.
pub(crate) fn resized(term: TerminalId, size: GridSize) {
    debug!("terminal view resized"; term = term.0, cols = size.cols, rows = size.rows);
}
