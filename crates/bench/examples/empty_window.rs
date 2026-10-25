//! The empty GPUI window of the `baseline/*` benchmarks
//! (docs/design/bench.md「进程级基准」, ADR 0017「影响」): one window of the
//! Welcome size that draws nothing, with the probe and no kernel. `just
//! bench` builds it on macOS and passes its path in
//! `LECTERN_BENCH_BASELINE`. On other platforms `main` returns at once.
//!
//! TEMPORARY CI diagnostics (bench step 4, removed before the review): the
//! `diag` module writes the window's state and the hook's counters to
//! stderr once a second. With `LECTERN_BENCH_DIAG=natural|continuous` and
//! no socket it registers the first-frame hook itself, draws one frame or
//! frames without end, and quits after 8 s.

#[cfg(target_os = "macos")]
fn main() {
    window::run();
}

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
mod diag {
    use std::io::{self, Write};
    use std::sync::atomic::{AtomicU8, AtomicU64, Ordering::Relaxed};
    use std::thread;
    use std::time::{Duration, Instant};

    pub static RENDERS: AtomicU64 = AtomicU64::new(0);
    /// 0 unknown, 1 visible, 2 hidden.
    pub static VISIBLE: AtomicU8 = AtomicU8::new(0);
    /// 0 unknown, 1 active, 2 not active.
    pub static ACTIVE: AtomicU8 = AtomicU8::new(0);
    /// Milliseconds after the start at which the first-frame callback came.
    pub static PRESENTED_MS: AtomicU64 = AtomicU64::new(0);

    pub fn say(line: &str) {
        let _ = writeln!(
            io::stderr(),
            "lectern-bench diag [{}]: {line}",
            std::process::id()
        );
    }

    /// `LECTERN_BENCH_DIAG`, when set.
    pub fn mode() -> Option<String> {
        std::env::var("LECTERN_BENCH_DIAG").ok()
    }

    /// The state once a second, 12 times.
    pub fn report(start: Instant) {
        thread::spawn(move || {
            for _ in 0..12 {
                thread::sleep(Duration::from_secs(1));
                let [next, null, handlers, zero, last] = lectern_platform::frame::diag_counts();
                say(&format!(
                    "t {:.1} s: renders {} visible {} active {} next {next} null {null} \
                     handlers {handlers} zero {zero} last_presented {} presented_ms {}",
                    start.elapsed().as_secs_f64(),
                    RENDERS.load(Relaxed),
                    VISIBLE.load(Relaxed),
                    ACTIVE.load(Relaxed),
                    f64::from_bits(last),
                    PRESENTED_MS.load(Relaxed),
                ));
            }
        });
    }

    struct Logger;

    impl log::Log for Logger {
        fn enabled(&self, metadata: &log::Metadata) -> bool {
            metadata.level() <= log::Level::Info
        }

        fn log(&self, record: &log::Record) {
            if self.enabled(record.metadata()) {
                say(&format!(
                    "log {} {}: {}",
                    record.level(),
                    record.target(),
                    record.args()
                ));
            }
        }

        fn flush(&self) {}
    }

    /// GPUI's own `log` records (such as "failed to retrieve next
    /// drawable") to stderr.
    pub fn logger() {
        static LOGGER: Logger = Logger;
        if log::set_logger(&LOGGER).is_ok() {
            log::set_max_level(log::LevelFilter::Info);
        }
    }
}

#[cfg(target_os = "macos")]
mod window {
    use std::sync::Arc;
    use std::sync::atomic::Ordering::Relaxed;
    use std::time::{Duration, Instant};

    use futures::StreamExt;
    use futures::channel::mpsc::{self, UnboundedSender};
    use gpui::{
        App, Bounds, Context, IntoElement, Render, Window, WindowBounds, WindowOptions,
        WindowVisibility, div, prelude::*, px, size,
    };
    use lectern_bench::probe::{Driver, Probe};
    use lectern_bench::wire::OpenTarget;

    use crate::diag;

    /// The Welcome window's size (crates/app/src/window.rs), so that the
    /// baseline draws as many pixels as `app/cold_start`.
    const WIDTH: f32 = 820.0;
    const HEIGHT: f32 = 520.0;

    struct Empty {
        continuous: bool,
    }

    impl Render for Empty {
        fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let renders = diag::RENDERS.fetch_add(1, Relaxed) + 1;
            let visible = if window.visibility() == WindowVisibility::Visible {
                1
            } else {
                2
            };
            diag::VISIBLE.store(visible, Relaxed);
            diag::ACTIVE.store(if window.is_window_active() { 1 } else { 2 }, Relaxed);
            if renders <= 3 {
                diag::say(&format!(
                    "render {renders}: bounds {:?} scale {} visible {visible}",
                    window.bounds(),
                    window.scale_factor()
                ));
            }
            if self.continuous {
                window.request_animation_frame();
            }
            div().size_full()
        }
    }

    /// The probe thread's requests. `Quit` goes to the main thread through
    /// a channel; there is nothing to open in an empty window.
    struct Requests {
        quit: UnboundedSender<()>,
    }

    impl Driver for Requests {
        fn open(&self, _: OpenTarget) {}

        fn quit(&self) {
            let _ = self.quit.unbounded_send(());
        }
    }

    pub(super) fn run() {
        let start = Instant::now();
        // Before the first frame: two environment variables and one method
        // replacement, no IO and no thread.
        let probe = Probe::from_env();
        if let Some(probe) = &probe {
            probe.watch_first_frame();
        }
        diag::logger();
        let mode = diag::mode();
        let continuous = mode.as_deref() == Some("continuous");
        if probe.is_none() && mode.is_some() {
            let registered = lectern_platform::frame::on_first_presented(move |_| {
                let ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
                diag::PRESENTED_MS.store(ms.max(1), Relaxed);
            });
            diag::say(&format!("mode {mode:?}: hook {registered:?}"));
        }
        diag::say(&format!("start: probe {} mode {mode:?}", probe.is_some()));
        diag::report(start);
        gpui_platform::application().run(move |cx: &mut App| {
            let displays = cx.displays();
            diag::say(&format!(
                "displays {}: primary {:?}",
                displays.len(),
                cx.primary_display().map(|d| d.bounds())
            ));
            let bounds = Bounds::centered(None, size(px(WIDTH), px(HEIGHT)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            };
            // Opening fails only without a display; the runner's first-frame
            // deadline then records the error.
            let opened = cx.open_window(options, |_, cx| cx.new(|_| Empty { continuous }));
            diag::say(&format!("open_window ok {}", opened.is_ok()));
            if let Err(error) = &opened {
                diag::say(&format!("open_window error {error:#}"));
            }
            if let Some(probe) = probe {
                let (quit, mut requests) = mpsc::unbounded();
                cx.spawn(async move |cx| {
                    if requests.next().await.is_some() {
                        cx.update(|cx| cx.quit());
                    }
                })
                .detach();
                // The first-frame callback and the probe thread keep what
                // they need; `probe` itself may go.
                probe.drive(Arc::new(Requests { quit }));
            } else if mode.is_some() {
                let timer = cx.background_executor().timer(Duration::from_secs(8));
                cx.spawn(async move |cx| {
                    timer.await;
                    cx.update(|cx| cx.quit());
                })
                .detach();
            }
            cx.activate(true);
        });
    }
}
