//! Colours (docs/design/terminal_view.md「数据结构」「颜色」): the `Theme`
//! given to `terminal` is `ui`'s terminal colours composited onto the
//! terminal's background; the colours of a frame (`Rgb`) become GPUI colours
//! here. No colour value is written in this crate (ADR 0015).

use gpui::{App, Hsla, Rgba, Window};
use lectern_services::terminal::{ColorScheme, Rgb, Theme};
use lectern_ui::{Appearance, Palette, colors, terminal_colors};

/// The terminal colours of this window's current appearance (`ui::colors`,
/// which honours `ui`'s fixed appearance), as a `Theme`. The host calls it
/// before `Terminals::open`; the pane calls it on every render and sends
/// `SetTheme` when it changed.
pub fn theme(window: &Window, cx: &App) -> Theme {
    theme_of(colors(window, cx))
}

/// `theme` without the window: each colour of `ui::terminal_colors(palette)`
/// laid over the terminal's background (the system's label colours are
/// translucent), as an opaque `Rgb`. The scheme is the palette's appearance.
pub fn theme_of(palette: &Palette) -> Theme {
    let terminal = terminal_colors(palette);
    let background = terminal.background;
    let flat = |color: Hsla| rgb_of(background.blend(color));
    Theme {
        foreground: flat(terminal.foreground),
        background: rgb_of(background),
        cursor: flat(terminal.cursor),
        ansi: terminal.ansi.map(flat),
        scheme: match palette.appearance {
            Appearance::Light => ColorScheme::Light,
            Appearance::Dark => ColorScheme::Dark,
        },
    }
}

/// An opaque colour as three bytes.
pub(crate) fn rgb_of(color: Hsla) -> Rgb {
    let Rgba { r, g, b, .. } = Rgba::from(color);
    let byte = |channel: f32| (channel * 255.0).round().clamp(0.0, 255.0) as u8;
    Rgb {
        r: byte(r),
        g: byte(g),
        b: byte(b),
    }
}

/// A colour of a frame or of a `Theme` as a GPUI colour.
pub(crate) fn color_of(color: Rgb) -> Hsla {
    Rgba {
        r: f32::from(color.r) / 255.0,
        g: f32::from(color.g) / 255.0,
        b: f32::from(color.b) / 255.0,
        a: 1.0,
    }
    .into()
}

/// `color_of` for the many cells of a frame: the last conversion is
/// remembered, because neighbouring cells mostly share their colour.
pub(crate) struct ColorMemo {
    last: Option<(Rgb, Hsla)>,
}

impl ColorMemo {
    pub(crate) fn new() -> ColorMemo {
        ColorMemo { last: None }
    }

    pub(crate) fn get(&mut self, color: Rgb) -> Hsla {
        match self.last {
            Some((rgb, hsla)) if rgb == color => hsla,
            _ => {
                let hsla = color_of(color);
                self.last = Some((color, hsla));
                hsla
            }
        }
    }
}
