//! Step 2 (docs/design/ui.md「测试」): `bezier_*`, `spring_*`,
//! `transition_*`, `active_transitions_*` and
//! `reduced_motion_has_no_displacement`.

use std::cell::{Cell, RefCell};
use std::f32::consts::PI;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{App, Context, IntoElement, Render, SpringState, TestAppContext, Window, div, px};
use lectern_i18n::{Locale, Strings};

use crate::motion::{
    ARRIVE_OFFSET, CHECK_FILL, CHECK_STROKE, ENTER_SESSION, ENTER_SESSION_SPRING, Easing,
    Effective, Motion, PANEL_CLOSE, PANEL_OPEN, SPRING_EPSILON, STANDARD, SWITCH, Transition,
    active_transitions, reduced,
};
use crate::{Appearance, Expander, Segmented, init_fixed};

/// The reference values of `y` at `x = 0, 0.1, …, 1` of the two bezier
/// curves of the spec and CSS `ease-out`, from a bisection on the unit
/// bezier in f64 (WebKit's `UnitBezier` solves the same curve).
const STANDARD_REFERENCE: [f32; 11] = [
    0.000000, 0.269821, 0.655694, 0.847154, 0.918410, 0.954809, 0.975778, 0.988238, 0.995393,
    0.998969, 1.000000,
];
const SWITCH_REFERENCE: [f32; 11] = [
    0.000000, 0.341197, 0.603200, 0.792588, 0.918339, 0.991423, 1.024143, 1.029329, 1.019539,
    1.006373, 1.000000,
];
const EASE_OUT_REFERENCE: [f32; 11] = [
    0.000000, 0.160572, 0.308366, 0.445186, 0.570880, 0.684643, 0.785139, 0.870423, 0.937718,
    0.982973, 1.000000,
];

fn assert_matches(easing: Easing, reference: &[f32; 11]) {
    for (i, expected) in reference.iter().enumerate() {
        let x = i as f32 / 10.0;
        let y = easing.eval(x);
        assert!(
            (y - expected).abs() <= 1e-3,
            "{easing:?} at {x}: {y} vs {expected}"
        );
    }
}

#[test]
fn bezier_matches_the_css_reference() {
    assert_matches(STANDARD, &STANDARD_REFERENCE);
    assert_matches(SWITCH.easing, &SWITCH_REFERENCE);
    assert_matches(Easing::EaseOut, &EASE_OUT_REFERENCE);
}

#[test]
fn bezier_ends_at_0_and_1() {
    for easing in [STANDARD, SWITCH.easing, Easing::EaseOut, Easing::Linear] {
        assert_eq!(easing.eval(0.0), 0.0, "{easing:?}");
        assert_eq!(easing.eval(1.0), 1.0, "{easing:?}");
        // Progress outside 0..=1 is clamped.
        assert_eq!(easing.eval(-0.5), 0.0, "{easing:?}");
        assert_eq!(easing.eval(1.5), 1.0, "{easing:?}");
    }
}

#[test]
fn bezier_switch_overshoots_near_0_6() {
    assert!(SWITCH.easing.eval(0.6) > 1.0);
    assert!(SWITCH.easing.eval(0.7) > 1.0);
    assert!(STANDARD.eval(0.6) < 1.0);
    let gpui = SWITCH.easing.into_gpui();
    assert_eq!(gpui(0.6), SWITCH.easing.eval(0.6));
}

#[test]
fn spring_constants_match_the_spec() {
    let (natural_frequency, damping_ratio) = ENTER_SESSION_SPRING.canonical();
    assert!((damping_ratio - 0.86).abs() <= 0.005, "{damping_ratio}");
    assert!(
        (natural_frequency - 2.0 * PI / 0.35).abs() <= 0.01,
        "{natural_frequency}"
    );
    assert_eq!(ENTER_SESSION.easing, Easing::Spring(ENTER_SESSION_SPRING));
    let Motion { moves, .. } = ENTER_SESSION;
    assert!(moves);
    let rest = SpringState {
        position: 0.0,
        velocity: 0.0,
    };
    let settle = ENTER_SESSION_SPRING.settle_time(rest, 1.0, SPRING_EPSILON);
    let constant = ENTER_SESSION.duration;
    assert!(
        settle.abs_diff(constant) <= Duration::from_millis(1),
        "settle time {settle:?}, constant {constant:?}"
    );
    // The sampled easing starts at rest and ends at the target.
    assert_eq!(ENTER_SESSION.easing.eval(0.0), 0.0);
    assert_eq!(ENTER_SESSION.easing.eval(1.0), 1.0);
}

#[test]
fn spec_constants_hold_the_table() {
    assert_eq!(PANEL_OPEN.duration, Duration::from_millis(380));
    assert_eq!(PANEL_CLOSE.duration, Duration::from_millis(280));
    assert_eq!(CHECK_STROKE.delay, Duration::from_millis(60));
    assert_eq!(CHECK_STROKE.duration, Duration::from_millis(220));
    assert_eq!(ARRIVE_OFFSET, gpui::px(8.0));
    assert_eq!((PANEL_OPEN.moves, CHECK_FILL.moves), (true, false));
}

/// A root view whose first render runs the test body: `Transition::tick`
/// requests the next animation frame, which GPUI only allows from a view's
/// render (it notifies the view being rendered).
struct Probe(Rc<RefCell<Option<Body>>>);

type Body = Box<dyn FnOnce(&mut Window, &App)>;

impl Render for Probe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(body) = self.0.borrow_mut().take() {
            body(window, cx);
        }
        div()
    }
}

/// `init_fixed` with `reduced_motion`, then `body` inside a render.
pub(super) fn in_render(
    cx: &mut TestAppContext,
    reduced_motion: bool,
    body: impl FnOnce(&mut Window, &App) + 'static,
) {
    cx.update(|cx| {
        init_fixed(
            cx,
            Strings::of(Locale::En),
            Appearance::Light,
            reduced_motion,
        );
    });
    let ran = Rc::new(Cell::new(false));
    let body: Body = Box::new({
        let ran = ran.clone();
        move |window, cx| {
            body(window, cx);
            ran.set(true);
        }
    });
    let slot = Rc::new(RefCell::new(Some(body)));
    let _window = cx.add_window(move |_, _| Probe(slot));
    cx.run_until_parked();
    assert!(ran.get(), "the body ran in a render");
}

pub(super) fn at(start: Instant, ms: u64) -> Instant {
    start + Duration::from_millis(ms)
}

#[gpui::test]
fn spring_keeps_its_velocity_when_retargeted(cx: &mut TestAppContext) {
    in_render(cx, false, |window, cx| {
        let start = Instant::now();
        let mut t = Transition::new(0.0);
        t.go(1.0, ENTER_SESSION, start, cx);
        let v100 = t.tick(at(start, 100), window);
        assert!(v100 > 0.0 && v100 < 1.0, "{v100}");
        t.go(0.0, ENTER_SESSION, at(start, 100), cx);
        assert_eq!(t.target(), 0.0);

        // The next frame still moves toward 1: the velocity is kept.
        let v108 = t.tick(at(start, 108), window);
        assert!(v108 > v100, "{v108} after {v100}");

        // Then it turns back, without a jump, and settles at 0 (the
        // damping of 0.86 lets it pass 0 by a few thousandths first).
        let mut previous = v108;
        let mut turned = false;
        let mut ms = 116;
        while t.is_running(at(start, ms)) {
            let value = t.tick(at(start, ms), window);
            assert!((value - previous).abs() < 0.1, "jump at {ms} ms");
            if value < previous {
                turned = true;
            } else {
                assert!(
                    !turned || value.abs() < 0.01,
                    "value rose again at {ms} ms: {value}"
                );
            }
            previous = value;
            ms += 8;
        }
        assert!(turned);
        assert!(ms < 1500, "settled at {ms} ms");
        assert_eq!(t.tick(at(start, ms), window), 0.0);
    });
}

#[gpui::test]
fn transition_continues_from_the_current_value(cx: &mut TestAppContext) {
    in_render(cx, false, |window, cx| {
        let start = Instant::now();
        let mut t = Transition::new(0.0);
        t.go(1.0, PANEL_OPEN, start, cx);
        let half = t.tick(at(start, 190), window);
        assert!(half > 0.5 && half < 1.0, "{half}");

        t.go(0.0, PANEL_CLOSE, at(start, 190), cx);
        assert_eq!(t.value(), half, "no jump at the retarget");
        // STANDARD's steepest 8 ms frame covers about 10% of the range.
        let mut previous = half;
        for ms in (198..=470).step_by(8) {
            let value = t.tick(at(start, ms), window);
            assert!(value <= previous, "{value} after {previous} at {ms} ms");
            assert!(previous - value < 0.15, "jump at {ms} ms");
            previous = value;
        }
        assert_eq!(t.value(), 0.0);
        assert!(!t.is_running(at(start, 470)));
    });
}

#[gpui::test]
fn transition_same_target_does_not_restart(cx: &mut TestAppContext) {
    in_render(cx, false, |window, cx| {
        let start = Instant::now();
        let mut t = Transition::new(0.0);
        t.go(1.0, PANEL_OPEN, start, cx);
        t.tick(at(start, 100), window);
        t.go(1.0, PANEL_OPEN, at(start, 100), cx);
        let value = t.tick(at(start, 200), window);

        let mut fresh = Transition::new(0.0);
        fresh.go(1.0, PANEL_OPEN, start, cx);
        assert_eq!(value, fresh.tick(at(start, 200), window));
        assert_eq!(t.tick(at(start, 380), window), 1.0);
        assert!(!t.is_running(at(start, 380)));

        // At rest at the target: nothing starts.
        t.go(1.0, PANEL_OPEN, at(start, 400), cx);
        assert!(!t.is_running(at(start, 400)));
    });
}

#[gpui::test]
fn transition_set_arrives_at_once(cx: &mut TestAppContext) {
    in_render(cx, false, |window, cx| {
        let start = Instant::now();
        let mut t = Transition::new(0.0);
        t.go(1.0, PANEL_OPEN, start, cx);
        t.tick(at(start, 50), window);
        t.set(0.25);
        assert_eq!(t.value(), 0.25);
        assert_eq!(t.target(), 0.25);
        assert!(!t.is_running(at(start, 50)));
        assert_eq!(t.tick(at(start, 300), window), 0.25);
    });
}

#[gpui::test]
fn transition_delay_holds_the_start_value(cx: &mut TestAppContext) {
    in_render(cx, false, |window, cx| {
        let start = Instant::now();
        let mut t = Transition::new(0.0);
        t.go(1.0, CHECK_STROKE, start, cx);
        assert_eq!(t.tick(at(start, 30), window), 0.0);
        assert!(t.is_running(at(start, 30)));
        let value = t.tick(at(start, 170), window);
        assert!(value > 0.0 && value < 1.0, "{value}");
        assert_eq!(t.tick(at(start, 280), window), 1.0);
    });
}

#[gpui::test]
fn transition_reduced_motion_halves_and_fades(cx: &mut TestAppContext) {
    in_render(cx, true, |window, cx| {
        assert!(reduced(cx));
        match PANEL_OPEN.effective(cx) {
            Effective::Fade(motion) => {
                assert_eq!(motion.duration, Duration::from_millis(190));
                assert!(!motion.moves);
                assert_eq!(motion.easing, STANDARD);
            }
            other => panic!("{other:?}"),
        }
        match CHECK_STROKE.effective(cx) {
            Effective::Move(motion) => {
                assert_eq!(motion.duration, Duration::from_millis(110));
                assert_eq!(motion.delay, Duration::from_millis(60), "the delay stays");
            }
            other => panic!("{other:?}"),
        }
        let animation = CHECK_STROKE.animation(cx);
        assert_eq!(animation.duration, Duration::from_millis(170));
        assert_eq!((animation.easing)(0.2), 0.0, "still delayed");
        assert_eq!((animation.easing)(1.0), 1.0);

        let start = Instant::now();
        let mut t = Transition::new(0.0);
        t.go(1.0, PANEL_OPEN, start, cx);
        let value = t.tick(at(start, 100), window);
        assert!(value > 0.0 && value < 1.0, "{value}");
        assert_eq!(t.tick(at(start, 190), window), 1.0);
        assert!(!t.is_running(at(start, 190)));

        // A spring under reduced motion is a timed fade of half its
        // settle time.
        let mut s = Transition::new(0.0);
        s.go(1.0, ENTER_SESSION, start, cx);
        assert!(s.is_running(at(start, 100)));
        assert_eq!(s.tick(start + ENTER_SESSION.duration / 2, window), 1.0);
        assert!(!s.is_running(start + ENTER_SESSION.duration / 2));
    });
}

#[gpui::test]
fn transition_not_reduced_moves(cx: &mut TestAppContext) {
    in_render(cx, false, |_, cx| {
        assert!(!reduced(cx));
        assert_eq!(PANEL_OPEN.effective(cx), Effective::Move(PANEL_OPEN));
        let animation = CHECK_FILL.animation(cx);
        assert_eq!(animation.duration, Duration::from_millis(180));
        assert_eq!((animation.easing)(0.5), STANDARD.eval(0.5));
    });
}

#[gpui::test]
fn active_transitions_counts_the_ones_in_progress(cx: &mut TestAppContext) {
    in_render(cx, false, |window, cx| {
        let base = active_transitions(cx);
        let start = Instant::now();
        let mut a = Transition::new(0.0);
        let mut b = Transition::new(0.0);
        a.go(1.0, PANEL_OPEN, start, cx);
        b.go(1.0, PANEL_CLOSE, start, cx);
        assert_eq!(active_transitions(cx), base + 2);
        // Retargeting does not count twice.
        a.go(0.5, PANEL_OPEN, at(start, 10), cx);
        assert_eq!(active_transitions(cx), base + 2);

        // The count drops when a tick arrives, not before.
        b.tick(at(start, 100), window);
        assert_eq!(active_transitions(cx), base + 2);
        b.tick(at(start, 280), window);
        assert_eq!(active_transitions(cx), base + 1);
        drop(a);
        assert_eq!(active_transitions(cx), base);

        // `set` interrupting a transition takes one away too.
        let mut c = Transition::new(0.0);
        c.go(1.0, PANEL_OPEN, start, cx);
        assert_eq!(active_transitions(cx), base + 1);
        c.set(1.0);
        assert_eq!(active_transitions(cx), base);
        drop(c);
        assert_eq!(active_transitions(cx), base);
    });
}

/// A frame of the panel example: its offset (a fraction of its width)
/// and its opacity.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Frame {
    offset: f32,
    opacity: f32,
}

/// The panel-offset example of the design: a `Transition` from 1 (closed:
/// off-screen, transparent) to 0 (open). `effective` decides whether the
/// value moves the panel or fades it.
struct Panel {
    offset: Transition,
    clock: Rc<Cell<Instant>>,
    frames: Rc<RefCell<Vec<Frame>>>,
}

impl Render for Panel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let value = self.offset.tick(self.clock.get(), window);
        let frame = match PANEL_OPEN.effective(cx) {
            Effective::Move(_) => Frame {
                offset: value,
                opacity: 1.0,
            },
            Effective::Fade(_) => Frame {
                offset: 0.0,
                opacity: 1.0 - value,
            },
        };
        self.frames.borrow_mut().push(frame);
        div()
    }
}

/// Opens the panel and renders a frame every 16 ms until the transition
/// has stopped; returns the frames after the opening one.
fn open_panel(cx: &mut TestAppContext, reduced_motion: bool) -> Vec<Frame> {
    cx.update(|cx| {
        init_fixed(
            cx,
            Strings::of(Locale::En),
            Appearance::Light,
            reduced_motion,
        );
    });
    let start = Instant::now();
    let clock = Rc::new(Cell::new(start));
    let frames = Rc::new(RefCell::new(Vec::new()));
    let window = cx.add_window({
        let (clock, frames) = (clock.clone(), frames.clone());
        move |_, _| Panel {
            offset: Transition::new(1.0),
            clock,
            frames,
        }
    });
    cx.run_until_parked();
    assert_eq!(frames.borrow().len(), 1, "the closed panel");
    window
        .update(cx, |panel, _, cx| {
            panel.offset.go(0.0, PANEL_OPEN, start, cx);
            cx.notify();
        })
        .unwrap();
    cx.run_until_parked();
    frames.borrow_mut().clear();
    let mut elapsed = 0;
    loop {
        elapsed += 16;
        clock.set(at(start, elapsed));
        let requested = window
            .update(cx, |_, window, cx| window.simulate_next_frame(cx))
            .unwrap();
        cx.run_until_parked();
        if requested == 0 {
            break;
        }
        assert!(elapsed < 1000, "the transition did not stop");
    }
    assert!(
        window
            .update(cx, |_, _, cx| active_transitions(cx))
            .unwrap()
            == 0
    );
    let recorded = frames.borrow();
    recorded.clone()
}

#[gpui::test]
fn reduced_motion_has_no_displacement(cx: &mut TestAppContext) {
    let frames = open_panel(cx, true);
    assert!(frames.len() >= 5, "{frames:?}");
    for frame in &frames {
        assert_eq!(frame.offset, 0.0, "{frame:?}");
    }
    let opacities: Vec<f32> = frames.iter().map(|f| f.opacity).collect();
    assert!(opacities.windows(2).all(|w| w[0] <= w[1]), "{opacities:?}");
    assert!(opacities[0] < 1.0);
    assert_eq!(*opacities.last().unwrap(), 1.0);
    // Half the duration: 190 ms, so the last frame is at or before 208 ms.
    assert!(frames.len() <= 13, "{}", frames.len());

    // Step 3: the `Expander`'s height and the `Segmented` slider arrive at
    // once; only the expander's opacity transitions.
    in_render(cx, true, |window, cx| {
        let start = Instant::now();
        let mut height = Transition::new(0.0);
        let mut opacity = Transition::new(0.0);
        Expander::go(&mut height, &mut opacity, px(120.0), start, cx);
        assert_eq!(height.value(), 120.0);
        assert!(!height.is_running(start));
        assert!(opacity.is_running(start));
        for ms in (0..=120).step_by(8) {
            assert_eq!(height.tick(at(start, ms), window), 120.0, "{ms} ms");
        }
        let mid = opacity.tick(at(start, 50), window);
        assert!(mid > 0.0 && mid < 1.0, "{mid}");
        // EXPAND_FADE halves to 100 ms under reduced motion.
        assert_eq!(opacity.tick(at(start, 100), window), 1.0);
        Expander::go(&mut height, &mut opacity, px(0.0), at(start, 200), cx);
        assert_eq!(height.value(), 0.0);
        assert!(!height.is_running(at(start, 200)));

        let mut slider = Transition::new(0.0);
        Segmented::select(&mut slider, 2, start, cx);
        assert_eq!(slider.value(), 2.0);
        assert!(!slider.is_running(start));
        assert_eq!(slider.tick(at(start, 8), window), 2.0);
    });
}

#[gpui::test]
fn panel_moves_when_motion_is_not_reduced(cx: &mut TestAppContext) {
    let frames = open_panel(cx, false);
    assert!(frames.len() >= 20, "{frames:?}");
    assert!(frames.iter().all(|f| f.opacity == 1.0));
    let offsets: Vec<f32> = frames.iter().map(|f| f.offset).collect();
    assert!(offsets[0] > 0.0 && offsets[0] < 1.0, "{offsets:?}");
    assert!(offsets.windows(2).all(|w| w[0] >= w[1]), "{offsets:?}");
    assert_eq!(*offsets.last().unwrap(), 0.0);
}
