//! Code colouring (docs/design/ui.md「代码配色」): `TokenKind` to a system
//! colour and a weight, the same mapping in both appearances; the colours
//! come from the `Palette`.
//!
//! `Type` and `Attribute` are pink, not the design table's indigo: indigo
//! and blue are 2.7 ΔE apart under the deuteranopia simulation in dark mode
//! (`tests::colors::code_colors_are_distinguishable_under_three_visions`),
//! and pink is the one system colour that passes both appearances
//! (docs/design/ui.md「与设计的差异」).

use gpui::{FontWeight, Hsla};
use lectern_syntax::TokenKind;

use crate::palette::{Palette, SystemColor};

/// The style of one `TokenKind`. Every colour comes from the `Palette`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct CodeStyle {
    pub color: Hsla,
    pub weight: FontWeight,
    pub italic: bool,
    pub underline: bool,
}

/// The style of `kind` in `palette`'s appearance. Called per token while
/// rendering: one match and one index.
pub fn code_style(kind: TokenKind, palette: &Palette) -> CodeStyle {
    let plain = |color: Hsla| CodeStyle {
        color,
        weight: FontWeight::NORMAL,
        italic: false,
        underline: false,
    };
    match kind {
        TokenKind::Keyword | TokenKind::Tag => plain(palette.system(SystemColor::Purple)),
        TokenKind::String => plain(palette.system(SystemColor::Red)),
        TokenKind::Number | TokenKind::Constant => plain(palette.system(SystemColor::Blue)),
        TokenKind::Type | TokenKind::Attribute => plain(palette.system(SystemColor::Pink)),
        TokenKind::Comment => plain(palette.secondary_label),
        TokenKind::Link => CodeStyle {
            underline: true,
            ..plain(palette.system(SystemColor::Blue))
        },
        TokenKind::Heading => CodeStyle {
            weight: FontWeight::SEMIBOLD,
            ..plain(palette.label)
        },
        TokenKind::Emphasis => CodeStyle {
            italic: true,
            ..plain(palette.label)
        },
        TokenKind::Function
        | TokenKind::Property
        | TokenKind::Variable
        | TokenKind::Operator
        | TokenKind::Punctuation
        | TokenKind::Escape
        | TokenKind::Other => plain(palette.label),
    }
}

/// The whole table, for the snapshot tests and the probe.
pub fn code_styles(palette: &Palette) -> [(TokenKind, CodeStyle); 18] {
    TokenKind::ALL.map(|kind| (kind, code_style(kind, palette)))
}
