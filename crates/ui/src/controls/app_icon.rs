//! The app icon tile (the top of Welcome's left column): the app's artwork,
//! one SVG with the tile, its gradient and the glyph in it (`icon::APP_ICON`,
//! served by `IconAssets`), drawn as an image so that its own colours show;
//! the `svg` element would paint it in one colour. Decoration: it declares
//! no node. The shadow is GPUI's `shadow_lg`: the canvas's own (0 8 24 at
//! 25 % black) would need a colour value here.

use gpui::{App, IntoElement, ParentElement, Pixels, RenderOnce, Styled, Window, div, img};

use crate::controls::require_ready;
use crate::icon::APP_ICON_PATH;

/// The tile's corner radius over its side, the canvas's 22 of 96.
const RADIUS_RATIO: f32 = 22.0 / 96.0;

#[derive(IntoElement)]
pub struct AppIcon {
    size: Pixels,
}

impl AppIcon {
    /// A square tile of `size` (the canvas's is 96pt).
    pub fn new(size: Pixels) -> AppIcon {
        AppIcon { size }
    }
}

impl RenderOnce for AppIcon {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        require_ready(cx);
        div()
            .size(self.size)
            .flex_shrink_0()
            .rounded(self.size * RADIUS_RATIO)
            .shadow_lg()
            .child(img(APP_ICON_PATH).size_full())
    }
}
