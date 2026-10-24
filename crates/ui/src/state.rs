//! The one GPUI global of this crate (docs/design/ui.md「数据结构」): two
//! palettes, the strings, the reduce-motion flag, the mono metrics cache and
//! the platform watch. Fixed size; nothing in it grows while the app runs.

use gpui::{AnyWindowHandle, Global, Pixels};
use lectern_i18n::Strings;
use lectern_platform::appearance::Watch;

use crate::font::MonoMetrics;
use crate::palette::{Appearance, Palette};

/// The number of mono metrics the cache holds: `TextRole::Code` and the
/// future zoom steps (docs/design/ui.md「字体与度量」).
pub(crate) const MONO_CACHE_SLOTS: usize = 4;

pub(crate) struct UiState {
    /// Indexed by `Appearance::index`.
    pub palettes: [Palette; 2],
    pub strings: Strings,
    pub reduced_motion: bool,
    pub accent_follows_system: bool,
    /// `init_fixed`'s appearance: when set, `colors` uses it instead of the
    /// window's (the test platform cannot change a window's appearance).
    pub fixed_appearance: Option<Appearance>,
    pub ready: bool,
    /// Keyed by font size; empty slots are None.
    pub mono_cache: [Option<(Pixels, MonoMetrics)>; MONO_CACHE_SLOTS],
    /// How many times a font was resolved for `mono_metrics` (test hook).
    pub mono_resolutions: u32,
    /// Windows `attach` has registered an observer for.
    pub attached: Vec<AnyWindowHandle>,
    /// Dropped with the app; keeps the platform notifications registered.
    #[allow(
        dead_code,
        reason = "held so that the platform notifications stay registered"
    )]
    pub watch: Option<Watch>,
}

impl Global for UiState {}

impl UiState {
    pub(crate) fn palette(&self, appearance: Appearance) -> &Palette {
        &self.palettes[appearance.index()]
    }
}
