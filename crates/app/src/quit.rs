//! The quit order (docs/design/app.md「退出顺序」): kernel, then executor,
//! then logging, on a `lectern-quit` thread while the main thread keeps
//! running; then `cx.quit()`.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use gpui::{App, Global, actions};
use lectern_executor::Shutdown;
use lectern_logging::{Field, Flushed, Value, debug, info, warn};

use crate::startup::Started;

actions!(lectern, [Quit]);

/// Kernel and executor together, counted from the start of the quit
/// (docs/design/kernel.md `Kernel::shutdown`).
const SERVICES: Duration = Duration::from_secs(2);
/// `Logging::shutdown` after both (docs/design/logging.md).
const LOGGING: Duration = Duration::from_millis(500);
/// `Logging::shutdown` in `on_app_quit`, inside GPUI's 200 ms
/// `SHUTDOWN_TIMEOUT`.
const LOGGING_ON_TERMINATE: Duration = Duration::from_millis(150);
/// How often the main thread looks at the quit thread while it waits.
const POLL: Duration = Duration::from_millis(10);

/// The three steps. The release build implements them with `Started`;
/// tests use fakes that record the order and can hang.
pub trait QuitSteps: Send + 'static {
    /// `Kernel::shutdown`; `done` runs once the kernel thread has finished.
    fn kernel(&self, done: Box<dyn FnOnce() + Send>);
    fn executor(&self, timeout: Duration) -> Shutdown;
    fn logging(&self, timeout: Duration) -> Flushed;
}

pub struct Timeouts {
    pub services: Duration,
    pub logging: Duration,
}

impl Timeouts {
    pub const RELEASE: Timeouts = Timeouts {
        services: SERVICES,
        logging: LOGGING,
    };
}

/// What each step ended with. The quit thread has already logged what
/// matters, so only the tests read it.
#[derive(Debug)]
#[cfg_attr(not(test), expect(dead_code, reason = "only the tests read it"))]
pub struct Outcome {
    pub kernel_timed_out: bool,
    pub executor: Shutdown,
    pub logging: Flushed,
}

/// Runs the three steps in order and returns once each has finished or
/// timed out. Blocks: never on the main thread.
pub fn run(steps: &dyn QuitSteps, timeouts: &Timeouts) -> Outcome {
    let start = Instant::now();
    let deadline = start + timeouts.services;
    let (tx, rx) = mpsc::channel();
    steps.kernel(Box::new(move || {
        let _ = tx.send(());
    }));
    // A dropped `done` (the kernel thread already gone) is not a timeout.
    let kernel_timed_out = matches!(
        rx.recv_timeout(deadline.saturating_duration_since(Instant::now())),
        Err(RecvTimeoutError::Timeout)
    );
    let kernel_took = start.elapsed();
    if kernel_timed_out {
        warn!("kernel shutdown timed out"; waited_us = kernel_took);
    }
    let executor_start = Instant::now();
    let executor = steps.executor(deadline.saturating_duration_since(Instant::now()));
    if let Shutdown::TimedOut { busy_workers } = executor {
        warn!("executor shutdown timed out"; busy_workers = busy_workers);
    }
    // Records after `Logging::shutdown` are only counted.
    debug!("quit finished"; kernel_us = kernel_took, executor_us = executor_start.elapsed());
    let logging = steps.logging(timeouts.logging);
    Outcome {
        kernel_timed_out,
        executor,
        logging,
    }
}

impl QuitSteps for Started {
    fn kernel(&self, done: Box<dyn FnOnce() + Send>) {
        self.kernel.shutdown(done);
    }

    fn executor(&self, timeout: Duration) -> Shutdown {
        self.executor.shutdown(timeout)
    }

    fn logging(&self, timeout: Duration) -> Flushed {
        self.logging.shutdown(timeout)
    }
}

#[derive(Clone, Copy)]
enum QuitSource {
    Action,
    System,
}

impl Field for QuitSource {
    fn value(&self) -> Value<'_> {
        Value::text(match self {
            QuitSource::Action => "action",
            QuitSource::System => "system",
        })
    }
}

/// Set once the quit order has started; a second ⌘Q is ignored.
struct Quitting;

impl Global for Quitting {}

/// Whether the quit order has started. A barrier that fires afterwards
/// starts nothing.
pub fn is_quitting(cx: &App) -> bool {
    cx.has_global::<Quitting>()
}

/// Registers the `Quit` action and the `on_app_quit` hook. Main thread, at
/// startup.
pub fn init(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| quit(cx));
    cx.on_app_quit(|cx| {
        // Through `Quit` logging is already shut down and this returns at
        // once; a system quit gets its end record here.
        if let Some(started) = cx.try_global::<Started>() {
            if !is_quitting(cx) {
                info!("quit requested"; path = QuitSource::System);
            }
            let _ = started.logging.shutdown(LOGGING_ON_TERMINATE);
        }
        async {}
    })
    .detach();
}

/// The `Quit` action. Main thread, does not block.
pub fn quit(cx: &mut App) {
    if is_quitting(cx) {
        return;
    }
    cx.set_global(Quitting);
    let Some(started) = cx.try_global::<Started>() else {
        // Before the barrier: nothing was started.
        cx.quit();
        return;
    };
    info!("quit requested"; path = QuitSource::Action);
    let steps = started.clone();
    let spawned = thread::Builder::new()
        .name("lectern-quit".into())
        .spawn(move || run(&steps, &Timeouts::RELEASE));
    let handle = match spawned {
        Ok(handle) => handle,
        Err(e) => {
            warn!("could not start the quit thread"; error = Value::io(&e));
            cx.quit();
            return;
        }
    };
    // Wait without blocking the main thread: kernel teardown may need it.
    cx.spawn(async move |cx| {
        while !handle.is_finished() {
            cx.background_executor().timer(POLL).await;
        }
        let _ = handle.join();
        cx.update(|cx| cx.quit());
    })
    .detach();
}
