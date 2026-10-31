//! Terminal colours (docs/design/terminal_view.md「对其他 crate 的要求」第 1 项):
//! the default colours and the 16 ANSI colours of one appearance, all from
//! the `Palette`. `terminal_view` converts them to the `Theme` it gives
//! `terminal` (ADR 0015: the colour mapping is in this crate, not in a view).

use gpui::{Hsla, Rgba};

use crate::palette::{Appearance, Palette, SystemColor, over};

/// The colours of a terminal. `foreground` and `cursor` are the system's
/// `label`, which is translucent: the consumer composites them over
/// `background`. The ANSI colours 1–6 and 9–14 are opaque.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TerminalColors {
    /// `label`.
    pub foreground: Hsla,
    /// `control_background`.
    pub background: Hsla,
    /// `label`.
    pub cursor: Hsla,
    /// ANSI 0–15, in the order of SGR 30–37, 90–97.
    pub ansi: [Hsla; 16],
}

/// The ANSI colours 1–6, in order: red, green, yellow, blue, magenta, cyan.
const CHROMATIC: [SystemColor; 6] = [
    SystemColor::Red,
    SystemColor::Green,
    SystemColor::Yellow,
    SystemColor::Blue,
    SystemColor::Purple,
    SystemColor::Cyan,
];

/// The least contrast (WCAG 2) of ANSI 1–6 against the background.
const MIN_CONTRAST: f32 = 3.0;

/// How much of `label` each mixing step adds.
const STEP: f32 = 0.05;

/// The steps are capped: 40 steps of 5 % reach 87 % of `label`, far more
/// than any system colour needs, and the loop ends even for a palette whose
/// `label` has no contrast with its background.
const MAX_STEPS: usize = 40;

/// The colours of `palette`'s appearance. Pure and cheap (a few dozen
/// float operations), so a view may call it while rendering.
pub fn terminal_colors(palette: &Palette) -> TerminalColors {
    let background = palette.control_background;
    // `label` over the background, opaque: the target of the mixing.
    let label = background.blend(palette.label);
    let [red, green, yellow, blue, magenta, cyan] =
        CHROMATIC.map(|color| legible(palette.system(color), label, background));
    let (black, white) = match palette.appearance {
        Appearance::Light => (palette.label, palette.tertiary_label),
        Appearance::Dark => (palette.tertiary_label, palette.label),
    };
    let bright_black = palette.secondary_label;
    TerminalColors {
        foreground: palette.label,
        background,
        cursor: palette.label,
        ansi: [
            black,
            red,
            green,
            yellow,
            blue,
            magenta,
            cyan,
            white,
            bright_black,
            red,
            green,
            yellow,
            blue,
            magenta,
            cyan,
            white,
        ],
    }
}

/// `color` mixed towards `label` by `STEP` until it has `MIN_CONTRAST`
/// against `background`: a rule derived from the palette, no colour value
/// written down (like the diff backgrounds).
fn legible(color: Hsla, label: Hsla, background: Hsla) -> Hsla {
    let mut color = color;
    for _ in 0..MAX_STEPS {
        if contrast(color, background) >= MIN_CONTRAST {
            break;
        }
        color = over(color, label, STEP);
    }
    color
}

/// WCAG 2 contrast ratio of two opaque colours.
fn contrast(a: Hsla, b: Hsla) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// WCAG 2 relative luminance.
fn luminance(color: Hsla) -> f32 {
    let Rgba { r, g, b, .. } = Rgba::from(color);
    let linear = |c: f32| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}
