//! The payload types of the workspace's contributions (docs/design/ui.md
//! 「贡献载荷」, docs/design/workspace.md「`ui::contrib` 的载荷类型」).
//!
//! `services::workspace`'s registry is in the kernel layer and cannot hold
//! GPUI types or Fluent keys, so each contribution's concrete payload lives
//! here and travels through the type-erased `Payload`; `workspace` downcasts
//! it back. This crate only defines the types and `into_payload`: it calls
//! none of the closures, registers nothing and unregisters nothing. Fields,
//! calling thread and lifetimes are the workspace design's.
//!
//! A `Text` field cannot be given a literal:
//!
//! ```compile_fail,E0308
//! let items = lectern_ui::contrib::ToolbarItems {
//!     title: "literal",
//!     subtitle: None,
//!     controls: Vec::new(),
//! };
//! ```

use std::sync::Arc;

use gpui::{Action, AnyElement, AnyView, App, Window};
use lectern_i18n::Key;
use lectern_services::workspace::{Payload, Target, WorkspaceCommand};

use crate::icon::Icon;
use crate::text::Text;

/// A view factory: called each time the view is needed (a screen can be
/// pushed more than once, a panel opened again), so `Fn`; held on any
/// thread, called on the main thread.
pub type ViewFactory<Arg> = Arc<dyn Fn(&Arg, &mut Window, &mut App) -> AnyView + Send + Sync>;

/// A screen of the content area (`ContributionKind::Screen`).
#[derive(Clone)]
pub struct Screen {
    pub build: ViewFactory<Target>,
    /// The toolbar's title, subtitle and at most three controls on the right;
    /// `workspace` calls it on the main thread every frame for the current values.
    pub toolbar: Arc<dyn Fn(&mut App) -> ToolbarItems + Send + Sync>,
}

/// What a screen puts in the toolbar.
pub struct ToolbarItems {
    pub title: Text,
    pub subtitle: Option<Text>,
    /// At most 3.
    pub controls: Vec<AnyElement>,
}

/// A panel that replaces the sidebar's content (`ContributionKind::SidebarPanel`).
#[derive(Clone)]
pub struct SidebarPanel {
    pub build: ViewFactory<()>,
}

/// An entry of the project sidebar (`ContributionKind::SidebarEntry`'s
/// payload). The icon is this crate's `Icon`, not an SF Symbols name.
#[derive(Clone, Debug)]
pub struct SidebarItem {
    pub label: Key,
    pub icon: Option<Icon>,
    pub on_select: WorkspaceCommand,
}

/// A page of the inspector (`ContributionKind::InspectorTab`).
#[derive(Clone)]
pub struct InspectorTab {
    pub label: Key,
    pub build: ViewFactory<()>,
}

/// A floating panel above the inspector (`ContributionKind::Panel`).
#[derive(Clone)]
pub struct Panel {
    pub build: ViewFactory<()>,
}

/// A layer over the window's centre (`ContributionKind::Overlay`).
#[derive(Clone)]
pub struct Overlay {
    pub build: ViewFactory<()>,
}

/// Key bindings: each is the keystrokes, the action and an optional
/// `KeyContext` predicate (the context names of workspace's「Action 与快捷键」).
pub struct KeyBindings(pub Vec<(&'static str, Box<dyn Action>, Option<&'static str>)>);

macro_rules! into_payload {
    ($($ty:ident),* $(,)?) => {$(
        impl $ty {
            /// Into `services::workspace::Payload`; `workspace` takes it
            /// back with `Payload::downcast`.
            pub fn into_payload(self) -> Payload {
                Payload(Box::new(self))
            }
        }
    )*};
}

into_payload!(
    Screen,
    SidebarPanel,
    SidebarItem,
    InspectorTab,
    Panel,
    Overlay,
    KeyBindings
);
