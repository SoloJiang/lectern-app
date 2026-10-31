//! OS-specific capabilities. Today: reading files beneath a directory root
//! without following symbolic links, in full or by byte range, and replacing
//! one atomically ([`safe_read`], docs/design/platform.md),
//! the physical footprint, creation time and children of a process, the
//! machine description and the thread QoS for lectern-bench ([`process`],
//! [`host`], [`thread`]),
//! the first frame's time, the per-frame recording and the error of the
//! measurement build's method replacements ([`frame`], [`HookError`]; behind
//! the `bench-hooks` feature),
//! the CPU time of a thread ([`ThreadCpuClock`]),
//! the app data and log directories ([`dirs::app_data`], [`dirs::logs`]),
//! the OS version for log file headers ([`system::os_version`]),
//! the local time zone's offset from UTC ([`time::utc_offset`]),
//! the user's login shell environment for agents ([`login_env`], on macOS
//! and Linux),
//! the window frame autosave name of AppKit
//! ([`window::set_frame_autosave_name`]),
//! the system appearance's colours, reduce-motion setting and their changes
//! ([`appearance`]), writing text to the system clipboard
//! ([`clipboard::write_text`]), and, on macOS, the FSEvents and kqueue backend of
//! `lectern-fs-watch` (`fs_events`, docs/design/fs_watch.md「platform 后端」).
//! `open -a`,
//! Keychain, Seatbelt and the status line hook follow once their designs are
//! merged.

pub mod appearance;
pub mod clipboard;
pub mod dirs;
pub mod frame;
mod hook;
pub mod host;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod login_env;
pub mod process;
#[cfg(unix)]
pub mod safe_read;
pub mod system;
pub mod thread;
mod thread_clock;
pub mod time;
pub mod window;

pub use hook::HookError;
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
