//! `ui/*` (docs/design/ui.md「性能」): the motion primitives of step 2.
//! GPUI only builds on macOS in this workspace, so this module is compiled
//! there alone; the budgets are `macos_only` and skipped elsewhere.

use std::cell::RefCell;
use std::hint::black_box;
use std::time::{Duration, Instant};

use gpui::{App, Context, IntoElement, Render, TestAppContext, Window, WindowHandle, div};
use lectern_bench::budgets::fmt_duration;
use lectern_bench::{Bench, BenchError, Ctx, Measured, Samples, Value};
use lectern_i18n::{Locale, Strings};
use lectern_ui::motion::{ENTER_SESSION, Motion, PANEL_OPEN, STANDARD, Transition};
use lectern_ui::testing::step_transition;
use lectern_ui::{Appearance, init_fixed};

pub const BENCHES: &[Bench] = &[TRANSITION_TICK, TRANSITION_TICK_FRAME, BEZIER_EVAL];

/// `Easing::eval` of `STANDARD` over 1000 points of the curve.
pub const BEZIER_EVAL: Bench = Bench {
    id: "ui/bezier_eval",
    run: bezier_eval,
};

fn bezier_eval(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let mut i = 0u32;
    let time = ctx.time(|| {
        i = (i + 1) % 1000;
        STANDARD.eval(black_box(i as f32 / 1000.0))
    });
    Ok(vec![Measured::new("ui/bezier_eval", Value::Time(time))])
}

/// The simulated time between two ticks, and how often the transition is
/// re-aimed at the other end so that it never arrives (both legs are
/// shorter than `PANEL_OPEN` and the spring's settle time).
const STEP: Duration = Duration::from_micros(10);
const LEG: Duration = Duration::from_millis(100);

/// A root view that ticks one `Transition` `ticks` times per render:
/// `Transition::tick` requests the next animation frame, which GPUI allows
/// only from a view's render, so the ticks are timed as the difference
/// between a render with `ticks` of them and a render with none.
struct Ticker {
    transition: Transition,
    motion: Motion,
    now: Instant,
    leg_start: Instant,
    target: f32,
    ticks: u32,
}

impl Ticker {
    fn new(motion: Motion) -> Ticker {
        let now = Instant::now();
        Ticker {
            transition: Transition::new(0.0),
            motion,
            now,
            // Before the first leg, so that the first tick re-aims at 1.
            leg_start: now - LEG,
            target: 0.0,
            ticks: 0,
        }
    }
}

impl Render for Ticker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for _ in 0..self.ticks {
            self.now += STEP;
            if self.now.duration_since(self.leg_start) >= LEG {
                self.leg_start = self.now;
                self.target = 1.0 - self.target;
                self.transition.go(self.target, self.motion, self.now, cx);
            }
            black_box(self.transition.tick(self.now, window));
        }
        div()
    }
}

/// `Transition::tick` of a running `PANEL_OPEN` (bezier) transition without
/// GPUI's frame request: the transition's own work, which ui.md「性能」
/// constrains. The note carries the `ENTER_SESSION` spring.
pub const TRANSITION_TICK: Bench = Bench {
    id: "ui/transition_tick",
    run: transition_tick,
};

fn transition_tick(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let cx = TestAppContext::single();
    cx.update(|cx| init_fixed(cx, Strings::of(Locale::En), Appearance::Light, false));
    let (bezier, spring) =
        cx.update(|cx| (steps(ctx, cx, PANEL_OPEN), steps(ctx, cx, ENTER_SESSION)));
    let mut measured = Measured::new("ui/transition_tick", Value::Time(bezier));
    measured.note = Some(format!(
        "ENTER_SESSION spring: median {}",
        fmt_duration(median(&spring))
    ));
    Ok(vec![measured])
}

/// One step per call, re-aimed at the other end every `LEG` as `Ticker` does.
fn steps(ctx: &mut Ctx, cx: &App, motion: Motion) -> Samples {
    let mut transition = Transition::new(0.0);
    let mut now = Instant::now();
    let mut leg_start = now - LEG;
    let mut target = 0.0;
    ctx.time(|| {
        now += STEP;
        if now.duration_since(leg_start) >= LEG {
            leg_start = now;
            target = 1.0 - target;
            transition.go(target, motion, now, cx);
        }
        step_transition(&mut transition, black_box(now))
    })
}

/// `Transition::tick` with GPUI's frame request (a boxed next-frame
/// callback, the platform wake-up and the `notify` it runs), recorded only:
/// the cost is GPUI's and varies with the runner; the frame budget itself is
/// `app/animation_frames`.
pub const TRANSITION_TICK_FRAME: Bench = Bench {
    id: "ui/transition_tick_frame",
    run: transition_tick_frame,
};

fn transition_tick_frame(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let mut cx = TestAppContext::single();
    cx.update(|cx| init_fixed(cx, Strings::of(Locale::En), Appearance::Light, false));
    let window = cx.add_window(|_, _| Ticker::new(PANEL_OPEN));
    cx.run_until_parked();
    let bezier = ticks(ctx, &mut cx, &window, PANEL_OPEN)?;
    let spring = ticks(ctx, &mut cx, &window, ENTER_SESSION)?;
    let mut measured = Measured::new("ui/transition_tick_frame", Value::Time(bezier));
    measured.note = Some(format!(
        "ENTER_SESSION spring: median {}",
        fmt_duration(median(&spring))
    ));
    Ok(vec![measured])
}

/// Ticks per render: about 100 ns each against a render of a few tens of
/// microseconds, so the difference is the ticks.
const TICKS: u32 = 4096;

/// One sample per pair of renders: `(with ticks − without) / TICKS`.
fn ticks(
    ctx: &mut Ctx,
    cx: &mut TestAppContext,
    window: &WindowHandle<Ticker>,
    motion: Motion,
) -> Result<Samples, BenchError> {
    window
        .update(cx, |ticker, _, _| {
            ticker.motion = motion;
            ticker.transition.set(0.0);
            ticker.target = 0.0;
            ticker.leg_start = ticker.now - LEG;
        })
        .map_err(|e| BenchError::debug("window", e))?;
    let cx = RefCell::new(cx);
    let render = |ticks: u32| {
        let mut cx = cx.borrow_mut();
        window
            .update(*cx, |ticker, _, cx| {
                ticker.ticks = ticks;
                cx.notify();
            })
            .expect("the window is open");
        cx.run_until_parked();
        // The frame requests of that render (none without ticks) run now,
        // and the render they ask for ticks nothing.
        window
            .update(*cx, |ticker, window, cx| {
                ticker.ticks = 0;
                window.simulate_next_frame(cx)
            })
            .expect("the window is open");
        cx.run_until_parked();
    };
    let (subject, baseline) = ctx.compare(|| render(TICKS), || render(0));
    Ok(Samples(
        subject
            .0
            .iter()
            .zip(&baseline.0)
            .map(|(s, b)| s.saturating_sub(*b) / TICKS)
            .collect(),
    ))
}

fn median(samples: &Samples) -> Duration {
    let mut sorted = samples.0.clone();
    sorted.sort();
    sorted.get(sorted.len() / 2).copied().unwrap_or_default()
}
