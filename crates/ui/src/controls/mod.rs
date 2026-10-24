//! The shared controls (docs/design/ui.md「控件」): the few the first
//! screens need that gpui-component lacks or draws unlike macOS. Each is a
//! `RenderOnce` element that takes `Text` (never `&str`), draws with the
//! `Palette`, animates with `motion` and declares its semantic node through
//! `semantic::declare`, so a view gives only the `NodeId`.
//!
//! Every `render` asserts `is_ready`: before `ready` gpui-component's
//! `Theme` global does not exist, and the panic names the assembly error
//! (docs/design/ui.md「状态与异常」).

mod agent_dot;
mod button;
mod circle_check;
mod expander;
mod grouped_list;
mod hint;
mod loading;
mod progress;
mod segmented;

pub use agent_dot::{Agent, AgentDot};
pub use button::{Button, ButtonKind, ButtonSize};
pub use circle_check::CircleCheck;
pub use expander::Expander;
pub use grouped_list::{GroupedList, Row};
pub use hint::{Hint, HintFrame};
pub use loading::{LoadingPhase, Skeleton, loading_phase};
pub use progress::{Progress, ProgressShape};
pub use segmented::Segmented;

use gpui::App;

use crate::init::is_ready;

/// The guard at the top of every control's `render`.
#[track_caller]
pub(crate) fn require_ready(cx: &App) {
    assert!(
        is_ready(cx),
        "a lectern_ui control rendered before lectern_ui::ready: app mounts views only after ready"
    );
}
