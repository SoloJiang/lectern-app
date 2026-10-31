//! The probe's node IDs of this crate (docs/design/conversation.md「界面」,
//! UI 探针的语义节点). `lectern_ui::semantic::NodeId` writes a keyed node as
//! `view.area.item[key]`, which is why the page's root is `conversation.page`
//! and the page switch's items are `conversation.pages.item[<page>]` (the
//! design writes `conversation` and `conversation.pages.tab[<page>]`; see
//! 「与设计的差异」). Business keys are entry keys, never indices of the list.

use lectern_services::conversation::EntryKey;
use lectern_ui::semantic::NodeId;

const VIEW: &str = "conversation";

/// `conversation.<area>`.
pub(crate) fn area(name: &'static str) -> NodeId {
    NodeId::new(VIEW, name, None)
}

/// The page's root: `conversation.page`.
pub(crate) fn page() -> NodeId {
    area("page")
}

/// The page switch: `conversation.pages`.
pub(crate) fn pages() -> NodeId {
    area("pages")
}

/// One item of the page switch: `conversation.pages.item[<page>]`.
pub(crate) fn page_item(page: &'static str) -> NodeId {
    NodeId::new(VIEW, "pages", Some(page))
}

/// An entry of the list: `conversation.list.item[<entry-key>]`. The key is
/// made of ordinals (`[a-z0-9:_-]`), but a provider that breaks that rule
/// must not trip `NodeId`'s format check, so it is made safe first.
pub(crate) fn entry(key: &EntryKey) -> NodeId {
    NodeId::new(VIEW, "list", None).item(&safe(&key.0))
}

/// A business key that `NodeId::item` accepts.
pub(crate) fn safe(key: &str) -> String {
    key.replace('\n', " ").replace("].", "]_")
}
