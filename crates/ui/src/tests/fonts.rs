//! `mono_metrics_is_cached`, the font resolution's three outcomes and the
//! text roles (docs/design/ui.md「字体与度量」).

use gpui::{FontId, FontWeight, TestAppContext, VisualTestContext, px};
use lectern_i18n::{Locale, Strings};

use crate::font::resolve_mono_for_test;
use crate::testing::mono_resolutions;
use crate::{
    Appearance, MONO_FALLBACK_FAMILY, MONO_FAMILY, TextRole, UI_FAMILY, init_fixed, mono_metrics,
};

#[gpui::test]
fn mono_metrics_is_cached(cx: &mut TestAppContext) {
    cx.update(|cx| init_fixed(cx, Strings::of(Locale::En), Appearance::Light, false));
    let window = cx.add_window(|_, _| gpui::Empty);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let first = cx.update(|window, cx| mono_metrics(window, cx));
    let second = cx.update(|window, cx| mono_metrics(window, cx));
    assert_eq!(first, second);
    cx.update(|_, cx| assert_eq!(mono_resolutions(cx), 1));

    // GPUI's test text system: units_per_em 1000, ascent 1025, descent -275,
    // the advance of `m` 600 at 12px. The descent is kept positive and the
    // line height is rounded up to 0.5.
    assert_eq!(first.family, MONO_FAMILY);
    assert!((first.cell_width.as_f32() - 7.2).abs() < 1e-3, "{first:?}");
    assert!((first.ascent.as_f32() - 12.3).abs() < 1e-3, "{first:?}");
    assert!((first.descent.as_f32() - 3.3).abs() < 1e-3, "{first:?}");
    assert_eq!(first.line_height, px(16.0));
}

fn resolver(ids: &'static [(&'static str, u32)]) -> impl Fn(&'static str) -> FontId {
    move |family| {
        ids.iter()
            .find(|(name, _)| *name == family)
            .map(|(_, id)| FontId(*id as usize))
            // Unknown families get the fallback stack's id (probe's).
            .unwrap_or(FontId(1))
    }
}

#[test]
fn mono_resolves_when_its_id_differs_from_the_fallback() {
    let (id, family) = resolve_mono_for_test(resolver(&[
        (UI_FAMILY, 2),
        (MONO_FAMILY, 3),
        (MONO_FALLBACK_FAMILY, 4),
    ]));
    assert_eq!((id, family), (FontId(3), MONO_FAMILY));
}

#[test]
fn mono_falls_back_to_menlo_then_to_the_ui_font() {
    let (id, family) =
        resolve_mono_for_test(resolver(&[(UI_FAMILY, 2), (MONO_FALLBACK_FAMILY, 4)]));
    assert_eq!((id, family), (FontId(4), MONO_FALLBACK_FAMILY));
    let (id, family) = resolve_mono_for_test(resolver(&[(UI_FAMILY, 2)]));
    assert_eq!((id, family), (FontId(2), UI_FAMILY));
}

#[test]
fn mono_is_taken_as_resolved_when_the_text_system_cannot_tell_families_apart() {
    let (id, family) = resolve_mono_for_test(|_| FontId(1));
    assert_eq!((id, family), (FontId(1), MONO_FAMILY));
}

#[test]
fn text_roles_have_the_spec_sizes() {
    assert_eq!(TextRole::LargeTitle.size(), px(26.0));
    assert_eq!(TextRole::Title1.size(), px(22.0));
    assert_eq!(TextRole::Title2.size(), px(17.0));
    assert_eq!(TextRole::Title3.size(), px(15.0));
    assert_eq!(TextRole::Headline.size(), px(13.0));
    assert_eq!(TextRole::Body.size(), px(13.0));
    assert_eq!(TextRole::Secondary.size(), px(13.0));
    assert_eq!(TextRole::Callout.size(), px(12.0));
    assert_eq!(TextRole::Caption.size(), px(11.0));
    assert_eq!(TextRole::Footnote.size(), px(10.0));
    assert_eq!(TextRole::Code.size(), px(12.0));
    for role in [
        TextRole::LargeTitle,
        TextRole::Title1,
        TextRole::Title2,
        TextRole::Title3,
        TextRole::Headline,
        TextRole::Body,
        TextRole::Secondary,
        TextRole::Callout,
        TextRole::Caption,
        TextRole::Footnote,
        TextRole::Code,
    ] {
        assert!(role.line_height() > role.size(), "{role:?}");
        let expected = if role == TextRole::Code {
            MONO_FAMILY
        } else {
            UI_FAMILY
        };
        assert_eq!(role.font().family.as_ref(), expected, "{role:?}");
    }
    assert_eq!(TextRole::LargeTitle.font().weight, FontWeight::BOLD);
    for role in [
        TextRole::Title1,
        TextRole::Title2,
        TextRole::Title3,
        TextRole::Headline,
    ] {
        assert_eq!(role.font().weight, FontWeight::SEMIBOLD, "{role:?}");
    }
    assert_eq!(TextRole::Body.font().weight, FontWeight::NORMAL);
}
