//! The empty GPUI window of the `baseline/*` benchmarks
//! (docs/design/bench.md「进程级基准」, ADR 0017「影响」): one window of the
//! Welcome size that draws nothing, with the probe and no kernel. `just
//! bench` builds it on macOS and passes its path in
//! `LECTERN_BENCH_BASELINE`. On other platforms `main` returns at once.

#[cfg(target_os = "macos")]
fn main() {
    window::run();
}

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
mod window {
    use std::sync::Arc;

    use futures::StreamExt;
    use futures::channel::mpsc::{self, UnboundedSender};
    use gpui::{
        App, Bounds, Context, IntoElement, Render, Window, WindowBounds, WindowOptions, div,
        prelude::*, px, size,
    };
    use lectern_bench::probe::{Driver, Probe};
    use lectern_bench::wire::OpenTarget;

    /// The Welcome window's size (crates/app/src/window.rs), so that the
    /// baseline draws as many pixels as `app/cold_start`.
    const WIDTH: f32 = 820.0;
    const HEIGHT: f32 = 520.0;

    struct Empty;

    impl Render for Empty {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
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
        // Before the first frame: two environment variables and one method
        // replacement, no IO and no thread.
        let probe = Probe::from_env();
        if let Some(probe) = &probe {
            probe.watch_first_frame();
        }
        gpui_platform::application().run(move |cx: &mut App| {
            let bounds = Bounds::centered(None, size(px(WIDTH), px(HEIGHT)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            };
            // Opening fails only without a display; the runner's first-frame
            // deadline then records the error.
            let _ = cx.open_window(options, |_, cx| cx.new(|_| Empty));
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
            }
            cx.activate(true);
        });
    }
}
