//! OS-specific capabilities. Today: reading files beneath a directory root
//! without following symbolic links ([`safe_read`], docs/design/platform.md),
//! the physical footprint of a process for lectern-bench ([`process`]),
//! and, on macOS, the FSEvents and kqueue backend of `lectern-fs-watch`
//! (`fs_events`, docs/design/fs_watch.md「platform 后端」). `open -a`,
//! Keychain, Seatbelt and the status line hook follow once their designs are
//! merged.

pub mod process;
#[cfg(unix)]
pub mod safe_read;

#[cfg(unix)]
pub use safe_read::{FileKind, Meta, Read, RelPath, SafeError, SafeRoot, Timestamp};

/// The file watching backend (`fs_events::backend`) exists on macOS only; on
/// other platforms just its event conversion is compiled, for its tests.
#[cfg(any(target_os = "macos", test))]
pub mod fs_events;
