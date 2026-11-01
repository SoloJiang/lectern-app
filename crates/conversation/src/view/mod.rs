//! The views (docs/design/conversation.md「视图」): the inspector's page
//! ([`ConversationTab`]) and the Messages page of a session
//! ([`MessagesModel`]). Both read snapshots and send commands; neither does
//! IO.

mod messages;
mod tab;
mod terminal;

use gpui::{AnyElement, App, IntoElement, ParentElement, Styled, Window, div};
use lectern_i18n::{Key, Strings, keys};
use lectern_ui::semantic::{NodeId, Role, Semantic, semantic};
use lectern_ui::{Text, TextRole, TextRoleExt, strings};

use crate::nodes;

pub(crate) use messages::{MessagesEvent, MessagesModel};
pub use tab::{ConversationTab, Page};
pub(crate) use terminal::TerminalPage;

/// The line "the session may still run somewhere else", above the reply box
/// and above the resume button. Only a hint: it stops nothing.
pub(crate) fn elsewhere_line(strings: &Strings, window: &Window, cx: &App) -> AnyElement {
    let line = Text::key(strings, keys::conversation::ELSEWHERE);
    semantic(
        Semantic::new(nodes::area("elsewhere"), Role::Text).label(&line),
        div()
            .w_full()
            .text_role(TextRole::Caption, window, cx)
            .child(line.shared()),
    )
    .into_any_element()
}

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
