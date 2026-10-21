//! OS-specific capabilities. Today: reading files beneath a directory root
//! without following symbolic links, in full or by byte range, and replacing
//! one atomically ([`safe_read`], docs/design/platform.md),
//! the physical footprint of a process for lectern-bench ([`process`]),
//! the CPU time of a thread ([`ThreadCpuClock`]),
//! and, on macOS, the FSEvents and kqueue backend of `lectern-fs-watch`
//! (`fs_events`, docs/design/fs_watch.md「platform 后端」). `open -a`,
//! Keychain, Seatbelt and the status line hook follow once their designs are
//! merged.

pub mod process;
#[cfg(unix)]
pub mod safe_read;
mod thread_clock;

#[cfg(unix)]
pub use safe_read::{
    DirEntry, FileIdentity, FileKind, FullRead, MAX_RANGE_BYTES, MAX_RANGES, Meta, OpenFile,
    Opened, RangeRead, Read, RelPath, ReplaceError, SafeError, SafeRoot, Timestamp,
};
pub use thread_clock::ThreadCpuClock;

/// The file watching backend (`fs_events::backend`) exists on macOS only; on
/// other platforms just its event conversion is compiled, for its tests.
#[cfg(any(target_os = "macos", test))]
pub mod fs_events;
