//! The bounded buffers between the calling threads and the writer
//! (docs/design/logging.md「缓冲」). A record is framed as a 4-byte header
//! (`level << 24 | length`, little endian) and the encoded line.

use log::Level;

/// Before the writer starts: one allocation, kept until the writer has
/// written it out.
pub(crate) const EARLY_CAP: usize = 64 << 10;
/// Each of the two running buffers.
pub(crate) const RUN_CAP: usize = 128 << 10;
/// Each of the two running buffers while verbose logging is on.
pub(crate) const VERBOSE_CAP: usize = 512 << 10;
/// A buffer this full wakes the writer at once.
pub(crate) const URGENT_BYTES: usize = 32 << 10;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Phase {
    Early,
    Running,
    Closed,
}

pub(crate) struct Buffers {
    pub(crate) phase: Phase,
    pub(crate) early: Vec<u8>,
    /// The running buffer callers copy into; the writer swaps it with its own.
    pub(crate) front: Vec<u8>,
    /// The running buffers' capacity.
    pub(crate) cap: usize,
    /// An error record or [`URGENT_BYTES`] arrived since the last swap.
    pub(crate) urgent: bool,
}

impl Buffers {
    pub(crate) fn new() -> Buffers {
        Buffers {
            phase: Phase::Early,
            early: Vec::with_capacity(EARLY_CAP),
            front: Vec::new(),
            cap: RUN_CAP,
            urgent: false,
        }
    }

    /// Appends a record; false when it does not fit (the record is dropped)
    /// or the buffers are closed. Never allocates: the buffer's capacity is
    /// reserved and the record must fit in it.
    pub(crate) fn push(&mut self, level: Level, line: &[u8]) -> bool {
        let (buf, cap) = match self.phase {
            Phase::Early => (&mut self.early, EARLY_CAP),
            Phase::Running => {
                let cap = self.cap.min(self.front.capacity());
                (&mut self.front, cap)
            }
            Phase::Closed => return false,
        };
        if buf.len() + 4 + line.len() > cap {
            return false;
        }
        let header = (level as u32) << 24 | line.len() as u32;
        buf.extend_from_slice(&header.to_le_bytes());
        buf.extend_from_slice(line);
        if level == Level::Error || (self.phase == Phase::Running && buf.len() >= URGENT_BYTES) {
            self.urgent = true;
        }
        true
    }

    /// Sets the running capacity; a larger one is reserved at once.
    pub(crate) fn set_cap(&mut self, cap: usize) {
        self.cap = cap;
        if self.phase == Phase::Running && self.front.capacity() < cap {
            self.front.reserve_exact(cap - self.front.len());
        }
    }
}

/// The records of a framed buffer.
pub(crate) fn frames(mut bytes: &[u8]) -> impl Iterator<Item = (Level, &[u8])> {
    std::iter::from_fn(move || {
        let header = u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?);
        let len = (header & 0x00ff_ffff) as usize;
        let level = match header >> 24 {
            1 => Level::Error,
            2 => Level::Warn,
            3 => Level::Info,
            4 => Level::Debug,
            _ => Level::Trace,
        };
        let line = bytes.get(4..4 + len)?;
        bytes = &bytes[4 + len..];
        Some((level, line))
    })
}
