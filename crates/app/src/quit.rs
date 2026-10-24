//! The quit order (docs/design/app.md「退出顺序」「退出顺序的变化」): kernel,
//! watcher, executor, cache, then logging, on a `lectern-quit` thread while
//! the main thread keeps running; then `cx.quit()`.

use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use gpui::{App, Global, actions};
use lectern_executor::Shutdown;
use lectern_logging::{Field, Flushed, Value, debug, info, warn};
use lectern_services::workspace::LeaveScope;
use lectern_workspace::WorkspaceHost;

use crate::startup::Started;

actions!(lectern, [Quit]);

/// Kernel, watcher, executor and cache together, counted from the start of
/// the quit (docs/design/kernel.md `Kernel::shutdown`).
const SERVICES: Duration = Duration::from_secs(2);
/// `Logging::shutdown` after both (docs/design/logging.md).
const LOGGING: Duration = Duration::from_millis(500);
/// `Logging::shutdown` in `on_app_quit`, inside GPUI's 200 ms
/// `SHUTDOWN_TIMEOUT`.
const LOGGING_ON_TERMINATE: Duration = Duration::from_millis(150);
/// How often the main thread looks at the quit thread while it waits.
const POLL: Duration = Duration::from_millis(10);

/// The steps. The release build implements them with `Started`; tests use
/// fakes that record the order and can hang.
pub trait QuitSteps: Send + Sync + 'static {
    /// `Kernel::shutdown`; `done` runs once the kernel thread has finished.
    fn kernel(&self, done: Box<dyn FnOnce() + Send>);
    /// `Watcher::shutdown`. Blocks up to the watcher's `flush_timeout`;
    /// [`run`] waits for it only until the deadline.
    fn watcher(&self);
    fn executor(&self, timeout: Duration) -> Shutdown;
    /// `Cache::close`. Blocks; [`run`] waits for it only until the deadline.
    fn cache(&self);
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
    pub watcher_timed_out: bool,
    pub executor: Shutdown,
    /// False also when there was no time left to start it.
    pub cache_timed_out: bool,
    pub logging: Flushed,
}

/// Runs the steps in order and returns once each has finished or timed
/// out. Blocks: never on the main thread.
pub fn run(steps: Arc<dyn QuitSteps>, timeouts: &Timeouts) -> Outcome {
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
    let watcher_start = Instant::now();
    let watcher_timed_out = bounded(deadline, {
        let steps = steps.clone();
        move || steps.watcher()
    });
    let watcher_took = watcher_start.elapsed();
    if watcher_timed_out {
        warn!("watcher shutdown timed out"; timeout_us = watcher_took);
    }
    let executor_start = Instant::now();
    let executor = steps.executor(deadline.saturating_duration_since(Instant::now()));
    let executor_took = executor_start.elapsed();
    if let Shutdown::TimedOut { busy_workers } = executor {
        warn!("executor shutdown timed out"; busy_workers = busy_workers);
    }
    let cache_start = Instant::now();
    // No time left: dropping the cache is as safe and only loses the access
    // times (docs/design/cache.md「生命周期」).
    let cache_timed_out = !deadline.saturating_duration_since(cache_start).is_zero()
        && bounded(deadline, {
            let steps = steps.clone();
            move || steps.cache()
        });
    let cache_took = cache_start.elapsed();
    if cache_timed_out {
        warn!("cache close timed out"; timeout_us = cache_took);
    }
    // Records after `Logging::shutdown` are only counted.
    debug!("quit finished";
        kernel_us = kernel_took,
        watcher_us = watcher_took,
        executor_us = executor_took,
        cache_us = cache_took);
    let logging = steps.logging(timeouts.logging);
    Outcome {
        kernel_timed_out,
        watcher_timed_out,
        executor,
        cache_timed_out,
        logging,
    }
}

/// Runs `step` on a short-lived `lectern-quit-step` thread and waits for it
/// until `deadline`. True when it timed out: the thread is left to finish or
/// to end with the process. A thread that can't be created skips the step,
/// which only leaves its work to the process exit.
fn bounded(deadline: Instant, step: impl FnOnce() + Send + 'static) -> bool {
    let (tx, rx) = mpsc::channel();
    let spawned = thread::Builder::new()
        .name("lectern-quit-step".into())
        .spawn(move || {
            step();
            let _ = tx.send(());
        });
    if let Err(e) = spawned {
        warn!("could not start a quit step"; error = Value::io(&e));
        return false;
    }
    matches!(
        rx.recv_timeout(deadline.saturating_duration_since(Instant::now())),
        Err(RecvTimeoutError::Timeout)
    )
}

impl QuitSteps for Started {
    fn kernel(&self, done: Box<dyn FnOnce() + Send>) {
        // The probe session first (it does not block): its `quit` request has
        // been answered, and nothing should be asked of a stopping app.
        #[cfg(feature = "ui-probe")]
        if let Some(probe) = &self.probe {
            probe.shutdown();
        }
        self.kernel.shutdown(done);
    }

    fn watcher(&self) {
        if let Some(watcher) = &self.watcher {
            watcher.shutdown();
        }
    }

    fn executor(&self, timeout: Duration) -> Shutdown {
        self.executor.shutdown(timeout)
    }

    fn cache(&self) {
        if let Some(cache) = &self.cache {
            cache.close();
        }
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

/// Who is asked before the quit order starts: the leave guards of the
/// workspace (docs/design/workspace.md「本 crate 的公开项」`ask_leave`). A
/// trait so that the tests answer in their own way.
pub trait LeaveAsk: Send + Sync + 'static {
    /// `done(true)` when every guard allowed. Any thread.
    fn ask(&self, done: Box<dyn FnOnce(bool) + Send>);
}

impl LeaveAsk for WorkspaceHost {
    fn ask(&self, done: Box<dyn FnOnce(bool) + Send>) {
        self.ask_leave(LeaveScope::App, done);
    }
}

/// The `Quit` action. Main thread, does not block. With the workspace
/// installed, its leave guards are asked first; a refusal ends the quit and
/// the app goes on.
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
    match started.shell.as_ref().map(|shell| shell.host.clone()) {
        Some(host) => ask_then(cx, Arc::new(host), shut_down),
        None => shut_down(cx),
    }
}

/// Asks `ask`, then runs `proceed` on the main thread if it allowed. A
/// refusal removes the quit marker, so ⌘Q works again. A dropped answer
/// allows, as a guard that drops its answer does (docs/design/workspace.md
/// 「服务接口」), so the app cannot be left unquittable.
pub(crate) fn ask_then(
    cx: &mut App,
    ask: Arc<dyn LeaveAsk>,
    proceed: impl FnOnce(&mut App) + 'static,
) {
    // `quit` set it already; the tests start here.
    cx.set_global(Quitting);
    let (tx, rx) = futures::channel::oneshot::channel();
    ask.ask(Box::new(move |allowed| {
        let _ = tx.send(allowed);
    }));
    cx.spawn(async move |cx| {
        let allowed = rx.await.unwrap_or(true);
        cx.update(|cx| {
            if allowed {
                proceed(cx);
            } else {
                info!("quit refused by a leave guard");
                cx.remove_global::<Quitting>();
            }
        });
    })
    .detach();
}

/// The quit order on its thread; then `cx.quit()`.
fn shut_down(cx: &mut App) {
    let Some(started) = cx.try_global::<Started>() else {
        cx.quit();
        return;
    };
    let steps: Arc<dyn QuitSteps> = Arc::new(started.clone());
    let spawned = thread::Builder::new()
        .name("lectern-quit".into())
        .spawn(move || run(steps, &Timeouts::RELEASE));
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
