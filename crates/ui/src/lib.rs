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
//! `Transition`, reduced motion). Controls and the probe nodes follow in
//! steps 3 and 4.

mod code;
pub mod contrib;
mod font;
mod icon;
mod init;
pub mod motion;
mod palette;
mod state;
mod text;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

#[cfg(test)]
mod tests;

pub use code::{CodeStyle, code_style, code_styles};
pub use font::{
    MONO_FALLBACK_FAMILY, MONO_FAMILY, MonoMetrics, TextRole, TextRoleExt, UI_FAMILY, mono_metrics,
};
pub use icon::Icon;
pub use init::{attach, init, init_fixed, is_ready, ready};
pub use palette::{Appearance, Palette, SystemColor, accent_follows_system, colors, palette};
pub use text::{Text, Verbatim, set_locale, strings};
