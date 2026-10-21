//! Small helpers shared by the modules.

use std::fmt;

/// One line on stderr with the crate's prefix.
pub(crate) fn log(args: fmt::Arguments<'_>) {
    eprintln!("lectern-syntax: {args}");
}

/// 64-bit FNV-1a over the concatenation of `parts`, the same function as
/// `lectern-code-intel`'s grammar and query versions. Stable across builds
/// and toolchains: the versions go into cache keys.
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
