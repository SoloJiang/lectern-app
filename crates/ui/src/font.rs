//! Fonts and metrics (docs/design/ui.md「字体与度量」): the system font
//! families, the text size roles and the monospaced metrics `file_reader`
//! and the diff rows lay their cells out with.

use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{App, Font, FontId, FontWeight, Hsla, Pixels, Styled, Window, font, px};
use lectern_logging::{error, warn};

use crate::palette::{Palette, colors};
use crate::state::{MONO_CACHE_SLOTS, UiState};

/// The system UI font. GPUI resolves this special name to Core Text's
/// `.AppleSystemUIFont` (SF Pro, PingFang for Chinese), not by a visible
/// font name (ADR 0017).
pub const UI_FAMILY: &str = ".SystemUIFont";
/// The system monospaced font's family name; Core Text matches it to
/// `.AppleSystemUIFontMonospaced-*` (SF Mono).
pub const MONO_FAMILY: &str = ".AppleSystemUIFontMonospaced";
/// Used when `MONO_FAMILY` does not resolve; a warn is logged then.
pub const MONO_FALLBACK_FAMILY: &str = "Menlo";

/// A family that exists nowhere: resolving it gives the `FontId` of GPUI's
/// own fallback stack, which a family that failed to resolve also gives.
const PROBE_FAMILY: &str = ".LecternNoSuchFamily";

/// The text size roles (interaction spec「视觉」).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TextRole {
    /// 40pt, the project home's large title.
    LargeTitle,
    /// 26pt.
    Title,
    /// 22pt.
    Subtitle,
    /// 13pt, body text.
    Body,
    /// 13pt, `secondary_label`.
    Secondary,
    /// 11pt, `secondary_label`.
    Caption,
    /// SF Mono 12pt.
    Code,
}

impl TextRole {
    pub fn size(self) -> Pixels {
        match self {
            TextRole::LargeTitle => px(40.0),
            TextRole::Title => px(26.0),
            TextRole::Subtitle => px(22.0),
            TextRole::Body | TextRole::Secondary => px(13.0),
            TextRole::Caption => px(11.0),
            TextRole::Code => px(12.0),
        }
    }

    /// The line height of flowing text in this role. Code lines in the
    /// self-drawn lists use `MonoMetrics::line_height` instead.
    pub fn line_height(self) -> Pixels {
        match self {
            TextRole::LargeTitle => px(48.0),
            TextRole::Title => px(32.0),
            TextRole::Subtitle => px(28.0),
            TextRole::Body | TextRole::Secondary | TextRole::Code => px(16.0),
            TextRole::Caption => px(14.0),
        }
    }

    /// The font: the system UI family, or the monospaced family for `Code`;
    /// large titles bold, titles semibold, the rest regular.
    pub fn font(self) -> Font {
        let mut font = font(self.family());
        font.weight = self.weight();
        font
    }

    fn family(self) -> &'static str {
        match self {
            TextRole::Code => MONO_FAMILY,
            _ => UI_FAMILY,
        }
    }

    fn weight(self) -> FontWeight {
        match self {
            TextRole::LargeTitle => FontWeight::BOLD,
            TextRole::Title => FontWeight::SEMIBOLD,
            _ => FontWeight::NORMAL,
        }
    }

    fn color(self, palette: &Palette) -> Hsla {
        match self {
            TextRole::Secondary | TextRole::Caption => palette.secondary_label,
            _ => palette.label,
        }
    }
}

/// Puts a text role on a `Styled` element: family, size, line height,
/// weight and colour.
pub trait TextRoleExt: Styled + Sized {
    fn text_role(self, role: TextRole, window: &Window, cx: &App) -> Self;
}

impl<E: Styled> TextRoleExt for E {
    fn text_role(self, role: TextRole, window: &Window, cx: &App) -> Self {
        let palette = colors(window, cx);
        self.font_family(role.family())
            .font_weight(role.weight())
            .text_size(role.size())
            .line_height(role.line_height())
            .text_color(role.color(palette))
    }
}

/// The metrics of the monospaced font, which `file_reader`'s self-drawn list
/// and `review_view`'s diff rows lay their cells and rows out with.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct MonoMetrics {
    /// One cell: `TextSystem::em_advance` at `TextRole::Code`'s size (SF Mono
    /// is monospaced, so every ASCII glyph has it).
    pub cell_width: Pixels,
    /// `ascent + descent`, rounded up to 0.5pt.
    pub line_height: Pixels,
    pub ascent: Pixels,
    pub descent: Pixels,
    /// The family these metrics are of: `MONO_FAMILY` when it resolved,
    /// otherwise `MONO_FALLBACK_FAMILY`, otherwise `UI_FAMILY`.
    pub family: &'static str,
}

static WARNED_MONO: AtomicBool = AtomicBool::new(false);
static ERRORED_MONO: AtomicBool = AtomicBool::new(false);

/// The metrics of `TextRole::Code`. The first call resolves the font through
/// the window's text system and measures it (main thread, tens of
/// microseconds; the font is mapped by the system, no file of Lectern's is
/// read); later calls return the cached value. The cache holds one entry per
/// size, at most `MONO_CACHE_SLOTS`, and does not grow.
pub fn mono_metrics(window: &Window, cx: &mut App) -> MonoMetrics {
    let size = TextRole::Code.size();
    if let Some(metrics) = cx
        .global::<UiState>()
        .mono_cache
        .iter()
        .flatten()
        .find_map(|(cached, metrics)| (*cached == size).then_some(*metrics))
    {
        return metrics;
    }
    let metrics = measure(window, size);
    let state = cx.global_mut::<UiState>();
    state.mono_resolutions += 1;
    let slot = state
        .mono_cache
        .iter()
        .position(Option::is_none)
        .unwrap_or(MONO_CACHE_SLOTS - 1);
    state.mono_cache[slot] = Some((size, metrics));
    metrics
}

fn measure(window: &Window, size: Pixels) -> MonoMetrics {
    let text_system = window.text_system();
    let (font_id, family) = resolve_mono(|family| text_system.resolve_font(&font(family)));
    let cell_width = text_system
        .em_advance(font_id, size)
        .unwrap_or_else(|_| text_system.layout_width(font_id, size, 'm'));
    let ascent = text_system.ascent(font_id, size);
    // GPUI reports the descent as the font does: below the baseline, so
    // negative on Core Text and in its test text system.
    let descent = text_system.descent(font_id, size).abs();
    let line_height = px(((ascent + descent).as_f32() * 2.0).ceil() / 2.0);
    MonoMetrics {
        cell_width,
        line_height,
        ascent,
        descent,
        family,
    }
}

/// Which family the code font is: `MONO_FAMILY`, else the fallback, else the
/// UI font. GPUI has no failure signal from `resolve_font`: a family that
/// does not resolve gives the `FontId` of GPUI's own fallback stack, the same
/// as `PROBE_FAMILY` gives. A text system that gives every family the same
/// id (GPUI's test platform) cannot be probed; `MONO_FAMILY` is taken as
/// resolved then.
fn resolve_mono(resolve: impl Fn(&'static str) -> FontId) -> (FontId, &'static str) {
    let probe = resolve(PROBE_FAMILY);
    let ui = resolve(UI_FAMILY);
    let mono = resolve(MONO_FAMILY);
    if probe == ui || mono != probe {
        return (mono, MONO_FAMILY);
    }
    let fallback = resolve(MONO_FALLBACK_FAMILY);
    if fallback != probe {
        if !WARNED_MONO.swap(true, Ordering::Relaxed) {
            warn!("the system monospaced font is unavailable; the fallback family is used"; family = MONO_FALLBACK_FAMILY);
        }
        return (fallback, MONO_FALLBACK_FAMILY);
    }
    if !ERRORED_MONO.swap(true, Ordering::Relaxed) {
        error!("no monospaced font resolved; the UI font is used for code");
    }
    (ui, UI_FAMILY)
}

#[cfg(test)]
pub(crate) fn resolve_mono_for_test(
    resolve: impl Fn(&'static str) -> FontId,
) -> (FontId, &'static str) {
    resolve_mono(resolve)
}
