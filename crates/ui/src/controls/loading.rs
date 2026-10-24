//! The loading state (interaction spec「状态与异常」, the「加载」row):
//! `loading_phase` says what to draw after how long, `Skeleton` is the
//! placeholder block. The block does not shimmer, with or without reduced
//! motion: gpui-component's `Skeleton` cannot switch its pulse off, so the
//! block is drawn here.

use std::time::Duration;

use gpui::{App, IntoElement, Pixels, RenderOnce, Styled, Window, div, px};

use crate::controls::require_ready;
use crate::motion::{LOADING_PROGRESS_AT, LOADING_SKELETON_AT};
use crate::palette::colors;

/// What a view draws while it loads. Pure: a function of the elapsed time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LoadingPhase {
    /// Under `LOADING_SKELETON_AT`: nothing, the result may still come first.
    Nothing,
    /// Under `LOADING_PROGRESS_AT`: skeleton blocks where the content goes.
    Skeleton,
    /// Longer: a progress indicator.
    Progress,
}

pub fn loading_phase(elapsed: Duration) -> LoadingPhase {
    if elapsed < LOADING_SKELETON_AT {
        LoadingPhase::Nothing
    } else if elapsed < LOADING_PROGRESS_AT {
        LoadingPhase::Skeleton
    } else {
        LoadingPhase::Progress
    }
}

/// A skeleton block: `tertiary_label` thinned, rounded, still. Decoration;
/// it declares no node.
#[derive(IntoElement)]
pub struct Skeleton {
    width: Pixels,
    height: Pixels,
}

const RADIUS: Pixels = px(4.0);
/// How much of `tertiary_label` the block keeps.
const OPACITY: f32 = 0.35;

impl Skeleton {
    pub fn new(width: Pixels, height: Pixels) -> Skeleton {
        Skeleton { width, height }
    }
}

impl RenderOnce for Skeleton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        require_ready(cx);
        let palette = colors(window, cx);
        div()
            .w(self.width)
            .h(self.height)
            .flex_shrink_0()
            .rounded(RADIUS)
            .bg(palette.tertiary_label.opacity(OPACITY))
    }
}
