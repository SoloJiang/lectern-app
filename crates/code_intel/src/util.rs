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
