//! The on-disk part (`sink` feature, docs/design/logging.md「落盘」).

mod buffer;
mod export;
mod files;
mod fs;
mod panic;
mod verbose;
mod writer;

use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};
use std::thread::{JoinHandle, Thread};
use std::time::{Duration, Instant, SystemTime};

use log::Level;

use crate::engine::limit::Table;
use crate::engine::logger::{self, INSTALLED, Output, Shared};
use crate::engine::{encode, time};
use crate::filter::{self, Filter};
use crate::{clock, hooks};

pub(crate) use buffer::{Phase, RUN_CAP, VERBOSE_CAP, frames};
pub use export::{ExportError, ExportOptions, Exported};
pub(crate) use files::AllCaps;

/// The longest verbose logging the settings can turn on (「未决问题」2).
pub(crate) const MAX_VERBOSE: Duration = Duration::from_secs(7 * 86_400);

/// What `install_early` needs before the first frame.
#[derive(Clone, Debug, Default)]
pub struct EarlyConfig {
    /// `platform::dirs::logs()`. None keeps records in memory only.
    pub dir: Option<PathBuf>,
    /// `--log <filter>` from the command line; wins over `LECTERN_LOG`.
    pub cli_filter: Option<String>,
}

/// `install_early` was already called in this process.
#[derive(Debug, PartialEq, Eq)]
pub struct AlreadyInstalled;

/// The file header's description of the build.
#[derive(Clone, Debug)]
pub struct Header {
    pub version: &'static str,
    pub build: &'static str,
    /// The system version `platform` reads, such as "macOS 15.1".
    pub os: String,
    pub arch: &'static str,
}

pub struct Config {
    pub header: Header,
    /// Called once on the writer thread; `app` uses it to give the thread
    /// utility QoS through `platform`.
    pub on_writer_start: Option<Box<dyn FnOnce() + Send>>,
}

/// Verbose logging from the settings.
#[derive(Clone, Debug)]
pub struct Verbose {
    pub filter: Filter,
    /// 24 hours by default; at most 7 days.
    pub lasts: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterSource {
    Default,
    Env,
    Cli,
    Setting,
}

/// A snapshot of the logger's state.
#[derive(Clone, Debug)]
pub struct Status {
    pub filter: Filter,
    pub source: FilterSource,
    pub verbose_until: Option<SystemTime>,
    pub dir: Option<PathBuf>,
    /// Records dropped (full buffer, failed write) and left out by rate
    /// limits, in this process.
    pub dropped: u64,
    pub suppressed: u64,
    /// The bytes of default and verbose files, updated by the writer after
    /// each rotation and cleanup.
    pub disk_bytes: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Flushed {
    Clean,
    TimedOut { pending_bytes: u64 },
}

/// The process environment `install_early` reads.
pub(crate) struct Env {
    pub home: Option<String>,
    pub lectern_log: Option<String>,
    pub backtrace: bool,
}

impl Env {
    pub(crate) fn read() -> Env {
        Env {
            home: std::env::var("HOME").ok(),
            lectern_log: std::env::var("LECTERN_LOG").ok(),
            backtrace: std::env::var_os("RUST_BACKTRACE").is_some_and(|v| v != "0"),
        }
    }
}

/// The filter's source and verbose logging's state.
pub(crate) struct Settings {
    pub source: FilterSource,
    pub until: Option<SystemTime>,
    /// `--log` or `LECTERN_LOG` was given: the settings do not change the
    /// filter in this run.
    pub explicit: bool,
}

pub(crate) enum VerboseCmd {
    On {
        filter: Filter,
        until: SystemTime,
        applied: bool,
    },
    Off {
        applied: bool,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    Shutdown,
    /// Tests: end the writer without the end record, as a crash would.
    #[cfg(test)]
    Abandon,
}

/// Requests to the writer and its answers.
#[derive(Default)]
pub(crate) struct Control {
    pub flush_asked: u64,
    pub flushed: u64,
    pub fsync: bool,
    /// Verbose switches in the order they were made.
    pub verbose: Vec<VerboseCmd>,
    pub stop: Option<Stop>,
    /// The writer has ended (shutdown, or its loop panicked).
    pub finished: bool,
}

pub(crate) const BUSY: u8 = 0;
pub(crate) const WAITING: u8 = 1;
pub(crate) const IDLE: u8 = 2;

/// The sink's state, part of the leaked `Shared`.
pub(crate) struct State {
    pub dir: Option<PathBuf>,
    pub run: u64,
    pub pid: u32,
    /// The run's start, as file names write it.
    pub stamp: String,
    pub backtrace: bool,
    pub caps: AllCaps,
    /// Filter directives `install_early` could not read.
    pub skipped: Vec<usize>,
    pub buffers: Mutex<Buffers>,
    /// Mirrors of `buffers` written under its lock, so that the writer reads
    /// them without the lock and takes it only to swap: the running buffer
    /// has records; it is urgent.
    pub pending: AtomicBool,
    pub urgent: AtomicBool,
    /// The running buffers' capacity (`Buffers::cap`).
    pub cap: AtomicUsize,
    /// Files a panic before `start` opened; the writer takes them over.
    pub handoff: Mutex<Option<writer::Files>>,
    pub writer: OnceLock<Thread>,
    pub join: Mutex<Option<JoinHandle<()>>>,
    pub writer_state: AtomicU8,
    pub control: Mutex<Control>,
    pub changed: Condvar,
    pub settings: Mutex<Settings>,
    pub disk_bytes: AtomicU64,
}

pub(crate) use buffer::Buffers;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// `try_lock` attempts before a caller blocks on the buffer lock. The lock
/// is held for one copy of at most 4 KiB, well under a microsecond, while
/// a blocked waiter costs a sleep and a wake-up in the kernel (and the
/// releasing caller the wake-up call): on macOS the contended path of
/// `std::sync::Mutex` goes to the kernel at once. A few hundred attempts
/// are about 1–2 µs.
const SPINS: u32 = 256;

/// How often callers met the buffer lock taken, blocked on it after
/// spinning, and woke the writer: the cost `log/contended_call` measures,
/// counted on the slow paths only.
pub(crate) static CONTENDED: AtomicU64 = AtomicU64::new(0);
pub(crate) static BLOCKED: AtomicU64 = AtomicU64::new(0);
pub(crate) static WAKES: AtomicU64 = AtomicU64::new(0);

/// The buffer lock for a caller: spins briefly before blocking.
fn lock_buffers(m: &Mutex<Buffers>) -> MutexGuard<'_, Buffers> {
    match m.try_lock() {
        Ok(guard) => return guard,
        Err(std::sync::TryLockError::Poisoned(e)) => return e.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => {
            CONTENDED.fetch_add(1, Ordering::Relaxed);
        }
    }
    for _ in 0..SPINS {
        std::hint::spin_loop();
        match m.try_lock() {
            Ok(guard) => return guard,
            Err(std::sync::TryLockError::Poisoned(e)) => return e.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {}
        }
    }
    BLOCKED.fetch_add(1, Ordering::Relaxed);
    lock(m)
}

impl State {
    /// Copies a record into the buffer; false when it is dropped.
    pub(crate) fn push(&self, level: Level, line: &[u8]) -> bool {
        let (pushed, newly_urgent) = {
            let mut b = lock_buffers(&self.buffers);
            let was_urgent = b.urgent;
            let pushed = b.push(level, line);
            if pushed && b.phase == Phase::Running {
                // SeqCst pairs with the writer's store of IDLE / WAITING and
                // its load of these flags: one of the two sides sees the other.
                self.pending.store(true, Ordering::SeqCst);
                self.urgent.store(b.urgent, Ordering::SeqCst);
            }
            (pushed, b.urgent && !was_urgent)
        };
        if pushed {
            // Only the record that makes the buffer urgent unparks a waiting
            // writer; it re-reads the flag after it starts waiting.
            self.wake(newly_urgent);
        }
        pushed
    }

    /// Unparks the writer when it sleeps without a deadline, or for an
    /// urgent record when it waits for more.
    fn wake(&self, urgent: bool) {
        let Some(thread) = self.writer.get() else {
            return;
        };
        match self.writer_state.load(Ordering::SeqCst) {
            IDLE => {
                if self
                    .writer_state
                    .compare_exchange(IDLE, BUSY, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    WAKES.fetch_add(1, Ordering::Relaxed);
                    thread.unpark();
                }
            }
            WAITING if urgent => {
                WAKES.fetch_add(1, Ordering::Relaxed);
                thread.unpark();
            }
            _ => {}
        }
    }

    fn command(&self, f: impl FnOnce(&mut Control)) {
        f(&mut lock(&self.control));
        if let Some(thread) = self.writer.get() {
            thread.unpark();
        }
    }

    pub(crate) fn writer_started(&self) -> bool {
        lock(&self.join).is_some()
    }

    /// Asks the writer to write what is buffered (and `fsync` it), and waits
    /// up to `timeout`. False when the writer is not running or did not
    /// finish in time.
    pub(crate) fn flush(&self, timeout: Duration, fsync: bool) -> bool {
        if !self.writer_started() {
            return false;
        }
        let want = {
            let mut c = lock(&self.control);
            if c.finished {
                return false;
            }
            c.flush_asked += 1;
            c.fsync |= fsync;
            c.flush_asked
        };
        if let Some(thread) = self.writer.get() {
            thread.unpark();
        }
        let deadline = Instant::now() + timeout;
        let mut c = lock(&self.control);
        while c.flushed < want && !c.finished {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            c = self
                .changed
                .wait_timeout(c, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        c.flushed >= want
    }

    fn set_cap(&self, cap: usize) {
        let mut b = lock(&self.buffers);
        b.set_cap(cap);
        self.cap.store(cap, Ordering::Relaxed);
    }

    pub(crate) fn verbose_on(&self) -> bool {
        lock(&self.settings).source != FilterSource::Default
    }
}

/// Installs the logger: `app` calls it on the main thread as the first line
/// of `main`, before the first frame, once (again returns `Err`).
///
/// Memory only: `log::set_logger`, the global level, `HOME` and
/// `LECTERN_LOG`, the filter, the 64 KiB early buffer, the panic hook. No
/// thread, no file.
pub fn install_early(config: EarlyConfig) -> Result<Early, AlreadyInstalled> {
    install_with(config, Env::read(), AllCaps::STANDARD)
}

pub(crate) fn install_with(
    config: EarlyConfig,
    env: Env,
    caps: AllCaps,
) -> Result<Early, AlreadyInstalled> {
    if INSTALLED.swap(true, Ordering::AcqRel) {
        return Err(AlreadyInstalled);
    }
    clock::mark_start();
    let epoch = panic::next_epoch();
    let (filter, shared) = prepare(config, env, caps);
    filter::activate(filter);
    let shared = logger::install(shared);
    panic::install(epoch);
    Ok(Early { shared })
}

/// Everything `install_early` allocates and reads, before it is installed.
fn prepare(config: EarlyConfig, env: Env, caps: AllCaps) -> (Filter, Shared) {
    let (filter, source, skipped) =
        choose_filter(config.cli_filter.as_deref(), env.lectern_log.as_deref());
    let started = clock::now();
    let pid = std::process::id();
    let state = State {
        dir: config.dir,
        run: run_id(pid, started),
        pid,
        stamp: time::file_stamp(started),
        backtrace: env.backtrace,
        caps,
        skipped,
        buffers: Mutex::new(Buffers::new()),
        handoff: Mutex::new(None),
        writer: OnceLock::new(),
        join: Mutex::new(None),
        writer_state: AtomicU8::new(BUSY),
        control: Mutex::new(Control::default()),
        changed: Condvar::new(),
        settings: Mutex::new(Settings {
            source,
            until: None,
            explicit: source != FilterSource::Default,
        }),
        disk_bytes: AtomicU64::new(0),
        pending: AtomicBool::new(false),
        urgent: AtomicBool::new(false),
        cap: AtomicUsize::new(RUN_CAP),
    };
    let home = env
        .home
        .map(|h| h.trim_end_matches('/').to_owned())
        .filter(|h| !h.is_empty())
        .map(String::into_boxed_str);
    let shared = Shared {
        home,
        table: Table::new(),
        dropped: AtomicU64::new(0),
        output: Output::Sink(state),
    };
    (filter, shared)
}

/// What `install_cost` allocated.
#[cfg(feature = "test-support")]
pub(crate) struct InstallCost {
    _shared: Box<Shared>,
    _filter: Filter,
}

/// `install_early`'s work without replacing the installed logger
/// (`testing::install_early_again`): the global level is stored again with
/// its current value, `set_logger` fails, the panic hook is taken and put
/// back.
#[cfg(feature = "test-support")]
pub(crate) fn install_cost(config: EarlyConfig, env: Env, caps: AllCaps) -> InstallCost {
    struct Nop;
    impl log::Log for Nop {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            false
        }
        fn log(&self, _: &log::Record<'_>) {}
        fn flush(&self) {}
    }
    static NOP: Nop = Nop;
    let (filter, shared) = prepare(config, env, caps);
    let _ = filter.max();
    log::set_max_level(log::max_level());
    let _ = log::set_logger(&NOP);
    std::panic::set_hook(std::panic::take_hook());
    InstallCost {
        _shared: Box::new(shared),
        _filter: filter,
    }
}

fn choose_filter(cli: Option<&str>, env: Option<&str>) -> (Filter, FilterSource, Vec<usize>) {
    let given = |s: Option<&str>| {
        s.map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    if let Some(cli) = given(cli) {
        let (f, skipped) = Filter::parse(&cli);
        (f, FilterSource::Cli, skipped)
    } else if let Some(env) = given(env) {
        let (f, skipped) = Filter::parse(&env);
        (f, FilterSource::Env, skipped)
    } else {
        (Filter::default(), FilterSource::Default, Vec::new())
    }
}

/// 64 random bits per run, seeded by `RandomState` (no file read).
fn run_id(pid: u32, started: SystemTime) -> u64 {
    let mut h = std::hash::RandomState::new().build_hasher();
    h.write_u32(pid);
    h.write_u128(
        started
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    h.finish()
}

/// The logger between `install_early` and `start`.
pub struct Early {
    shared: &'static Shared,
}

impl Early {
    /// Called by `app` once the startup barrier opens (after the first frame
    /// is shown). Starts the `lectern-log` thread and returns at once; the
    /// directory, cleanup, `verbose.json` and files are all handled on that
    /// thread. Without a directory, or when the thread cannot start, the
    /// returned `Logging` keeps records in memory only.
    pub fn start(self, config: Config) -> Logging {
        let shared = self.shared;
        let logging = Logging { shared };
        let state = logging.state();
        if state.dir.is_none() {
            return logging;
        }
        hooks::spawned();
        let spawned = std::thread::Builder::new()
            .name("lectern-log".to_owned())
            .spawn(move || writer::run(shared, config));
        match spawned {
            Ok(handle) => *lock(&state.join) = Some(handle),
            Err(e) => {
                let line = writer::note_line(
                    Level::Warn,
                    "log writer thread unavailable",
                    &[("error", crate::Value::io(&e))],
                    shared.home.as_deref(),
                );
                state.push(Level::Warn, &line);
            }
        }
        logging
    }
}

/// The running logger. `app` keeps one and hands clones to the settings
/// and the export command.
#[derive(Clone)]
pub struct Logging {
    shared: &'static Shared,
}

impl Logging {
    pub(crate) fn state(&self) -> &'static State {
        match self.shared.sink() {
            Some(state) => state,
            None => unreachable!("a Logging always has a sink"),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(shared: &'static Shared) -> Logging {
        Logging { shared }
    }

    /// Turns verbose logging from the settings on or off. Any thread, never
    /// blocks: the filter changes at once, `verbose.json` is written by the
    /// writer. When `--log` or `LECTERN_LOG` set the filter, the setting is
    /// saved but does not change this run's filter.
    pub fn set_verbose(&self, verbose: Option<Verbose>) {
        let state = self.state();
        let cmd = {
            let mut s = lock(&state.settings);
            match verbose {
                Some(Verbose { filter, lasts }) => {
                    let until = clock::now() + lasts.min(MAX_VERBOSE);
                    let applied = !s.explicit;
                    if applied {
                        filter::activate(filter.clone());
                        s.source = FilterSource::Setting;
                        s.until = Some(until);
                        state.set_cap(VERBOSE_CAP);
                    }
                    VerboseCmd::On {
                        filter,
                        until,
                        applied,
                    }
                }
                None => {
                    let applied = s.source == FilterSource::Setting;
                    if applied {
                        filter::activate(Filter::default());
                        s.source = FilterSource::Default;
                        s.until = None;
                        state.set_cap(RUN_CAP);
                    }
                    VerboseCmd::Off { applied }
                }
            }
        };
        state.command(|c| c.verbose.push(cmd));
    }

    /// A snapshot of the current state. Any thread, never blocks.
    pub fn status(&self) -> Status {
        let state = self.state();
        let s = lock(&state.settings);
        Status {
            filter: filter::active()
                .map(|a| a.filter.clone())
                .unwrap_or_default(),
            source: s.source,
            verbose_until: s.until,
            dir: state.dir.clone(),
            dropped: self.shared.dropped.load(Ordering::Relaxed),
            suppressed: self.shared.suppressed(),
            disk_bytes: state.disk_bytes.load(Ordering::Relaxed),
        }
    }

    /// Copies the recent runs into a new directory under `parent`. Blocks
    /// (copies files): call it from an executor `Background` task. First
    /// lets the writer write what is buffered (up to 1 s).
    pub fn export(&self, parent: &Path, options: &ExportOptions) -> Result<Exported, ExportError> {
        export::export(self, parent, options)
    }

    /// Normal exit, after kernel and executor are shut down: writes the
    /// buffer and the end record, `fsync`s, closes the files and joins the
    /// writer, waiting at most `timeout`. Later records are only counted.
    pub fn shutdown(&self, timeout: Duration) -> Flushed {
        self.stop(Stop::Shutdown, timeout)
    }

    fn stop(&self, stop: Stop, timeout: Duration) -> Flushed {
        let state = self.state();
        if !state.writer_started() {
            let mut b = lock(&state.buffers);
            b.phase = Phase::Closed;
            return Flushed::Clean;
        }
        state.command(|c| {
            if c.stop.is_none() {
                c.stop = Some(stop);
            }
        });
        let deadline = Instant::now() + timeout;
        let mut c = lock(&state.control);
        while !c.finished {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                drop(c);
                let b = lock(&state.buffers);
                return Flushed::TimedOut {
                    pending_bytes: (b.front.len() + b.early.len()) as u64,
                };
            }
            c = state
                .changed
                .wait_timeout(c, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        drop(c);
        if let Some(handle) = lock(&state.join).take() {
            let _ = handle.join();
        }
        Flushed::Clean
    }

    /// Tests: stop the writer without the end record.
    #[cfg(test)]
    pub(crate) fn abandon(&self) {
        self.stop(Stop::Abandon, Duration::from_secs(5));
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn flush(&self, timeout: Duration) -> bool {
        self.state().flush(timeout, false)
    }
}

/// Encodes a line for a record the logger writes itself.
pub(crate) fn encode_line(f: impl FnOnce(&mut encode::Out<'_>)) -> Vec<u8> {
    let mut buf = vec![0u8; encode::MAX_LINE];
    let mut out = encode::Out::new(&mut buf);
    f(&mut out);
    let len = out.bytes().len();
    buf.truncate(len);
    buf
}

#[cfg(test)]
pub(crate) fn install_for_test(
    config: EarlyConfig,
    env: Env,
    caps: AllCaps,
) -> Result<Early, AlreadyInstalled> {
    install_with(config, env, caps)
}

#[cfg(test)]
pub(crate) use buffer::EARLY_CAP as EARLY_CAP_FOR_TESTS;
#[cfg(test)]
pub(crate) use files::{Caps, Keep, Listed, Name, cleanup, file_name, list, parse_name, plan};
