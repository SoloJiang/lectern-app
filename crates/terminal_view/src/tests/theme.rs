//! `theme_*` (docs/design/terminal_view.md「颜色」): `theme_of` for both
//! appearances, and one `SetTheme` when the appearance changes.

use gpui::TestAppContext;
use lectern_services::terminal::{ColorScheme, Input};
use lectern_ui::Appearance;
use lectern_ui::testing::simulate_appearance_change;

use crate::testing::frame;
use crate::tests::support::Rig;
use crate::{theme, theme_of};

fn palette_theme(
    appearance: Appearance,
    cx: &mut TestAppContext,
) -> lectern_services::terminal::Theme {
    cx.update(|cx| theme_of(lectern_ui::palette(appearance, cx)))
}

#[gpui::test]
fn theme_light_snapshot(cx: &mut TestAppContext) {
    Rig::new(cx);
    insta::assert_debug_snapshot!(palette_theme(Appearance::Light, cx));
}

#[gpui::test]
fn theme_dark_snapshot(cx: &mut TestAppContext) {
    Rig::with_appearance(cx, Appearance::Dark);
    insta::assert_debug_snapshot!(palette_theme(Appearance::Dark, cx));
}

#[gpui::test]
fn theme_scheme_follows_the_appearance(cx: &mut TestAppContext) {
    Rig::new(cx);
    assert_eq!(
        palette_theme(Appearance::Light, cx).scheme,
        ColorScheme::Light
    );
    assert_eq!(
        palette_theme(Appearance::Dark, cx).scheme,
        ColorScheme::Dark
    );
}

/// The background is the terminal's own colour; the foreground is `label`
/// laid over it, opaque, and the two differ in both appearances.
#[gpui::test]
fn theme_colours_are_opaque_and_distinct(cx: &mut TestAppContext) {
    Rig::new(cx);
    for appearance in [Appearance::Light, Appearance::Dark] {
        let theme = palette_theme(appearance, cx);
        assert_ne!(theme.foreground, theme.background);
        assert_eq!(theme.cursor, theme.foreground);
        // ANSI black and white are not the same colour as the other's ground.
        assert_ne!(theme.ansi[0], theme.ansi[7]);
    }
}

#[gpui::test]
fn theme_of_the_window_is_the_theme_of_its_appearance(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, None);
    let mut visual = opened.visual(cx);
    let from_window = visual.update(|window, cx| theme(window, cx));
    assert_eq!(from_window, palette_theme(Appearance::Light, cx));
}

/// The pane starts with the theme the host opened the terminal with (the
/// current one), so the first render sends no `SetTheme`.
#[gpui::test]
fn theme_first_render_sends_nothing(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    rig.open(cx, Some(frame(&["$"]).build()));
    let themes = rig
        .inputs()
        .into_iter()
        .filter(|input| matches!(input, Input::SetTheme(_)))
        .count();
    assert_eq!(themes, 0);
}

#[gpui::test]
fn theme_change_of_appearance_sends_one_set_theme(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(frame(&["$"]).build()));
    rig.inputs();
    cx.update(|cx| simulate_appearance_change(Appearance::Dark, cx));
    opened.draw(cx);
    opened.draw(cx);
    assert_eq!(
        rig.inputs(),
        vec![Input::SetTheme(palette_theme(Appearance::Dark, cx))]
    );
    // And back.
    cx.update(|cx| simulate_appearance_change(Appearance::Light, cx));
    opened.draw(cx);
    assert_eq!(
        rig.inputs(),
        vec![Input::SetTheme(palette_theme(Appearance::Light, cx))]
    );
}
