//! The crate's limits (docs/design/file_reader.md「`syntax` 的公开项」
//! `limits`). Later steps add the tier, window and LRU limits here.

use std::time::Duration;

/// A parse call counts as stuck once its thread's CPU time since it entered
/// tree-sitter exceeds the call's budget plus this ([`crate::health`]).
pub const STUCK_GRACE: Duration = Duration::from_secs(1);

/// The grace when the thread's CPU clock cannot be read and wall time is
/// used instead: wall time also grows while the thread waits for a CPU.
pub const STUCK_GRACE_WALL: Duration = Duration::from_secs(10);
