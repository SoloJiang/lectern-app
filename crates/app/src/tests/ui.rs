//! The `ui` hook-up on the skeleton window (docs/design/ui.md「验收」第 1
//! 步的 `app` 项). GPUI's test platform cannot change a window's appearance,
//! so the change arrives through `lectern_ui::testing::simulate_appearance_change`,
//! which runs what `attach`'s observer runs.

use gpui::{TestAppContext, WindowHandle};
use gpui_component::Theme;
use lectern_i18n::{Locale, Strings};
use lectern_logging::EarlyConfig;
use lectern_ui::testing::simulate_appearance_change;
use lectern_ui::{Appearance, colors, is_ready, palette};

use crate::profile::Profile;
use crate::startup::{Started, Startup};
use crate::window::{self, Skeleton};

/// The one skeleton window.
fn skeleton(cx: &mut TestAppContext) -> WindowHandle<Skeleton> {
    let windows = cx.update(|cx| cx.windows());
    assert_eq!(windows.len(), 1, "{windows:?}");
    windows[0]
        .downcast::<Skeleton>()
        .expect("the root view is the skeleton")
}

/// How many frames the skeleton has rendered once the pending work ran.
fn renders(window: &WindowHandle<Skeleton>, cx: &mut TestAppContext) -> usize {
    cx.run_until_parked();
    cx.update(|cx| window.read(cx).map(|skeleton| skeleton.renders).unwrap())
}

/// The desktop order with the test profile: `init_fixed` (the test
/// profile's `init` plus `ready`), the skeleton window with `attach`,
/// `release`. A simulated appearance change refreshes the window once and
/// `colors` serves the other palette.
#[gpui::test]
fn appearance_change_refreshes_the_skeleton_window(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let early = lectern_logging::install_early(EarlyConfig {
        dir: Some(dir.path().to_owned()),
        cli_filter: None,
    })
    .expect("one install per test process");
    cx.update(|cx| {
        cx.set_global(Startup::new(early, Profile::Test));
        lectern_ui::init_fixed(cx, Strings::of(Locale::En), Appearance::Light, false);
        window::open(cx);
        assert!(cx.has_global::<Started>(), "the test profile released");
        assert!(is_ready(cx));
    });
    let window = skeleton(cx);
    let before = renders(&window, cx);
    assert!(before >= 1, "{before}");
    let dark = cx.update(|cx| *palette(Appearance::Dark, cx));

    cx.update(|cx| simulate_appearance_change(Appearance::Dark, cx));

    assert_eq!(renders(&window, cx), before + 1, "refreshed once");
    cx.update(|cx| {
        window
            .update(cx, |_, window, cx| assert_eq!(*colors(window, cx), dark))
            .unwrap();
    });
}

/// `init`, the skeleton window with `attach`, then an appearance change
/// before `ready` (no `Startup` global, so `open` releases nothing): no
/// panic, the window is refreshed, and gpui-component's theme global is
/// never created, so nothing read or wrote it. The served `Palette` cannot
/// be checked here: without `init_fixed` it follows the window's own
/// appearance, which the test platform cannot change
/// (docs/design/ui.md「与设计的差异」).
#[gpui::test]
fn appearance_change_before_ready_does_not_touch_the_theme(cx: &mut TestAppContext) {
    cx.update(|cx| {
        lectern_ui::init(cx, Strings::of(Locale::En));
        window::open(cx);
        assert!(!cx.has_global::<Started>());
        assert!(!is_ready(cx));
    });
    let window = skeleton(cx);
    let before = renders(&window, cx);

    cx.update(|cx| simulate_appearance_change(Appearance::Dark, cx));

    assert_eq!(renders(&window, cx), before + 1, "refreshed once");
    cx.update(|cx| {
        assert!(!is_ready(cx));
        assert!(
            !cx.has_global::<Theme>(),
            "the theme exists only after `ready`"
        );
        // Both built-in palettes are still served, one per appearance.
        assert_ne!(
            palette(Appearance::Light, cx),
            palette(Appearance::Dark, cx)
        );
    });
}
