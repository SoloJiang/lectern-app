//! Unit tests (docs/design/logging.md「测试」). Each test installs the
//! logger in its own process under nextest; `serial()` also keeps them
//! apart when `cargo test` runs them in one process. The clock is a
//! `ManualClock` and waits are flushes, not sleeps.

mod buffers;
mod encode;
mod export;
mod filters;
mod io;
mod limits;
mod panics;
mod privacy;
mod rotation;
mod scopes;
mod verbose;

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::clock::manual::ManualClock;
use crate::engine::logger;
use crate::hooks::imp as hooks;
use crate::sink::{AllCaps, Env, install_for_test};
use crate::{Config, Early, EarlyConfig, Header, Logging};

/// The home directory the tests' logger sees.
pub(crate) const HOME: &str = "/home/tester";

static SERIAL: Mutex<()> = Mutex::new(());

/// One test's logger: its lock, its clock and its directory.
pub(crate) struct Fixture {
    pub dir: tempfile::TempDir,
    pub clock: ManualClock,
    _serial: MutexGuard<'static, ()>,
}

/// 2026-10-22T03:27:01Z.
pub(crate) fn base_time() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_792_639_621)
}

impl Fixture {
    pub(crate) fn new() -> Fixture {
        let serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        Fixture {
            dir: tempfile::tempdir().expect("temporary directory"),
            clock: ManualClock::install(base_time()),
            _serial: serial,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        self.dir.path()
    }

    /// `install_early` with this directory, a command-line and an
    /// environment filter.
    pub(crate) fn install(&self, cli: Option<&str>, env: Option<&str>) -> Early {
        self.install_with(cli, env, AllCaps::STANDARD)
    }

    pub(crate) fn install_with(
        &self,
        cli: Option<&str>,
        env: Option<&str>,
        caps: AllCaps,
    ) -> Early {
        install_for_test(
            EarlyConfig {
                dir: Some(self.path().to_owned()),
                cli_filter: cli.map(str::to_owned),
            },
            test_env(env),
            caps,
        )
        .expect("first install in this test")
    }

    /// Installs and starts, waiting until the writer has written the header.
    pub(crate) fn start(&self, cli: Option<&str>) -> Logging {
        let logging = self.install(cli, None).start(config());
        assert!(logging.flush(Duration::from_secs(5)), "the writer starts");
        logging
    }

    /// Ends this run (with the end record) and uninstalls, so that the
    /// test can install a new run on the same directory.
    pub(crate) fn restart(&self, logging: &Logging) {
        logging.shutdown(Duration::from_secs(5));
        reset();
        // The next run's file names differ by their start time.
        self.clock.advance(Duration::from_secs(1));
    }

    /// The same, without the end record: the run "crashed".
    pub(crate) fn crash(&self, logging: &Logging) {
        logging.abandon();
        reset();
        self.clock.advance(Duration::from_secs(1));
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(shared) = logger::current()
            && let Some(state) = shared.sink()
            && state.writer_started()
        {
            hooks::stall_writer(false);
        }
        reset();
    }
}

pub(crate) fn test_env(filter: Option<&str>) -> Env {
    Env {
        home: Some(HOME.to_owned()),
        lectern_log: filter.map(str::to_owned),
        backtrace: false,
    }
}

pub(crate) fn config() -> Config {
    Config {
        header: Header {
            version: "0.1.0",
            build: "test",
            os: "TestOS 1.0".to_owned(),
            arch: "test64",
        },
        on_writer_start: None,
    }
}

/// Removes the installed logger (its state is leaked) and the hooks' state.
pub(crate) fn reset() {
    if let Some(shared) = logger::current()
        && let Some(state) = shared.sink()
        && state.writer_started()
    {
        hooks::stall_writer(false);
        let logging = Logging::for_test(shared);
        logging.shutdown(Duration::from_secs(5));
    }
    logger::uninstall();
    hooks::reset();
    crate::callsite::LECTERN_LIMIT.store(
        crate::callsite::PER_WINDOW,
        std::sync::atomic::Ordering::Relaxed,
    );
    // The process-wide suppressed count would otherwise carry earlier
    // tests' counts into `Status::suppressed` when they share a process.
    crate::callsite::SUPPRESSED.store(0, std::sync::atomic::Ordering::Relaxed);
}

/// Every file under `dir`, recursively, with its text.
pub(crate) fn all_files(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let text =
                    String::from_utf8_lossy(&std::fs::read(&path).unwrap_or_default()).into_owned();
                out.push((path, text));
            }
        }
    }
    out.sort();
    out
}

/// The records of the default files in `dir`, in file order.
pub(crate) fn records(dir: &Path) -> Vec<serde_json::Value> {
    records_in(dir)
}

pub(crate) fn verbose_records(dir: &Path) -> Vec<serde_json::Value> {
    records_in(&dir.join("verbose"))
}

fn records_in(dir: &Path) -> Vec<serde_json::Value> {
    let mut files: Vec<_> = crate::sink::list(dir);
    files.sort_by(|a, b| a.name.cmp(&b.name));
    files
        .iter()
        .flat_map(|f| {
            std::fs::read_to_string(&f.path)
                .unwrap_or_default()
                .lines()
                .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not JSON ({e}): {l}")))
                .collect::<Vec<serde_json::Value>>()
        })
        .collect()
}

/// The records with this `msg`.
pub(crate) fn with_msg<'a>(
    records: &'a [serde_json::Value],
    msg: &str,
) -> Vec<&'a serde_json::Value> {
    records.iter().filter(|r| r["msg"] == msg).collect()
}

/// Asserts that no file under `dir` contains `canary`.
pub(crate) fn assert_absent(dir: &Path, canary: &str) {
    let files = all_files(dir);
    assert!(!files.is_empty(), "the test wrote no files");
    for (path, text) in files {
        assert!(
            !text.contains(canary),
            "{} contains the canary:\n{text}",
            path.display()
        );
    }
}

/// A third-party record through `log::logger()`, as GPUI would write it.
pub(crate) fn third_party(
    level: log::Level,
    target: &str,
    file: &'static str,
    line: u32,
    args: std::fmt::Arguments<'_>,
) {
    log::logger().log(
        &log::Record::builder()
            .level(level)
            .target(target)
            .file_static(Some(file))
            .line(Some(line))
            .args(args)
            .build(),
    );
}
