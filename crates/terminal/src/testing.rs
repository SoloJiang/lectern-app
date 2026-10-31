//! Test support: the engine without a PTY (design 「验收」 step 2), and the
//! plugin in a `TestKernel` with fake projects and sessions (step 3).
//!
//! Feed it program output, send it input, take the bytes it would write to
//! the PTY, and build frames. The terminal's own tests and the benchmarks
//! use it.

use std::sync::Arc;
use std::time::Instant;

use lectern_services::terminal::{ColorScheme, Frame, GridSize, Input, Rgb, RunState, Theme};

mod harness;

pub use crate::engine::OutputEffects;
pub use harness::{FakeProjects, FakeSessions, Harness, frame_text};

/// The engine of one terminal, with no PTY and no thread.
pub struct Engine(crate::engine::Engine);

impl Engine {
    /// A running terminal (input reaches the "program", i.e. `take_pty_output`).
    pub fn new(size: GridSize, theme: Theme) -> Engine {
        let mut engine = crate::engine::Engine::new(size, theme).expect("libghostty-vt terminal");
        engine.set_state(RunState::Running { pid: 1 }, Instant::now());
        Engine(engine)
    }

    /// Program output, as if read from the PTY at `Instant::now()`.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.0.write_output(bytes, Instant::now());
    }

    /// Program output read at `now`.
    pub fn feed_at(&mut self, bytes: &[u8], now: Instant) {
        self.0.write_output(bytes, now);
    }

    pub fn input(&mut self, input: Input) {
        self.0.input(input, Instant::now()).expect("input");
    }

    pub fn set_state(&mut self, state: RunState) {
        self.0.set_state(state, Instant::now());
    }

    /// Bytes the engine would write to the PTY since the last call.
    pub fn take_pty_output(&mut self) -> Vec<u8> {
        self.0.take_pty_output()
    }

    pub fn take_effects(&mut self) -> OutputEffects {
        self.0.take_effects()
    }

    pub fn title(&self) -> Option<Arc<str>> {
        self.0.title()
    }

    pub fn copy_selection(&self) -> Option<String> {
        self.0.copy_selection()
    }

    /// A frame now, whatever the pacing.
    pub fn frame(&mut self) -> Arc<Frame> {
        self.0.frame().expect("frame")
    }

    /// Whether the pacing would publish a frame at `now`.
    pub fn frame_due(&self, now: Instant) -> bool {
        self.0.frame_due(now)
    }

    /// Publishes a frame at `now` (what the terminal thread does when it is due).
    pub fn publish(&mut self, now: Instant) -> Arc<Frame> {
        self.0.publish(now).expect("frame")
    }
}

/// A fixed dark theme for tests and benchmarks: the xterm defaults.
pub fn theme() -> Theme {
    let c = |r, g, b| Rgb { r, g, b };
    Theme {
        foreground: c(0xe5, 0xe5, 0xe5),
        background: c(0x10, 0x10, 0x10),
        cursor: c(0xff, 0xff, 0xff),
        ansi: [
            c(0x00, 0x00, 0x00),
            c(0xcd, 0x00, 0x00),
            c(0x00, 0xcd, 0x00),
            c(0xcd, 0xcd, 0x00),
            c(0x00, 0x00, 0xee),
            c(0xcd, 0x00, 0xcd),
            c(0x00, 0xcd, 0xcd),
            c(0xe5, 0xe5, 0xe5),
            c(0x7f, 0x7f, 0x7f),
            c(0xff, 0x00, 0x00),
            c(0x00, 0xff, 0x00),
            c(0xff, 0xff, 0x00),
            c(0x5c, 0x5c, 0xff),
            c(0xff, 0x00, 0xff),
            c(0x00, 0xff, 0xff),
            c(0xff, 0xff, 0xff),
        ],
        scheme: ColorScheme::Dark,
    }
}

pub fn size(cols: u16, rows: u16) -> GridSize {
    GridSize {
        cols,
        rows,
        cell_width: 8,
        cell_height: 16,
    }
}

/// About `bytes` of plain ASCII output, lines of `cols - 1` printable
/// characters and CRLF (`terminal/parse_plain`).
pub fn plain_output(bytes: usize, cols: u16) -> Vec<u8> {
    let width = usize::from(cols.max(2) - 1);
    let mut out = Vec::with_capacity(bytes + width);
    let mut n = 0u32;
    while out.len() < bytes {
        for i in 0..width {
            out.push(b'!' + ((n as usize + i) % 94) as u8);
        }
        out.extend_from_slice(b"\r\n");
        n = n.wrapping_add(1);
    }
    out
}

/// About `bytes` of output that changes to a new 24-bit foreground colour
/// every 8 characters, in lines of `cols - 1` characters
/// (`terminal/parse_sgr`, `terminal/scrollback_footprint`).
pub fn sgr_output(bytes: usize, cols: u16) -> Vec<u8> {
    let width = usize::from(cols.max(2) - 1);
    let mut out = Vec::with_capacity(bytes + 4 * width);
    let mut n = 0u32;
    while out.len() < bytes {
        let mut col = 0;
        while col < width {
            let (r, g, b) = (n % 251, (n * 7) % 253, (n * 13) % 255);
            out.extend_from_slice(format!("\x1b[38;2;{r};{g};{b}m").as_bytes());
            for i in 0..8.min(width - col) {
                out.push(b'a' + ((col + i) % 26) as u8);
            }
            col += 8;
            n = n.wrapping_add(1);
        }
        out.extend_from_slice(b"\x1b[0m\r\n");
    }
    out
}

/// `lines` lines of `cols - 1` characters in 24-bit colour
/// (`terminal/scrollback_footprint`).
pub fn colored_lines(lines: usize, cols: u16) -> Vec<u8> {
    let one = sgr_output(1, cols);
    let mut out = Vec::with_capacity(one.len() * lines);
    for _ in 0..lines {
        out.extend_from_slice(&one);
    }
    out
}

/// A screen of `rows` full lines, each a different colour, drawn from the
/// top left: every row changes (`terminal/frame_full`). `round` varies the
/// text so two rounds never draw the same screen.
pub fn full_screen(cols: u16, rows: u16, round: usize) -> Vec<u8> {
    let width = usize::from(cols);
    let mut out = b"\x1b[H".to_vec();
    for row in 0..usize::from(rows) {
        let n = (row + round) % 216;
        out.extend_from_slice(format!("\x1b[38;5;{}m", 16 + n).as_bytes());
        for i in 0..width {
            out.push(b'A' + ((row + i + round) % 26) as u8);
        }
        if row + 1 < usize::from(rows) {
            out.extend_from_slice(b"\r\n");
        }
    }
    out.extend_from_slice(b"\x1b[0m");
    out
}
