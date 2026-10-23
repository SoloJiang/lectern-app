//! The icons of the first screens (docs/design/ui.md「控件」,「未决问题」1):
//! gpui-component's Lucide set, only the ten needed. The enum is defined in
//! step 1 for `contrib::SidebarItem`; the SVG resources (`IconAssets`) come
//! with the controls in step 3.

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Icon {
    Folder,
    Clone,
    ChevronRight,
    ChevronDown,
    Check,
    Close,
    Search,
    Inspector,
    Back,
    Forward,
}

impl Icon {
    /// Every icon, in declaration order.
    pub const ALL: [Icon; 10] = [
        Icon::Folder,
        Icon::Clone,
        Icon::ChevronRight,
        Icon::ChevronDown,
        Icon::Check,
        Icon::Close,
        Icon::Search,
        Icon::Inspector,
        Icon::Back,
        Icon::Forward,
    ];
}
