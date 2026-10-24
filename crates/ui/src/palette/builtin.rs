//! The built-in colour table (docs/design/ui.md「数据结构」): what the
//! palettes start from before `ready` reads the system, and what they stay
//! when `platform` cannot read it (Linux `cargo check`, headless, tests, a
//! failed AppKit call). The only file of this crate that holds colour values.
//!
//! Source: the sRGB components that `lectern_platform::appearance::system_colors`
//! resolves from AppKit's `NSColor` on macOS 27.0.1 (Apple silicon), read on
//! 2026-10-23, light (`NSAppearanceNameAqua`) and dark
//! (`NSAppearanceNameDarkAqua`), with the default accent colour (blue). Apple's
//! HIG「Color」page publishes the 13 system colours of an earlier macOS; they
//! differ from these by a few units of 255, and this table follows the OS so
//! that the fallback and the live reading look the same.
//!
//! The agent identification colours come from the interaction spec's「视觉」
//! (the one custom colour of ADR 0015) and are never read from the system.

use gpui::{Hsla, Rgba};
use lectern_platform::appearance::{SYSTEM_COLOR_COUNT, Srgba, SystemColors};

use super::Appearance;

const fn srgba(r: f32, g: f32, b: f32, a: f32) -> Srgba {
    Srgba { r, g, b, a }
}

/// The light table, in the field order of `SystemColors`.
const LIGHT: SystemColors = SystemColors {
    label: srgba(0.0, 0.0, 0.0, 0.8471),
    secondary_label: srgba(0.0, 0.0, 0.0, 0.4980),
    tertiary_label: srgba(0.0, 0.0, 0.0, 0.2588),
    separator: srgba(0.0, 0.0, 0.0, 0.0980),
    window_background: srgba(1.0, 1.0, 1.0, 1.0),
    control_background: srgba(1.0, 1.0, 1.0, 1.0),
    under_page_background: srgba(0.9647, 0.9647, 0.9647, 1.0),
    accent: srgba(0.0, 0.4784, 1.0, 1.0),
    selected_content_background: srgba(0.0, 0.3922, 0.8824, 1.0),
    unemphasized_selected_content_background: srgba(0.8627, 0.8627, 0.8627, 1.0),
    system: [
        srgba(1.0, 0.2196, 0.2353, 1.0),    // systemRed
        srgba(1.0, 0.5529, 0.1569, 1.0),    // systemOrange
        srgba(1.0, 0.8000, 0.0, 1.0),       // systemYellow
        srgba(0.2039, 0.7804, 0.3490, 1.0), // systemGreen
        srgba(0.0, 0.7843, 0.7020, 1.0),    // systemMint
        srgba(0.0, 0.7647, 0.8157, 1.0),    // systemTeal
        srgba(0.0, 0.7529, 0.9098, 1.0),    // systemCyan
        srgba(0.0, 0.5333, 1.0, 1.0),       // systemBlue
        srgba(0.3804, 0.3333, 0.9608, 1.0), // systemIndigo
        srgba(0.7961, 0.1882, 0.8784, 1.0), // systemPurple
        srgba(1.0, 0.1765, 0.3333, 1.0),    // systemPink
        srgba(0.6745, 0.4980, 0.3686, 1.0), // systemBrown
        srgba(0.5569, 0.5569, 0.5765, 1.0), // systemGray
    ],
    accent_available: false,
};

/// The dark table.
const DARK: SystemColors = SystemColors {
    label: srgba(1.0, 1.0, 1.0, 0.8471),
    secondary_label: srgba(1.0, 1.0, 1.0, 0.5490),
    tertiary_label: srgba(1.0, 1.0, 1.0, 0.2471),
    separator: srgba(1.0, 1.0, 1.0, 0.0980),
    window_background: srgba(0.1176, 0.1176, 0.1176, 1.0),
    control_background: srgba(0.1176, 0.1176, 0.1176, 1.0),
    under_page_background: srgba(0.1569, 0.1569, 0.1569, 1.0),
    accent: srgba(0.0, 0.4784, 1.0, 1.0),
    selected_content_background: srgba(0.0, 0.3490, 0.8196, 1.0),
    unemphasized_selected_content_background: srgba(0.2745, 0.2745, 0.2745, 1.0),
    system: [
        srgba(1.0, 0.2588, 0.2706, 1.0),    // systemRed
        srgba(1.0, 0.5725, 0.1882, 1.0),    // systemOrange
        srgba(1.0, 0.8392, 0.0, 1.0),       // systemYellow
        srgba(0.1882, 0.8196, 0.3451, 1.0), // systemGreen
        srgba(0.0, 0.8549, 0.7647, 1.0),    // systemMint
        srgba(0.0, 0.8235, 0.8784, 1.0),    // systemTeal
        srgba(0.2353, 0.8275, 0.9961, 1.0), // systemCyan
        srgba(0.0, 0.5686, 1.0, 1.0),       // systemBlue
        srgba(0.4275, 0.4863, 1.0, 1.0),    // systemIndigo
        srgba(0.8588, 0.2039, 0.9490, 1.0), // systemPurple
        srgba(1.0, 0.2157, 0.3725, 1.0),    // systemPink
        srgba(0.7176, 0.5412, 0.4000, 1.0), // systemBrown
        srgba(0.5961, 0.5961, 0.6157, 1.0), // systemGray
    ],
    accent_available: false,
};

const _: () = assert!(LIGHT.system.len() == SYSTEM_COLOR_COUNT);

/// The built-in table of one appearance. `accent_available` is false: the
/// accent is the table's blue, not the user's setting.
pub(super) fn system_colors(appearance: Appearance) -> SystemColors {
    match appearance {
        Appearance::Light => LIGHT,
        Appearance::Dark => DARK,
    }
}

/// Agent identification (interaction spec「视觉」): Claude Code, light
/// `#D9790A` / dark `#C77A1A`.
pub(super) fn agent_claude(appearance: Appearance) -> Hsla {
    match appearance {
        Appearance::Light => hex(0xD9790A),
        Appearance::Dark => hex(0xC77A1A),
    }
}

/// Agent identification: Codex, light `#5856D6` / dark `#6260D8`.
pub(super) fn agent_codex(appearance: Appearance) -> Hsla {
    match appearance {
        Appearance::Light => hex(0x5856D6),
        Appearance::Dark => hex(0x6260D8),
    }
}

/// Text on the accent colour: AppKit's `alternateSelectedControlTextColor`,
/// white in both appearances. Used only for gpui-component's
/// `primary_foreground` (`init::project`) and by the views that put text of
/// their own inside a selected `Row` (`lectern_ui::on_accent`); not a
/// `Palette` field.
pub fn on_accent() -> Hsla {
    hex(0xFFFFFF)
}

fn hex(rgb: u32) -> Hsla {
    Hsla::from(gpui::rgb(rgb))
}

/// The `Srgba` of `platform` as GPUI's `Hsla`.
pub(super) fn convert(color: Srgba) -> Hsla {
    Hsla::from(Rgba {
        r: color.r,
        g: color.g,
        b: color.b,
        a: color.a,
    })
}
