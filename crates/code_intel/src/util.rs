//! Small helpers.

use std::fmt;

/// One line on stderr with the crate's prefix.
pub(crate) fn log(args: fmt::Arguments<'_>) {
    eprintln!("lectern-code-intel: {args}");
}

/// 64-bit FNV-1a over the concatenation of `parts`. Stable across builds
/// and toolchains: grammar and query versions are stored in cache keys.
pub(crate) fn fnv64(parts: &[&[u8]]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for &byte in *part {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

/// A fast, non-keyed hasher for short in-memory keys such as identifier
/// names (the multiply-rotate of rustc's FxHasher). Not for persisted keys
/// (use [`fnv64`]) or input an attacker could shape into collisions at scale.
#[derive(Default, Clone, Copy)]
pub(crate) struct FastHasher(u64);

impl std::hash::Hasher for FastHasher {
    fn write(&mut self, bytes: &[u8]) {
        const K: u64 = 0x517c_c1b7_2722_0a95;
        let mut chunks = bytes.chunks_exact(8);
        for chunk in &mut chunks {
            let word = u64::from_le_bytes(chunk.try_into().unwrap_or([0; 8]));
            self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(K);
        }
        let rest = chunks.remainder();
        if !rest.is_empty() {
            let mut word = [0u8; 8];
            word[..rest.len()].copy_from_slice(rest);
            self.0 = (self.0.rotate_left(5) ^ u64::from_le_bytes(word)).wrapping_mul(K);
        }
    }

    fn write_usize(&mut self, n: usize) {
        self.0 = (self.0.rotate_left(5) ^ n as u64).wrapping_mul(0x517c_c1b7_2722_0a95);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

pub(crate) type FastBuild = std::hash::BuildHasherDefault<FastHasher>;

/// Byte offsets of line starts, for turning byte offsets into rows.
pub(crate) struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    pub(crate) fn new(source: &[u8]) -> LineIndex {
        let mut starts = vec![0];
        starts.extend(
            source
                .iter()
                .enumerate()
                .filter(|(_, b)| **b == b'\n')
                .map(|(i, _)| i + 1),
        );
        LineIndex { starts }
    }

    /// The row of a byte offset.
    pub(crate) fn row(&self, offset: usize) -> u32 {
        let row = self.starts.partition_point(|&s| s <= offset) - 1;
        u32::try_from(row).unwrap_or(u32::MAX)
    }

    /// The byte offset of a row and byte column; None past the end of the
    /// row or of the text.
    pub(crate) fn offset(&self, row: u32, column: u32, len: usize) -> Option<usize> {
        let start = *self.starts.get(row as usize)?;
        let end = self.starts.get(row as usize + 1).map_or(len, |s| s - 1);
        let offset = start + column as usize;
        (offset <= end).then_some(offset)
    }

    /// The row of the last byte of `start..end` (of `start` when empty).
    pub(crate) fn last_row(&self, start: usize, end: usize) -> u32 {
        self.row(end.saturating_sub(1).max(start))
    }
}
