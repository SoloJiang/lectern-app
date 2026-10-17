//! OS-specific capabilities. Today: reading files beneath a directory root
//! without following symbolic links ([`safe_read`], docs/design/platform.md)
//! and the physical footprint of a process for lectern-bench ([`process`]).
//! FSEvents, `open -a`, Keychain, Seatbelt and the status line hook follow
//! once their designs are merged.

pub mod process;
#[cfg(unix)]
pub mod safe_read;

#[cfg(unix)]
pub use safe_read::{FileKind, Meta, Read, RelPath, SafeError, SafeRoot, Timestamp};
