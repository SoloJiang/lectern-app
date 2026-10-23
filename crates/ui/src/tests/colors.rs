//! `palette_builtin_*` and `code_colors_*` (docs/design/ui.md「测试」): the
//! built-in table's shape, the diff backgrounds, and the colour-vision
//! checks of the five code colours.

use gpui::{Hsla, Rgba};
use lectern_syntax::TokenKind;

use crate::palette::over;
use crate::{Appearance, Palette, SystemColor, code_style, code_styles};

fn rgb(color: Hsla) -> [f32; 3] {
    let c = Rgba::from(color);
    [c.r, c.g, c.b]
}

/// `fg` (with its alpha) over the opaque `bg`, as sRGB components.
fn composite(bg: Hsla, fg: Hsla) -> [f32; 3] {
    let (b, f) = (Rgba::from(bg), Rgba::from(fg));
    [
        b.r * (1.0 - f.a) + f.r * f.a,
        b.g * (1.0 - f.a) + f.g * f.a,
        b.b * (1.0 - f.a) + f.b * f.a,
    ]
}

fn linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn encode(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// WCAG 2 relative luminance.
fn luminance(rgb: [f32; 3]) -> f32 {
    0.2126 * linear(rgb[0]) + 0.7152 * linear(rgb[1]) + 0.0722 * linear(rgb[2])
}

/// WCAG 2 contrast ratio.
fn contrast(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// sRGB → CIE L*a*b* (D65).
fn lab(rgb: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = rgb.map(linear);
    let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
    let f = |t: f32| {
        if t > 0.008_856 {
            t.cbrt()
        } else {
            7.787 * t + 16.0 / 116.0
        }
    };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// CIE76.
fn delta_e(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (a, b) = (lab(a), lab(b));
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

#[derive(Clone, Copy, Debug)]
enum Vision {
    Normal,
    Protanopia,
    Deuteranopia,
}

/// Viénot, Brettel and Mollon (1999): dichromat simulation as a 3×3 matrix
/// on linear RGB.
fn simulate(vision: Vision, rgb: [f32; 3]) -> [f32; 3] {
    let m: [[f32; 3]; 3] = match vision {
        Vision::Normal => return rgb,
        Vision::Protanopia => [
            [0.11238, 0.88762, 0.0],
            [0.11238, 0.88762, 0.0],
            [0.00401, -0.00401, 1.0],
        ],
        Vision::Deuteranopia => [
            [0.29275, 0.70725, 0.0],
            [0.29275, 0.70725, 0.0],
            [-0.02234, 0.02234, 1.0],
        ],
    };
    let l = rgb.map(linear);
    m.map(|row| encode(row[0] * l[0] + row[1] * l[1] + row[2] * l[2]))
}

/// The five colours of the code table, opaque (`secondary_label` composited
/// over `control_background`).
fn code_colors(palette: &Palette) -> [(&'static str, [f32; 3]); 5] {
    [
        ("purple", rgb(palette.system(SystemColor::Purple))),
        ("red", rgb(palette.system(SystemColor::Red))),
        ("blue", rgb(palette.system(SystemColor::Blue))),
        ("pink", rgb(palette.system(SystemColor::Pink))),
        (
            "secondary_label",
            composite(palette.control_background, palette.secondary_label),
        ),
    ]
}

fn all_colors(palette: &Palette) -> Vec<(&'static str, Hsla)> {
    let mut colors = vec![
        ("label", palette.label),
        ("secondary_label", palette.secondary_label),
        ("tertiary_label", palette.tertiary_label),
        ("separator", palette.separator),
        ("window_background", palette.window_background),
        ("control_background", palette.control_background),
        ("under_page_background", palette.under_page_background),
        ("accent", palette.accent),
        (
            "selected_content_background",
            palette.selected_content_background,
        ),
        (
            "unemphasized_selected_content_background",
            palette.unemphasized_selected_content_background,
        ),
        ("diff_added_background", palette.diff_added_background),
        ("diff_removed_background", palette.diff_removed_background),
        ("agent_claude", palette.agent_claude),
        ("agent_codex", palette.agent_codex),
    ];
    for color in SystemColor::ALL {
        colors.push(("system", palette.system(color)));
    }
    colors
}

#[test]
fn palette_builtin_has_27_colors_with_alpha_in_range() {
    for appearance in Appearance::ALL {
        let palette = Palette::builtin(appearance);
        assert_eq!(palette.appearance, appearance);
        let colors = all_colors(&palette);
        assert_eq!(colors.len(), 27);
        for (name, color) in colors {
            assert!(
                (0.0..=1.0).contains(&color.a) && (0.0..=1.0).contains(&color.l),
                "{appearance:?} {name}: {color:?}"
            );
        }
        // Surfaces and system colours are opaque; the labels are translucent.
        assert_eq!(palette.window_background.a, 1.0);
        assert_eq!(palette.system(SystemColor::Red).a, 1.0);
        assert!(palette.label.a < 1.0 && palette.label.a > 0.8);
    }
}

#[test]
fn palette_builtin_diff_backgrounds_match_the_hand_composite() {
    for (appearance, opacity) in [(Appearance::Light, 0.12), (Appearance::Dark, 0.18)] {
        let palette = Palette::builtin(appearance);
        for (name, actual, source) in [
            (
                "added",
                palette.diff_added_background,
                palette.system(SystemColor::Green),
            ),
            (
                "removed",
                palette.diff_removed_background,
                palette.system(SystemColor::Red),
            ),
        ] {
            let expected = composite(palette.control_background, source.alpha(opacity));
            let actual = rgb(actual);
            for i in 0..3 {
                assert!(
                    (actual[i] - expected[i]).abs() < 1e-3,
                    "{appearance:?} {name}: {actual:?} vs {expected:?}"
                );
            }
            assert_eq!(
                actual,
                rgb(over(palette.control_background, source, opacity))
            );
        }
    }
}

#[test]
fn palette_builtin_label_contrast_is_at_least_7() {
    for appearance in Appearance::ALL {
        let palette = Palette::builtin(appearance);
        let label = composite(palette.window_background, palette.label);
        let ratio = contrast(label, rgb(palette.window_background));
        assert!(ratio >= 7.0, "{appearance:?}: {ratio}");
    }
}

#[test]
fn palette_builtin_differs_between_appearances() {
    let (light, dark) = (
        Palette::builtin(Appearance::Light),
        Palette::builtin(Appearance::Dark),
    );
    assert_ne!(light, dark);
    assert!(light.window_background.l > dark.window_background.l);
    assert!(light.label.l < dark.label.l);
}

#[test]
fn code_colors_are_distinguishable_under_three_visions() {
    for appearance in Appearance::ALL {
        let palette = Palette::builtin(appearance);
        let colors = code_colors(&palette);
        for vision in [Vision::Normal, Vision::Protanopia, Vision::Deuteranopia] {
            for (i, (a_name, a)) in colors.iter().enumerate() {
                for (b_name, b) in &colors[i + 1..] {
                    let distance = delta_e(simulate(vision, *a), simulate(vision, *b));
                    assert!(
                        distance >= 12.0,
                        "{appearance:?} {vision:?}: {a_name} vs {b_name} ΔE {distance}"
                    );
                }
            }
        }
    }
}

#[test]
fn code_colors_contrast_with_the_control_background() {
    for appearance in Appearance::ALL {
        let palette = Palette::builtin(appearance);
        let background = rgb(palette.control_background);
        for (name, color) in code_colors(&palette) {
            let ratio = contrast(color, background);
            assert!(ratio >= 3.0, "{appearance:?} {name}: {ratio}");
        }
    }
}

#[test]
fn code_colors_use_only_palette_colors() {
    let palette = Palette::builtin(Appearance::Light);
    let allowed = [
        palette.label,
        palette.secondary_label,
        palette.system(SystemColor::Purple),
        palette.system(SystemColor::Red),
        palette.system(SystemColor::Blue),
        palette.system(SystemColor::Pink),
    ];
    for (kind, style) in code_styles(&palette) {
        assert!(allowed.contains(&style.color), "{kind:?}: {style:?}");
        assert_eq!(style, code_style(kind, &palette));
    }
    assert!(code_style(TokenKind::Link, &palette).underline);
    assert!(code_style(TokenKind::Emphasis, &palette).italic);
    assert_eq!(
        code_style(TokenKind::Heading, &palette).weight,
        gpui::FontWeight::SEMIBOLD
    );
}

#[test]
fn code_styles_light_snapshot() {
    insta::assert_debug_snapshot!(code_styles(&Palette::builtin(Appearance::Light)));
}

#[test]
fn code_styles_dark_snapshot() {
    insta::assert_debug_snapshot!(code_styles(&Palette::builtin(Appearance::Dark)));
}
