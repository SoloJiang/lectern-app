//! The shared controls (docs/design/ui.md「控件」): the few the first
//! screens need that gpui-component lacks or draws unlike macOS (step 5 adds
//! the second batch: `ActionCard`, `PopUpButton`, `SidebarRow`, `Capsule`,
//! `Disclosure`, and the decorative `AppIcon`; `TextArea` is the multi-line
//! text box of conversation and the review notes). Each is a
//! `RenderOnce` element that takes `Text` (never `&str`), draws with the
//! `Palette`, animates with `motion` and declares its semantic node through
//! `semantic::declare`, so a view gives only the `NodeId`.
//!
//! Every `render` asserts `is_ready`: before `ready` gpui-component's
//! `Theme` global does not exist, and the panic names the assembly error
//! (docs/design/ui.md「状态与异常」).

mod action_card;
mod agent_dot;
mod app_icon;
mod button;
mod capsule;
mod circle_check;
mod disclosure;
mod expander;
mod grouped_list;
mod hint;
mod hover;
mod loading;
mod menu;
mod progress;
mod segmented;
mod sidebar_row;
mod text_area;

pub use action_card::ActionCard;
pub use agent_dot::{Agent, AgentDot};
pub use app_icon::AppIcon;
pub use button::{Button, ButtonKind, ButtonSize};
pub use capsule::Capsule;
pub use circle_check::CircleCheck;
#[cfg(test)]
pub(crate) use circle_check::Mark;
pub use disclosure::Disclosure;
pub use expander::Expander;
pub use grouped_list::{GroupedList, Row};
pub use hint::{Hint, HintFrame};
pub use loading::{LoadingPhase, Skeleton, loading_phase};
pub use menu::{MenuItem, PopUpButton, show_menu};
pub use progress::{Progress, ProgressShape};
pub use segmented::Segmented;
#[cfg(test)]
pub(crate) use segmented::Selection;
pub use sidebar_row::SidebarRow;
#[cfg(test)]
pub(crate) use text_area::engine;
pub use text_area::{TextArea, TextAreaState};

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
