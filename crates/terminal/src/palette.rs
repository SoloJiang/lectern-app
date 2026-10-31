//! The 256-colour palette derived from a `Theme` (design 「网格与颜色」).

use lectern_services::terminal::{Rgb, Theme};
use libghostty_vt::style::{Palette, RgbColor};

pub(crate) fn rgb(color: Rgb) -> RgbColor {
    RgbColor {
        r: color.r,
        g: color.g,
        b: color.b,
    }
}

pub(crate) fn from_vt(color: RgbColor) -> Rgb {
    Rgb {
        r: color.r,
        g: color.g,
        b: color.b,
    }
}

/// ANSI 0–15 from the theme; 16–231 the xterm 6×6×6 cube; 232–255 the grey ramp.
pub(crate) fn palette(theme: &Theme) -> Palette {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let mut colors = [RgbColor { r: 0, g: 0, b: 0 }; 256];
    for (slot, color) in colors.iter_mut().zip(theme.ansi) {
        *slot = rgb(color);
    }
    for i in 0..216 {
        colors[16 + i] = RgbColor {
            r: LEVELS[i / 36],
            g: LEVELS[(i / 6) % 6],
            b: LEVELS[i % 6],
        };
    }
    for i in 0..24 {
        let v = 8 + 10 * i as u8;
        colors[232 + i] = RgbColor { r: v, g: v, b: v };
    }
    Palette(colors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lectern_services::terminal::ColorScheme;

    #[test]
    fn palette_cube_and_ramp_follow_xterm() {
        let c = |r, g, b| Rgb { r, g, b };
        let mut ansi = [c(0, 0, 0); 16];
        ansi[1] = c(1, 2, 3);
        let theme = Theme {
            foreground: c(0, 0, 0),
            background: c(0, 0, 0),
            cursor: c(0, 0, 0),
            ansi,
            scheme: ColorScheme::Dark,
        };
        let p = palette(&theme).0;
        assert_eq!(from_vt(p[1]), c(1, 2, 3));
        assert_eq!(from_vt(p[16]), c(0, 0, 0));
        assert_eq!(from_vt(p[196]), c(255, 0, 0));
        assert_eq!(from_vt(p[231]), c(255, 255, 255));
        assert_eq!(from_vt(p[232]), c(8, 8, 8));
        assert_eq!(from_vt(p[255]), c(238, 238, 238));
    }
}
