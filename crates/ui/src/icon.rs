//! The icons of the screens (docs/design/ui.md「控件」,「未决问题」1):
//! gpui-component's Lucide set, only the sixteen needed (ten of the first
//! batch, six of the second, 「第二批控件」). `IconAssets` embeds
//! their SVGs and nothing else (ADR 0018); `app` registers it with
//! `Application::with_assets`, the one place GPUI takes an asset source.

use gpui::SharedString;
use gpui_component::IconNamed;
use gpui_kit_assets::{IconName, icon_assets};

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
    Overview,
    Worktrees,
    Files,
    MergeRequest,
    Plus,
    Sidebar,
}

impl Icon {
    /// Every icon, in declaration order.
    pub const ALL: [Icon; 16] = [
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
        Icon::Overview,
        Icon::Worktrees,
        Icon::Files,
        Icon::MergeRequest,
        Icon::Plus,
        Icon::Sidebar,
    ];

    /// The Lucide file behind each icon. `Clone` is Lucide's `copy`, `Back`
    /// and `Forward` its `arrow-left` and `arrow-right`, `Overview` its
    /// `layout-grid`, `Worktrees` its `git-branch`, `MergeRequest` its
    /// `git-pull-request` and `Sidebar` its `panel-left`; the rest share
    /// their name.
    fn lucide(self) -> IconName {
        match self {
            Icon::Folder => IconName::Folder,
            Icon::Clone => IconName::Copy,
            Icon::ChevronRight => IconName::ChevronRight,
            Icon::ChevronDown => IconName::ChevronDown,
            Icon::Check => IconName::Check,
            Icon::Close => IconName::Close,
            Icon::Search => IconName::Search,
            Icon::Inspector => IconName::Inspector,
            Icon::Back => IconName::ArrowLeft,
            Icon::Forward => IconName::ArrowRight,
            Icon::Overview => IconName::LayoutGrid,
            Icon::Worktrees => IconName::GitBranch,
            Icon::Files => IconName::File,
            Icon::MergeRequest => IconName::GitPullRequest,
            Icon::Plus => IconName::Plus,
            Icon::Sidebar => IconName::PanelLeft,
        }
    }

    /// The asset path `IconAssets` answers for this icon.
    pub fn path(self) -> SharedString {
        self.lucide().path()
    }
}

impl IconNamed for Icon {
    fn path(self) -> SharedString {
        Icon::path(self)
    }
}

// `IconAssets`: the SVG source of the sixteen icons of `Icon`, and only those;
// every other path answers `Ok(None)`. `app` passes it to
// `Application::with_assets` before `run`; without it every icon paints
// blank. The macro takes no doc comment; the re-export in `lib.rs` carries it.
icon_assets!(
    pub IconAssets,
    [
        Folder,
        Copy,
        ChevronRight,
        ChevronDown,
        Check,
        Close,
        Search,
        Inspector,
        ArrowLeft,
        ArrowRight,
        LayoutGrid,
        GitBranch,
        File,
        GitPullRequest,
        Plus,
        PanelLeft
    ]
);
