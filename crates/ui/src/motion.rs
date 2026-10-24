//! Motion primitives (docs/design/ui.md「动效」): the durations and curves
//! of the interaction spec's「动效」table as named constants, cubic bezier
//! evaluation, the interruptible `Transition` (springs included) and the
//! reduced-motion rule. Every duration of this crate is defined here.
//!
//! GPUI's own `App::reduce_motion` flag stays false (this crate never sets
//! it): it would draw `with_animation`'s end state at once, while the spec
//! asks for a fade of half the duration. `Motion::animation` and
//! `Transition` apply that rule themselves from `reduced`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use gpui::{Animation, App, Pixels, SpringConfig, SpringState, Window, px, sampled_easing};

use crate::state::UiState;

/// The curve of a transition.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Easing {
    /// CSS `cubic-bezier(x1, y1, x2, y2)`. GPUI has no bezier easing; this
    /// crate evaluates it (Newton's method, at most 8 iterations, pure).
    CubicBezier([f32; 4]),
    /// What the interaction spec writes as `ease-out`: CSS's
    /// `cubic-bezier(0, 0, 0.58, 1)`.
    EaseOut,
    Linear,
    /// A spring (entering a session). `Transition` advances it frame by
    /// frame with `SpringConfig::step`, keeping its velocity, and does not
    /// restart it when the target changes; `Motion::duration` is the settle
    /// time, used only to decide the end and the fade's length under reduced
    /// motion.
    Spring(SpringConfig),
}

/// CSS `ease-out`.
const EASE_OUT: [f32; 4] = [0.0, 0.0, 0.58, 1.0];

/// A spring is settled when its displacement and velocity are within this
/// of the target (GPUI's default, which it does not export).
pub const SPRING_EPSILON: f32 = 0.001;

/// Newton's method on `x(t) = progress`: at most this many iterations.
const BEZIER_ITERATIONS: usize = 8;
const BEZIER_TOLERANCE: f32 = 1e-6;

impl Easing {
    /// Progress `0..=1` to value. A bezier may pass 1 (the switch's bounce).
    /// `Spring` uses GPUI's `sampled_easing` (sampled from rest).
    pub fn eval(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,
            Easing::EaseOut => bezier(EASE_OUT, t),
            Easing::CubicBezier(points) => bezier(points, t),
            Easing::Spring(config) => sampled_easing(config, SPRING_EPSILON).1(t),
        }
    }

    /// The closure `gpui::Animation::with_easing` takes.
    pub fn into_gpui(self) -> impl Fn(f32) -> f32 + 'static {
        move |t| self.eval(t)
    }
}

/// `y` of the unit cubic bezier at the `t` where `x(t) = x`: WebKit's
/// `UnitBezier` in polynomial form, solved with Newton's method from
/// `t = x`. Both curves of the spec have monotonic `x(t)`, so the iteration
/// converges well inside the budget; the tests compare against a bisection
/// reference.
fn bezier([x1, y1, x2, y2]: [f32; 4], x: f32) -> f32 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let cx = 3.0 * x1;
    let bx = 3.0 * (x2 - x1) - cx;
    let ax = 1.0 - cx - bx;
    let cy = 3.0 * y1;
    let by = 3.0 * (y2 - y1) - cy;
    let ay = 1.0 - cy - by;
    let mut t = x;
    for _ in 0..BEZIER_ITERATIONS {
        let error = ((ax * t + bx) * t + cx) * t - x;
        if error.abs() < BEZIER_TOLERANCE {
            break;
        }
        let slope = (3.0 * ax * t + 2.0 * bx) * t + cx;
        if slope.abs() < BEZIER_TOLERANCE {
            break;
        }
        t -= error / slope;
    }
    let t = t.clamp(0.0, 1.0);
    ((ay * t + by) * t + cy) * t
}

/// A named transition: one row of the interaction spec's「动效」table.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Motion {
    pub duration: Duration,
    pub delay: Duration,
    pub easing: Easing,
    /// Whether this transition moves an element. Under reduced motion a
    /// moving transition becomes a fade of half the duration; a non-moving
    /// one only has its duration halved.
    pub moves: bool,
}

const fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

const fn motion(duration_ms: u64, easing: Easing, moves: bool) -> Motion {
    Motion {
        duration: ms(duration_ms),
        delay: Duration::ZERO,
        easing,
        moves,
    }
}

/// `cubic-bezier(0.32, 0.72, 0, 1)`: the curve of most rows.
pub const STANDARD: Easing = Easing::CubicBezier([0.32, 0.72, 0.0, 1.0]);

/// The context panel slides in from the right.
pub const PANEL_OPEN: Motion = motion(380, STANDARD, true);
/// The panel slides out the way it came.
pub const PANEL_CLOSE: Motion = motion(280, STANDARD, true);
/// Marking a file as seen: the circle fills.
pub const CHECK_FILL: Motion = motion(180, STANDARD, false);
/// Marking a file as seen: the tick is stroked, 60 ms after the fill starts.
pub const CHECK_STROKE: Motion = Motion {
    duration: ms(220),
    delay: ms(60),
    easing: STANDARD,
    moves: false,
};
/// The seen-progress bar grows.
pub const PROGRESS: Motion = motion(360, STANDARD, false);
/// Seen-and-next: the reading pane scrolls to the next file's top. The
/// list does the scrolling; this gives the duration and the curve.
pub const SCROLL_NEXT: Motion = motion(300, Easing::EaseOut, true);
/// The comment box opens below the line: its height.
pub const EXPAND_HEIGHT: Motion = motion(280, STANDARD, true);
/// The comment box opens below the line: its content fades in.
pub const EXPAND_FADE: Motion = motion(200, STANDARD, false);
/// The segmented control's slider moves to the new option.
pub const SEGMENT: Motion = motion(260, STANDARD, true);
/// The switch's knob moves with a slight bounce at the end.
pub const SWITCH: Motion = motion(240, Easing::CubicBezier([0.34, 1.3, 0.64, 1.0]), true);
/// An agent's new round arrives: it fades in from `ARRIVE_OFFSET` below.
pub const ARRIVE: Motion = motion(240, Easing::EaseOut, true);
/// How far below its place a new round starts.
pub const ARRIVE_OFFSET: Pixels = px(8.0);
/// Hover: the background fades in.
pub const HOVER_IN: Motion = motion(120, Easing::Linear, false);
/// Hover: the background fades out, a little slower.
pub const HOVER_OUT: Motion = motion(200, Easing::Linear, false);

/// The loading state's phases (interaction spec「状态与异常」, `loading_phase`):
/// nothing is drawn before this much has elapsed …
pub const LOADING_SKELETON_AT: Duration = ms(100);
/// … a skeleton until this much, a progress indicator after it.
pub const LOADING_PROGRESS_AT: Duration = Duration::from_secs(1);
/// How long a `Hint` stays before it fades out (`HOVER_OUT`).
pub const HINT_DURATION: Duration = ms(2500);

/// Entering a session: spring, response 0.35 s, damping 0.86. With mass 1:
/// stiffness = (2π / 0.35)² ≈ 322.27, damping = 2 × 0.86 × √stiffness
/// ≈ 30.877.
pub const ENTER_SESSION_SPRING: SpringConfig = SpringConfig::new(322.27, 30.877, 1.0);

/// `ENTER_SESSION_SPRING`'s settle time from rest with `SPRING_EPSILON`,
/// as `SpringConfig::settle_time` (not a const fn) computes it: about
/// 0.49 s. The test `spring_constants_match_the_spec` holds the two
/// together.
const ENTER_SESSION_SETTLE: Duration = Duration::from_micros(491_013);

/// Entering a session: the list row's title moves and grows into the
/// review page's title, the rest fades in.
pub const ENTER_SESSION: Motion = Motion {
    duration: ENTER_SESSION_SETTLE,
    delay: Duration::ZERO,
    easing: Easing::Spring(ENTER_SESSION_SPRING),
    moves: true,
};

/// Whether the system's reduce-motion setting is on
/// (`platform::appearance::reduce_motion`, updated by its notification).
/// Unknown (before `init`) counts as off.
pub fn reduced(cx: &App) -> bool {
    cx.try_global::<UiState>()
        .is_some_and(|state| state.reduced_motion)
}

/// A transition after the reduced-motion rule.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Effective {
    /// Played as it is.
    Move(Motion),
    /// The displacement becomes a fade: the caller sets the offset to 0
    /// and applies this transition to the opacity alone.
    Fade(Motion),
}

impl Effective {
    /// The transition to play, whichever kind it is.
    pub fn motion(self) -> Motion {
        match self {
            Effective::Move(motion) | Effective::Fade(motion) => motion,
        }
    }
}

impl Motion {
    /// The transition actually used once reduced motion is considered:
    /// when `reduced`, the duration is halved (the delay stays) and a
    /// `moves` transition becomes `Fade`.
    pub fn effective(self, cx: &App) -> Effective {
        self.effective_when(reduced(cx))
    }

    /// `effective` for a given setting (pure; the controls' frame functions).
    pub(crate) fn effective_when(self, reduced: bool) -> Effective {
        if !reduced {
            return Effective::Move(self);
        }
        let halved = Motion {
            duration: self.duration / 2,
            ..self
        };
        if self.moves {
            Effective::Fade(Motion {
                moves: false,
                ..halved
            })
        } else {
            Effective::Move(halved)
        }
    }

    /// A one-shot GPUI animation (for `with_animation`), already adjusted
    /// by `effective`. Only for short animations that need no interruption
    /// (check, hover, arrive). GPUI's `Animation` has no delay, so the
    /// delay is folded in: the animation lasts `delay + duration` and its
    /// easing stays at 0 until the delay has passed.
    pub fn animation(self, cx: &App) -> Animation {
        let motion = self.effective(cx).motion();
        if motion.delay.is_zero() {
            return Animation::new(motion.duration).with_easing(motion.easing.into_gpui());
        }
        let total = motion.delay + motion.duration;
        let lead = motion.delay.as_secs_f32() / total.as_secs_f32();
        let easing = motion.easing;
        Animation::new(total).with_easing(move |t| {
            if t <= lead {
                0.0
            } else {
                easing.eval((t - lead) / (1.0 - lead))
            }
        })
    }
}

/// How many `Transition`s are in progress in this process. Tests run one
/// per process under nextest, so each sees its own count.
static ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// The number of `Transition`s in progress (springs included). `workspace`'s
/// frame recording uses it to tell that every animation has stopped
/// (`RecordStop::AnimationsSettled`). It counts `Transition`s only: the
/// one-shot GPUI animations of `Motion::animation` (check, hover, arrive)
/// are not counted; they last at most 240 ms, so a caller waits
/// `ARRIVE.duration` after the count reaches zero. The count drops in
/// `tick`, so a holder that stops rendering before the end (without
/// dropping) keeps its transition counted; the bench scenes render until
/// their stop condition holds, so they never meet this (known limitation).
pub fn active_transitions(_cx: &App) -> usize {
    ACTIVE.load(Ordering::Relaxed)
}

/// An interruptible scalar transition: a panel's offset, a progress value,
/// an expanding height, the title's displacement when entering a session.
/// A view keeps it in its own state and calls `tick` every frame.
///
/// A new target continues from the current value and the current
/// progress (the spec's「随时可打断」): no queue, no jump back to the start;
/// a spring keeps its velocity. The number in progress is kept in a
/// process-level count (`go` adds one, `tick` on arrival, `set` and drop
/// take one away) for `active_transitions`.
#[derive(Debug)]
pub struct Transition {
    from: f32,
    to: f32,
    /// The value at `at`.
    value: f32,
    /// The spring's velocity at `at`.
    velocity: f32,
    /// When the current leg started (`go`).
    start: Instant,
    /// When `value` and `velocity` were last computed.
    at: Instant,
    /// The leg's transition, after `effective`.
    motion: Motion,
    /// The reduce-motion setting when the leg started; a leg plays to the
    /// end the way it began.
    reduced: bool,
    running: bool,
    /// Whether this transition is in the process-level count.
    counted: bool,
}

impl Transition {
    pub fn new(value: f32) -> Transition {
        let now = Instant::now();
        Transition {
            from: value,
            to: value,
            value,
            velocity: 0.0,
            start: now,
            at: now,
            motion: PANEL_OPEN,
            reduced: false,
            running: false,
            counted: false,
        }
    }

    /// Sets a new target. `now` comes from the caller's `Instant::now()`,
    /// so tests control it. While in progress the current value becomes
    /// the new start and the clock restarts; the same target changes
    /// nothing.
    pub fn go(&mut self, target: f32, motion: Motion, now: Instant, cx: &App) {
        if target == self.to && (self.running || self.value == self.to) {
            return;
        }
        if self.running {
            let (value, velocity, _) = self.sample(now);
            self.value = value;
            self.velocity = velocity;
        }
        let reduced = reduced(cx);
        self.from = self.value;
        self.to = target;
        self.start = now;
        self.at = now;
        self.motion = motion.effective_when(reduced).motion();
        self.reduced = reduced;
        if !self.is_spring() {
            self.velocity = 0.0;
        }
        self.running = true;
        if !self.counted {
            self.counted = true;
            ACTIVE.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Arrives at once, without a transition.
    pub fn set(&mut self, value: f32) {
        self.from = value;
        self.to = value;
        self.value = value;
        self.velocity = 0.0;
        self.running = false;
        self.uncount();
    }

    /// The current value, for rendering; while in progress it also
    /// requests the next animation frame. Its own work is a constant-time
    /// computation without allocation (the frame request is GPUI's).
    pub fn tick(&mut self, now: Instant, window: &mut Window) -> f32 {
        if !self.running {
            return self.value;
        }
        let (value, velocity, done) = self.sample(now);
        self.at = now;
        if done {
            self.value = self.to;
            self.velocity = 0.0;
            self.running = false;
            self.uncount();
        } else {
            self.value = value;
            self.velocity = velocity;
            window.request_animation_frame();
        }
        self.value
    }

    /// The value of the last `tick` (or `set`, or `new`).
    pub fn value(&self) -> f32 {
        self.value
    }

    pub fn target(&self) -> f32 {
        self.to
    }

    pub fn is_running(&self, now: Instant) -> bool {
        self.running && !self.sample(now).2
    }

    /// A spring is integrated step by step; a timed leg is a function of
    /// the elapsed time. Under reduced motion a spring plays as a timed
    /// fade with its sampled easing.
    fn is_spring(&self) -> bool {
        matches!(self.motion.easing, Easing::Spring(_)) && !self.reduced
    }

    /// `(value, velocity, done)` at `now`, without committing. Pure.
    fn sample(&self, now: Instant) -> (f32, f32, bool) {
        if let (Easing::Spring(config), false) = (self.motion.easing, self.reduced) {
            let state = SpringState {
                position: self.value,
                velocity: self.velocity,
            };
            let state = config.step(state, self.to, now.duration_since(self.at).as_secs_f32());
            // The settle time from rest is a conservative bound; twice it
            // is the backstop for a leg that was retargeted with velocity.
            let done = config.is_settled(state, self.to, SPRING_EPSILON)
                || now.duration_since(self.start) >= self.motion.duration * 2;
            return (state.position, state.velocity, done);
        }
        let elapsed = now.duration_since(self.start);
        if elapsed < self.motion.delay {
            return (self.from, 0.0, false);
        }
        let active = elapsed - self.motion.delay;
        if active >= self.motion.duration {
            return (self.to, 0.0, true);
        }
        let progress = active.as_secs_f32() / self.motion.duration.as_secs_f32();
        let value = self.from + (self.to - self.from) * self.motion.easing.eval(progress);
        (value, 0.0, false)
    }

    fn uncount(&mut self) {
        if self.counted {
            self.counted = false;
            ACTIVE.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

impl Drop for Transition {
    fn drop(&mut self) {
        self.uncount();
    }
}
