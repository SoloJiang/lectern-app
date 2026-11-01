//! The views (docs/design/conversation.md「视图」): the inspector's page
//! ([`ConversationTab`]) and the Messages page of a session
//! ([`MessagesModel`]). Both read snapshots and send commands; neither does
//! IO.

mod messages;
mod tab;

use gpui::{AnyElement, App, IntoElement, ParentElement, Styled, Window, div};
use lectern_i18n::Key;
use lectern_ui::semantic::{NodeId, Role, Semantic, semantic};
use lectern_ui::{Text, TextRole, TextRoleExt, strings};

pub(crate) use messages::{MessagesEvent, MessagesModel};
pub use tab::{ConversationTab, Page};

/// One line of secondary text from a message of the resource files, as a
/// `Text` node: the empty states and the notices that stand in for a page.
pub(crate) fn text_line(node: NodeId, key: Key, window: &Window, cx: &App) -> AnyElement {
    let text = Text::key(&strings(cx), key);
    semantic(
        Semantic::new(node, Role::Text).label(&text),
        div()
            .w_full()
            .text_role(TextRole::Secondary, window, cx)
            .child(text.shared()),
    )
    .into_any_element()
}
