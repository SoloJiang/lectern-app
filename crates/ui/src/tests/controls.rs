//! Step 3 (docs/design/ui.md「测试」): `loading_phase_*`, `hint_*`,
//! `icon_assets_hold_every_icon`, `circle_check_toggles_from_key_and_click`,
//! `segmented_slider_continues_from_current_position`, a render of every
//! control after `ready` and the panic before it.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AssetSource, Context, ElementId, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyBinding, Modifiers, ParentElement, Render, Styled, TestAppContext, VisualTestContext,
    WeakEntity, Window, actions, div, point, px,
};
use lectern_i18n::{Locale, Strings, keys};

use crate::motion::{HINT_DURATION, HOVER_OUT, Transition};
use crate::semantic::NodeId;
use crate::tests::motion::{at, in_render};
use crate::{
    Agent, AgentDot, Appearance, Button, ButtonKind, ButtonSize, CircleCheck, Expander,
    GroupedList, Hint, Icon, IconAssets, LoadingPhase, Progress, ProgressShape, Row, Segmented,
    Skeleton, Text, init, init_fixed, loading_phase, strings,
};

fn fixed(cx: &mut TestAppContext, reduced_motion: bool) {
    cx.update(|cx| {
        init_fixed(
            cx,
            Strings::of(Locale::En),
            Appearance::Light,
            reduced_motion,
        );
    });
}

fn verbatim(text: &str) -> Text {
    Text::verbatim(Arc::<str>::from(text))
}

#[test]
fn loading_phase_follows_the_thresholds() {
    assert_eq!(
        loading_phase(Duration::from_millis(99)),
        LoadingPhase::Nothing
    );
    assert_eq!(
        loading_phase(Duration::from_millis(100)),
        LoadingPhase::Skeleton
    );
    assert_eq!(
        loading_phase(Duration::from_millis(999)),
        LoadingPhase::Skeleton
    );
    assert_eq!(
        loading_phase(Duration::from_secs(1)),
        LoadingPhase::Progress
    );
    assert_eq!(loading_phase(Duration::ZERO), LoadingPhase::Nothing);
}

#[test]
fn hint_expires_after_its_duration_and_the_fade() {
    let id = NodeId::new("workspace", "hint", None);
    let shown = Instant::now();
    let hint = Hint::new(id.clone(), verbatim("saved"), shown);
    assert_eq!(HINT_DURATION, Duration::from_millis(2500));
    assert!(!hint.is_expired(shown));
    assert!(!hint.is_expired(shown + HINT_DURATION));
    assert!(!hint.is_expired(shown + HINT_DURATION + HOVER_OUT.duration / 2));
    assert!(hint.is_expired(shown + HINT_DURATION + HOVER_OUT.duration));

    let short = Hint::new(id, verbatim("saved"), shown).duration(Duration::from_millis(500));
    assert!(short.is_expired(shown + Duration::from_millis(500) + HOVER_OUT.duration));
    assert!(!short.is_expired(shown + Duration::from_millis(499)));
}

#[test]
fn hint_shown_again_counts_from_the_new_instant() {
    let id = NodeId::new("workspace", "hint", None);
    let first = Instant::now();
    let later = first + HINT_DURATION;
    let hint = Hint::new(id.clone(), verbatim("one"), first);
    let again = Hint::new(id, verbatim("two"), later);
    let probe = later + HINT_DURATION / 2;
    assert!(hint.is_expired(probe));
    assert!(!again.is_expired(probe));
    assert_eq!(again.text().as_str(), "two");
}

#[test]
fn hint_frames_arrive_hold_and_fade() {
    let shown = Instant::now();
    let hint = Hint::new(NodeId::new("workspace", "hint", None), verbatim("x"), shown);
    let start = hint.frame_when(shown, false).expect("shown");
    assert_eq!(start.offset, -crate::motion::ARRIVE_OFFSET);
    assert_eq!(start.opacity, 0.0);
    let mid = hint
        .frame_when(shown + Duration::from_millis(120), false)
        .expect("arriving");
    assert!(mid.offset > start.offset && mid.offset < px(0.0), "{mid:?}");
    assert!(mid.opacity > 0.0 && mid.opacity < 1.0, "{mid:?}");
    let held = hint
        .frame_when(shown + Duration::from_secs(1), false)
        .expect("held");
    assert_eq!((held.offset, held.opacity), (px(0.0), 1.0));
    let fading = hint
        .frame_when(shown + HINT_DURATION + HOVER_OUT.duration / 2, false)
        .expect("fading");
    assert_eq!(fading.offset, px(0.0));
    assert!(fading.opacity > 0.0 && fading.opacity < 1.0, "{fading:?}");
    assert!(
        hint.frame_when(shown + HINT_DURATION + HOVER_OUT.duration, false)
            .is_none()
    );

    // Reduced motion: no offset at any point, the arrival takes half as long.
    let reduced = hint.frame_when(shown, true).expect("shown");
    assert_eq!(reduced.offset, px(0.0));
    let arrived = hint
        .frame_when(shown + crate::motion::ARRIVE.duration / 2, true)
        .expect("held");
    assert_eq!((arrived.offset, arrived.opacity), (px(0.0), 1.0));
}

#[test]
fn icon_assets_hold_every_icon() {
    for icon in Icon::ALL {
        let path = icon.path();
        let svg = IconAssets
            .load(&path)
            .expect("the source answers")
            .unwrap_or_else(|| panic!("{icon:?} ({path}) has no SVG"));
        assert!(!svg.is_empty(), "{icon:?}");
        assert!(
            svg.starts_with(b"<svg"),
            "{icon:?} is not an SVG: {:?}",
            &svg[..svg.len().min(16)]
        );
    }
    assert_eq!(
        IconAssets.list("icons/").expect("lists").len(),
        Icon::ALL.len()
    );
    assert!(
        IconAssets
            .load("icons/activity.svg")
            .expect("answers")
            .is_none(),
        "only the ten icons are embedded"
    );
}

actions!(lectern_ui_tests, [ToggleSeen]);

/// A view with one `CircleCheck` at a known place; V toggles it the way
/// the review view's action will.
struct CheckProbe {
    focus: FocusHandle,
    id: NodeId,
    checked: bool,
    changes: Rc<RefCell<Vec<bool>>>,
}

impl CheckProbe {
    fn toggle(&mut self, checked: bool, cx: &mut Context<Self>) {
        self.checked = checked;
        self.changes.borrow_mut().push(checked);
        cx.notify();
    }
}

impl Focusable for CheckProbe {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

const CHECK_ORIGIN: f32 = 100.0;

impl Render for CheckProbe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this: WeakEntity<CheckProbe> = cx.entity().downgrade();
        div()
            .size_full()
            .track_focus(&self.focus)
            .key_context("CheckProbe")
            .on_action(cx.listener(|this, _: &ToggleSeen, _, cx| {
                let checked = !this.checked;
                this.toggle(checked, cx);
            }))
            .child(
                div()
                    .absolute()
                    .top(px(CHECK_ORIGIN))
                    .left(px(CHECK_ORIGIN))
                    .child(CircleCheck::new(self.id.clone(), self.checked).on_change(
                        move |checked, _, cx| {
                            this.update(cx, |view, cx| view.toggle(checked, cx))
                                .expect("the view is alive");
                        },
                    )),
            )
    }
}

#[gpui::test]
fn circle_check_toggles_from_key_and_click(cx: &mut TestAppContext) {
    fixed(cx, false);
    cx.update(|cx| cx.bind_keys([KeyBinding::new("v", ToggleSeen, Some("CheckProbe"))]));
    let changes = Rc::new(RefCell::new(Vec::new()));
    let id = NodeId::new("review", "files", Some("src/main.rs")).child("seen");
    let (view, cx) = cx.add_window_view({
        let changes = changes.clone();
        let id = id.clone();
        move |_, cx| CheckProbe {
            focus: cx.focus_handle(),
            id,
            checked: false,
            changes,
        }
    });
    cx.update(|window, cx| {
        let focus = view.read(cx).focus.clone();
        window.focus(&focus, cx);
    });
    cx.run_until_parked();

    cx.simulate_keystrokes("v");
    cx.run_until_parked();
    assert_eq!(*changes.borrow(), [true]);
    assert!(view.read_with(cx, |view, _| view.checked));

    let centre = point(px(CHECK_ORIGIN + 8.0), px(CHECK_ORIGIN + 8.0));
    cx.simulate_click(centre, Modifiers::default());
    cx.run_until_parked();
    assert_eq!(*changes.borrow(), [true, false]);

    cx.simulate_click(centre, Modifiers::default());
    cx.run_until_parked();
    assert_eq!(*changes.borrow(), [true, false, true]);

    // Both paths change `checked`; the fill's animation element is a
    // function of the node alone, so both start the same one.
    let from_key = CircleCheck::fill_animation_id(&id);
    let from_click = CircleCheck::fill_animation_id(&id.clone());
    assert_eq!(from_key, from_click);
    assert_ne!(from_key, CircleCheck::stroke_animation_id(&id));
    assert_ne!(
        from_key,
        CircleCheck::fill_animation_id(&NodeId::new("review", "files", Some("b.rs")))
    );
}

/// A control mounted already checked draws the end state; only a change
/// from unchecked to checked animates, and a later check animates again.
#[test]
fn circle_check_animates_only_a_change_to_checked() {
    let mut mounted_checked = crate::controls::Mark::new(true);
    assert!(!mounted_checked.update(true), "mount: end state at once");
    assert!(!mounted_checked.update(true), "still no animation");
    assert!(!mounted_checked.update(false));
    assert!(mounted_checked.update(true), "a change animates");
    assert!(
        mounted_checked.update(true),
        "and keeps its element while it plays"
    );

    let mut mounted_unchecked = crate::controls::Mark::new(false);
    assert!(!mounted_unchecked.update(false));
    assert!(mounted_unchecked.update(true));
    assert!(!mounted_unchecked.update(false));
}

/// The reduced-motion fade of the segmented slider follows the same rule.
#[test]
fn segmented_fades_only_a_changed_selection() {
    let mut selection = crate::controls::Selection::new(1);
    assert!(!selection.update(1), "mount: no fade");
    assert!(selection.update(2), "a change fades");
    assert!(selection.update(2), "and keeps its element while it plays");
    assert!(selection.update(0));
}

#[gpui::test]
fn segmented_slider_continues_from_current_position(cx: &mut TestAppContext) {
    in_render(cx, false, |window, cx| {
        let start = Instant::now();
        let mut slider = Transition::new(0.0);
        Segmented::select(&mut slider, 1, start, cx);
        let half = slider.tick(at(start, 130), window);
        assert!(half > 0.0 && half < 1.0, "{half}");

        // Switched back halfway: the slider returns from where it is.
        Segmented::select(&mut slider, 0, at(start, 130), cx);
        assert_eq!(slider.value(), half);
        let mut previous = half;
        for ms in (138..=400).step_by(8) {
            let value = slider.tick(at(start, ms), window);
            assert!(value <= previous, "{value} after {previous} at {ms} ms");
            assert!(previous - value < 0.15, "jump at {ms} ms");
            previous = value;
        }
        assert_eq!(slider.value(), 0.0);
        assert!(!slider.is_running(at(start, 400)));
    });
}

/// A view that renders every control once.
struct Gallery {
    slider: Transition,
    height: Transition,
    opacity: Transition,
    hint: Hint,
}

impl Render for Gallery {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        let strings = strings(cx);
        let root = NodeId::new("gallery", "root", None);
        let items = vec![
            (root.child("files"), Text::key(&strings, keys::common::OK)),
            (root.child("conversation"), verbatim("Conversation")),
            (root.child("usage"), verbatim("Usage")),
        ];
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                Button::new(root.child("primary"), Text::key(&strings, keys::common::OK))
                    .kind(ButtonKind::Primary)
                    .icon(Icon::Check)
                    .on_click(|_, _| {}),
            )
            .child(
                Button::new(root.child("secondary"), verbatim("Clone"))
                    .kind(ButtonKind::Secondary)
                    .size(ButtonSize::Small)
                    .icon(Icon::Clone),
            )
            .child(
                Button::new(root.child("plain"), verbatim("Back"))
                    .kind(ButtonKind::Plain)
                    .disabled(true),
            )
            .child(
                AgentDot::new(Agent::ClaudeCode)
                    .blink(ElementId::Name("round-1".into()))
                    .node(root.child("agent")),
            )
            .child(AgentDot::new(Agent::Codex))
            .child(CircleCheck::new(root.child("seen"), true).on_change(|_, _, _| {}))
            .child(CircleCheck::new(root.child("unseen"), false))
            .child(Progress::new(root.child("bar"), ProgressShape::Bar, 0.4))
            .child(Progress::new(root.child("ring"), ProgressShape::Ring, 0.75))
            .child(Progress::new(
                root.child("ring-full"),
                ProgressShape::Ring,
                1.0,
            ))
            .child(
                Segmented::new(root.child("segments"), items, 1)
                    .slider(self.slider.tick(now, window))
                    .on_select(|_, _, _| {}),
            )
            .child(
                GroupedList::new(root.child("sessions"))
                    .header(verbatim("Today"))
                    .row(
                        Row::new(root.child("sessions").item("a"))
                            .selected(true)
                            .child(AgentDot::new(Agent::Codex))
                            .child(verbatim("first").shared()),
                    )
                    .row(
                        Row::new(root.child("sessions").item("b"))
                            .on_click(|_, _| {})
                            .child(verbatim("second").shared()),
                    ),
            )
            .child(Skeleton::new(px(120.0), px(12.0)))
            .child(Expander::new(
                root.child("comment"),
                px(self.height.tick(now, window)),
                self.opacity.tick(now, window),
                div().child(verbatim("comment").shared()),
            ))
            .child(self.hint.clone())
    }
}

#[gpui::test]
fn every_control_renders_after_ready(cx: &mut TestAppContext) {
    fixed(cx, false);
    let window = cx.add_window(|_, cx| {
        let now = Instant::now();
        let mut slider = Transition::new(0.0);
        Segmented::select(&mut slider, 1, now, cx);
        let mut height = Transition::new(0.0);
        let mut opacity = Transition::new(0.0);
        Expander::go(&mut height, &mut opacity, px(80.0), now, cx);
        Gallery {
            slider,
            height,
            opacity,
            hint: Hint::new(NodeId::new("gallery", "hint", None), verbatim("saved"), now),
        }
    });
    cx.run_until_parked();
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    // A few frames: the animations advance without a panic.
    for _ in 0..3 {
        cx.update(|window, cx| {
            window.simulate_next_frame(cx);
        });
        cx.run_until_parked();
    }
    // Hovering a row starts its transition; the next render reads it.
    cx.simulate_mouse_move(point(px(20.0), px(20.0)), None, Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.simulate_next_frame(cx);
    });
    cx.run_until_parked();
}

/// A root view with one control: before `ready` it panics with the
/// assembly message.
struct Early;

impl Render for Early {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Button::new(NodeId::new("early", "button", None), verbatim("too soon"))
    }
}

#[gpui::test]
#[should_panic(expected = "before lectern_ui::ready")]
fn control_before_ready_panics(cx: &mut TestAppContext) {
    cx.update(|cx| init(cx, Strings::of(Locale::En)));
    let _window = cx.add_window(|_, _| Early);
    cx.run_until_parked();
}
