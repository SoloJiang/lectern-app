//! The crate's limits (docs/design/file_reader.md「`syntax` 的公开项」
//! `limits`). Later steps add the text, page and LRU limits here.

use std::time::Duration;

/// 1 MiB: a file up to this size is read whole on the first screen (one
/// range, [`crate::window::first_read`]) and is parsed whole after it (tier
/// A, [`crate::highlight::FullParse`]).
pub const TIER_A_MAX: u64 = 1 << 20;

/// 4 KiB: a longer line is folded into display rows, and a file with one is
/// not tier A. The first screen looks back at most this far for the
/// target's line start; past it the target is in a long line.
pub const LONG_LINE: u64 = 4 << 10;

/// The first screen's window parse (ADR 0034): past it the screen shows
/// plain text first.
pub const FIRST_WINDOW_DEADLINE: Duration = Duration::from_millis(8);

/// One slice of a tier A whole-file parse.
pub const SLICE: Duration = Duration::from_millis(8);

/// A 64 KiB window whose parse takes longer than this twice in a row turns
/// the file's highlighting off for this opening
/// ([`crate::highlight::SlowWindows`]).
pub const SLOW_WINDOW: Duration = Duration::from_millis(128);

/// A window of the chain ([`crate::highlight::ChainWindow`]): about 64 KiB,
/// its tree 2–5 MiB.
pub const WINDOW: u64 = 64 << 10;

/// A window with no complete node is widened to this once.
pub const WINDOW_MAX: u64 = 256 << 10;

/// A parse call counts as stuck once its thread's CPU time since it entered
/// tree-sitter exceeds the call's budget plus this ([`crate::health`]).
pub const STUCK_GRACE: Duration = Duration::from_secs(1);

/// The grace when the thread's CPU clock cannot be read and wall time is
/// used instead: wall time also grows while the thread waits for a CPU.
pub const STUCK_GRACE_WALL: Duration = Duration::from_secs(10);
