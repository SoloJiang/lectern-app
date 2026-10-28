//! System color tokens, motion primitives and shared controls on top of
//! gpui-component (docs/design/ui.md).
//!
//! Every view crate takes its colours, fonts, text and code colouring from
//! here and only from here (ADR 0015): no colour value, font name or duration
//! appears in a view. This crate reads the system through
//! `lectern_platform::appearance` and GPUI, does no IO and starts no thread.
//!
//! Step 1 of the design's acceptance: initialisation (`init`, `ready`,
//! `init_fixed`, `attach`), colours (`Palette`, `colors`, `palette`), text
//! (`Text`, `strings`, `set_locale`), fonts (`TextRole`, `MonoMetrics`,
//! `mono_metrics`), code colouring (`code_style`, `code_styles`) and the
//! `contrib` payload types. Step 2: the `motion` module (durations, curves,
//! `Transition`, reduced motion). Step 3: the shared controls (`Button`,
//! `IconAssets`, `AgentDot`, `CircleCheck`, `Progress`, `Segmented`,
//! `GroupedList`, `Row`, `loading_phase`, `Skeleton`, `Expander`, `Hint`)
//! and the `semantic` node types they declare. Step 4 adds the probe's
//! recording to `semantic`. Step 5: the second batch of controls (`ActionCard`,
//! `PopUpButton`, `show_menu`, `SidebarRow`, `Capsule`, `Disclosure`,
//! `Segmented::fill`, `Button::pill`, `Row::context_menu`), `Palette::fill`
//! and `fill_strong`, and six more icons.

mod code;
pub mod contrib;
mod controls;
mod font;
mod icon;
mod init;
pub mod motion;
mod palette;
pub mod semantic;
mod state;
mod text;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

#[cfg(test)]
mod tests;

pub use code::{CodeStyle, code_style, code_styles};
pub use controls::{
    ActionCard, Agent, AgentDot, AppIcon, Button, ButtonKind, ButtonSize, Capsule, CircleCheck,
    Disclosure, Expander, GroupedList, Hint, HintFrame, LoadingPhase, MenuItem, PopUpButton,
    Progress, ProgressShape, Row, Segmented, SidebarRow, Skeleton, loading_phase, show_menu,
};
pub use font::{
    MONO_FALLBACK_FAMILY, MONO_FAMILY, MonoMetrics, TextRole, TextRoleExt, UI_FAMILY, mono_metrics,
};
pub use icon::Icon;
/// The SVG source of the sixteen icons of `Icon` and of the app icon, and
/// only those; every other path answers `Ok(None)`. `app` passes it to `Application::with_assets`
/// before `run`, the one place GPUI takes an asset source; without it every
/// icon paints blank.
pub use icon::IconAssets;
pub use init::{attach, init, init_fixed, is_ready, ready};
pub use palette::{
    Appearance, Palette, SystemColor, accent_follows_system, colors, on_accent, palette,
};
pub use text::{Text, Verbatim, set_locale, strings};
