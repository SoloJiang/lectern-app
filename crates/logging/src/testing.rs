//! Test support (`test-support` feature): capture records in other crates'
//! tests (their canary tests), and the entry points lectern-bench needs.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

pub use log::Level;

use crate::engine::limit::Table;
use crate::engine::logger::{self, INSTALLED, Output, Shared};
use crate::{callsite, clock, filter};

/// Records as the sink would write them, kept in memory.
pub struct Capture {
    shared: &'static Shared,
}

static CAPTURE: OnceLock<Capture> = OnceLock::new();

/// Installs a capture logger in this process, filtered at trace (third-party
/// targets included). nextest runs each test in its own process, so each
/// test installs its own; a second call in the same process returns the
/// same capture. Records are kept encoded as the sink writes them.
///
/// # Panics
///
/// When `install_early` already installed the sink in this process.
pub fn capture() -> &'static Capture {
    CAPTURE.get_or_init(|| {
        assert!(
            !INSTALLED.swap(true, Ordering::AcqRel),
            "lectern_logging::testing::capture: a logger is already installed"
        );
        clock::mark_start();
        filter::activate(filter::Filter::everything());
        let shared = logger::install(Shared {
            home: std::env::var("HOME")
                .ok()
                .map(|h| h.trim_end_matches('/').to_owned())
                .filter(|h| !h.is_empty())
                .map(String::into_boxed_str),
            table: Table::new(),
            dropped: AtomicU64::new(0),
            output: Output::Capture(Mutex::new(Vec::new())),
        });
        Capture { shared }
    })
}

impl Capture {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<String>> {
        match &self.shared.output {
            Output::Capture(lines) => lines.lock().unwrap_or_else(|e| e.into_inner()),
            #[allow(
                unreachable_patterns,
                reason = "Sink exists only with the sink feature"
            )]
            _ => unreachable!("a capture's output is a capture"),
        }
    }

    /// Every record so far, one JSON line each (without the newline).
    pub fn lines(&self) -> Vec<String> {
        self.lock().clone()
    }

    pub fn clear(&self) {
        self.lock().clear();
    }

    /// The canary check.
    ///
    /// # Panics
    ///
    /// When any record contains `needle`; the message shows that record.
    pub fn assert_absent(&self, needle: &str) {
        if let Some(line) = self.lock().iter().find(|l| l.contains(needle)) {
            panic!("a log record contains the canary {needle:?}: {line}");
        }
    }
}

/// Logs a third-party record, as a crate calling `log` directly would
/// (GPUI, gix): through `log::logger()`, with this target, location and
/// message.
pub fn third_party(
    level: Level,
    target: &str,
    file: Option<&'static str>,
    line: Option<u32>,
    args: fmt::Arguments<'_>,
) {
    log::logger().log(
        &log::Record::builder()
            .level(level)
            .target(target)
            .file_static(file)
            .line(line)
            .args(args)
            .build(),
    );
}

/// Sets how many error, warn and info records each Lectern call site writes
/// per 10-second window (20 normally). The benchmarks raise it to time the
/// written path again and again from one call site.
pub fn set_callsite_limit(per_window: u32) {
    callsite::LECTERN_LIMIT.store(per_window, Ordering::Relaxed);
}

#[cfg(feature = "sink")]
pub use sink_support::*;

#[cfg(feature = "sink")]
mod sink_support {
    use std::time::Duration;

    use crate::sink::{AllCaps, Env, InstallCost};
    use crate::{EarlyConfig, Logging};

    /// Writes what is buffered and waits for it (at most `timeout`); false
    /// when the writer is not running or took longer.
    pub fn flush(logging: &Logging, timeout: Duration) -> bool {
        logging.flush(timeout)
    }

    /// The callers' slow paths so far in this process (`log/contended_call`
    /// reports the difference over its run).
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Contention {
        /// A caller found the buffer lock taken.
        pub contended: u64,
        /// A caller still found it taken after spinning, and blocked.
        pub blocked: u64,
        /// A caller woke the writer (a system call on the calling thread).
        pub wakes: u64,
    }

    pub fn contention() -> Contention {
        use std::sync::atomic::Ordering::Relaxed;
        Contention {
            contended: crate::sink::CONTENDED.load(Relaxed),
            blocked: crate::sink::BLOCKED.load(Relaxed),
            wakes: crate::sink::WAKES.load(Relaxed),
        }
    }

    /// What [`install_early_again`] allocated; freed when dropped.
    pub struct EarlyAgain(#[allow(dead_code, reason = "held to be dropped")] InstallCost);

    /// The work of `install_early` once more, for `log/install_early`:
    /// reading the environment, parsing the filter, allocating the early
    /// buffer and the rate-limit table, the logger's state, setting the
    /// global level, `log::set_logger` (it fails: one is installed) and a
    /// panic hook round trip. Nothing replaces the installed logger.
    pub fn install_early_again(config: EarlyConfig) -> EarlyAgain {
        EarlyAgain(crate::sink::install_cost(
            config,
            Env::read(),
            AllCaps::STANDARD,
        ))
    }
}
