//! Colours (docs/design/ui.md「颜色」): the system semantic colours of one
//! appearance as a `Palette`, resolved from `platform` or the built-in table.

mod builtin;

use gpui::{App, Hsla, Window, WindowAppearance};
use lectern_platform::appearance::{self as platform, SYSTEM_COLOR_COUNT, SystemColors};

use crate::state::UiState;

pub(crate) use builtin::on_accent;

/// A window's appearance, light or dark.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Appearance {
    Light,
    Dark,
}

impl Appearance {
    /// `WindowAppearance::{Light, VibrantLight}` → `Light`, `{Dark, VibrantDark}` → `Dark`.
    pub fn of(window: WindowAppearance) -> Appearance {
        match window {
            WindowAppearance::Light | WindowAppearance::VibrantLight => Appearance::Light,
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Appearance::Dark,
        }
    }

    /// The log field's name: `light`, `dark`.
    pub fn as_str(self) -> &'static str {
        match self {
            Appearance::Light => "light",
            Appearance::Dark => "dark",
        }
    }

    pub(crate) const ALL: [Appearance; 2] = [Appearance::Light, Appearance::Dark];

    pub(crate) fn index(self) -> usize {
        match self {
            Appearance::Light => 0,
            Appearance::Dark => 1,
        }
    }

    pub(crate) fn to_platform(self) -> platform::Appearance {
        match self {
            Appearance::Light => platform::Appearance::Light,
            Appearance::Dark => platform::Appearance::Dark,
        }
    }
}

/// The 13 system colours, `systemRed` … `systemGray`, in the order of
/// `Palette::system` and `platform::SystemColors::system`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
pub enum SystemColor {
    Red,
    Orange,
    Yellow,
    Green,
    Mint,
    Teal,
    Cyan,
    Blue,
    Indigo,
    Purple,
    Pink,
    Brown,
    Gray,
}

impl SystemColor {
    pub const COUNT: usize = 13;

    /// Every colour, in discriminant order.
    pub const ALL: [SystemColor; SystemColor::COUNT] = [
        SystemColor::Red,
        SystemColor::Orange,
        SystemColor::Yellow,
        SystemColor::Green,
        SystemColor::Mint,
        SystemColor::Teal,
        SystemColor::Cyan,
        SystemColor::Blue,
        SystemColor::Indigo,
        SystemColor::Purple,
        SystemColor::Pink,
        SystemColor::Brown,
        SystemColor::Gray,
    ];
}

const _: () = assert!(SystemColor::COUNT == SYSTEM_COLOR_COUNT);

/// Every colour of one appearance. The fields are GPUI's `Hsla`, converted
/// from the sRGB components `platform` gives.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Palette {
    pub appearance: Appearance,
    // Text and separators: labelColor, secondaryLabelColor, tertiaryLabelColor, separatorColor.
    pub label: Hsla,
    pub secondary_label: Hsla,
    pub tertiary_label: Hsla,
    pub separator: Hsla,
    // Surfaces: windowBackgroundColor, controlBackgroundColor, underPageBackgroundColor.
    pub window_background: Hsla,
    pub control_background: Hsla,
    pub under_page_background: Hsla,
    // Accent and selection: controlAccentColor, selectedContentBackgroundColor,
    // unemphasizedSelectedContentBackgroundColor.
    pub accent: Hsla,
    pub selected_content_background: Hsla,
    pub unemphasized_selected_content_background: Hsla,
    /// systemRed … systemGray, in the order of `SystemColor`.
    pub system: [Hsla; SystemColor::COUNT],
    /// Derived: `system(Green)` / `system(Red)` over `control_background` at
    /// this appearance's opacity (the「淡底色」of the interaction spec).
    pub diff_added_background: Hsla,
    pub diff_removed_background: Hsla,
    /// The one custom colour (ADR 0015): agent identification.
    pub agent_claude: Hsla,
    pub agent_codex: Hsla,
}

/// The opacity of the diff backgrounds: light 12 %, dark 18 %
/// (docs/design/ui.md「数据结构」).
const DIFF_OPACITY: [f32; 2] = [0.12, 0.18];

impl Palette {
    pub fn system(&self, color: SystemColor) -> Hsla {
        self.system[color as usize]
    }

    /// The palette of `colors` read from the system (`ready`, a change
    /// notification) or taken from the built-in table.
    pub(crate) fn from_system(appearance: Appearance, colors: &SystemColors) -> Palette {
        let mut system = [Hsla::default(); SystemColor::COUNT];
        for (slot, color) in system.iter_mut().zip(colors.system.iter()) {
            *slot = builtin::convert(*color);
        }
        let control_background = builtin::convert(colors.control_background);
        let opacity = DIFF_OPACITY[appearance.index()];
        Palette {
            appearance,
            label: builtin::convert(colors.label),
            secondary_label: builtin::convert(colors.secondary_label),
            tertiary_label: builtin::convert(colors.tertiary_label),
            separator: builtin::convert(colors.separator),
            window_background: builtin::convert(colors.window_background),
            control_background,
            under_page_background: builtin::convert(colors.under_page_background),
            accent: builtin::convert(colors.accent),
            selected_content_background: builtin::convert(colors.selected_content_background),
            unemphasized_selected_content_background: builtin::convert(
                colors.unemphasized_selected_content_background,
            ),
            system,
            diff_added_background: over(
                control_background,
                system[SystemColor::Green as usize],
                opacity,
            ),
            diff_removed_background: over(
                control_background,
                system[SystemColor::Red as usize],
                opacity,
            ),
            agent_claude: builtin::agent_claude(appearance),
            agent_codex: builtin::agent_codex(appearance),
        }
    }

    /// The built-in table's palette.
    pub(crate) fn builtin(appearance: Appearance) -> Palette {
        Palette::from_system(appearance, &builtin::system_colors(appearance))
    }
}

/// `top` at `opacity` composited over `base` (source-over); opaque result.
pub(crate) fn over(base: Hsla, top: Hsla, opacity: f32) -> Hsla {
    base.blend(top.alpha(opacity))
}

/// The colours of the window's appearance. Called many times per frame: one
/// global lookup and one index, no allocation.
pub fn colors<'a>(window: &Window, cx: &'a App) -> &'a Palette {
    let state = cx.global::<UiState>();
    let appearance = state
        .fixed_appearance
        .unwrap_or_else(|| Appearance::of(window.appearance()));
    state.palette(appearance)
}

/// The colours of an appearance, for places without a window (tests, the
/// colour table snapshots).
pub fn palette(appearance: Appearance, cx: &App) -> &Palette {
    cx.global::<UiState>().palette(appearance)
}

/// Whether the accent colour comes from the system (`platform` read
/// `controlAccentColor`). Before `ready`, and when it cannot be read, the
/// accent is the built-in table's blue.
pub fn accent_follows_system(cx: &App) -> bool {
    cx.global::<UiState>().accent_follows_system
}
