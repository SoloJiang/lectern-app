//! Test support (`test-support` feature): the hooks the design's
//! `#[gpui::test]`s need. GPUI's test platform cannot change a window's
//! appearance from outside GPUI (`TestAppContext::test_window` is
//! crate-private in gpui-pre 0.3.7), so the handlers behind `attach`'s
//! observer and `init`'s watch callback are called directly here.

use std::time::Instant;

use gpui::App;
use lectern_platform::appearance::Change;

use crate::motion::Transition;
use crate::palette::Appearance;
use crate::state::UiState;

/// What the observer `attach` registered does when the window's appearance
/// becomes `appearance`.
pub fn simulate_appearance_change(appearance: Appearance, cx: &mut App) {
    crate::init::appearance_changed(appearance, cx);
}

/// What `init`'s watch callback does when `platform` reports `change`.
pub fn simulate_platform_change(change: Change, cx: &mut App) {
    crate::init::platform_changed(change, cx);
}

/// How many times `mono_metrics` resolved a font in this app.
pub fn mono_resolutions(cx: &App) -> u32 {
    cx.global::<UiState>().mono_resolutions
}

/// `Transition::tick` without GPUI's frame request, for the
/// `ui/transition_tick` benchmark: the transition's own work alone.
pub fn step_transition(transition: &mut Transition, now: Instant) -> f32 {
    transition.step(now);
    transition.value()
}
