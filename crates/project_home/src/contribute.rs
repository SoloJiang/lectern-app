//! What "project-home" contributes to the window (docs/design/project_home.md
//! 「本 crate 的公开项」): the `Screen(PROJECT_HOME)`, the `SidebarPanel("project")`,
//! the `SidebarEntry("overview")` and the `KeyBindings`. The payloads are the
//! types of `ui::contrib`; the view factories are called by `workspace` on the
//! main thread when a screen is pushed or the panel is shown.

use std::sync::Arc;

use gpui::{AppContext, Focusable};
use lectern_i18n::keys;
use lectern_kernel::Name;
use lectern_services::workspace::{
    Contribution, ContributionKind, Contributions, Registration, ScreenLayout, SidebarSection,
    Visibility, WorkspaceCommand, screens,
};
use lectern_ui::contrib::{KeyBindings, Screen, SidebarItem, SidebarPanel, ToolbarItems};
use lectern_ui::{Icon, Text, strings};

use crate::actions;
use crate::home::Home;
use crate::present;
use crate::screen::HomeScreen;
use crate::sidebar::{HomeSidebar, OVERVIEW_ENTRY};

/// The ID of the sidebar panel.
pub(crate) const PANEL_ID: &str = "project";

/// The screen's demands on the window (project_home.md「界面」): the sidebar
/// shown, no back button, the reading column, no inspector, no floating bar.
pub(crate) const LAYOUT: ScreenLayout = ScreenLayout {
    sidebar: Visibility::Shown,
    back_button: false,
    reading_column: true,
    inspector: false,
    floating_bar: false,
};

/// Registers the four contributions from `activate`: no token yet, so the
/// guards go to `ctx.own_staged` and take effect at the activation's commit.
pub(crate) fn register(contributions: &dyn Contributions, home: &Arc<Home>) -> Vec<Registration> {
    let screen = {
        let build_home = home.clone();
        let toolbar_home = home.clone();
        Screen {
            build: Arc::new(move |_, window, cx| {
                let home = build_home.clone();
                let view = cx.new(|cx| HomeScreen::new(home, cx));
                // The screen on top takes the focus once, when it is built.
                let focus = view.focus_handle(cx);
                window.focus(&focus, cx);
                view.into()
            }),
            toolbar: Arc::new(move |cx| {
                let strings = strings(cx);
                match toolbar_home.snapshot() {
                    Some(snapshot) => ToolbarItems {
                        title: Text::verbatim(snapshot.name.clone()),
                        subtitle: present::subtitle(&strings, &snapshot),
                        controls: Vec::new(),
                    },
                    None => ToolbarItems {
                        title: Text::key(&strings, keys::project_home::OVERVIEW),
                        subtitle: None,
                        controls: Vec::new(),
                    },
                }
            }),
        }
    };
    let panel = {
        let home = home.clone();
        SidebarPanel {
            build: Arc::new(move |_, _, cx| {
                let home = home.clone();
                cx.new(|cx| HomeSidebar::new(home, cx)).into()
            }),
        }
    };
    let overview = SidebarItem {
        label: keys::project_home::OVERVIEW,
        icon: Some(Icon::Overview),
        on_select: WorkspaceCommand::PopToBase,
    };
    let bindings = KeyBindings(actions::key_bindings());

    vec![
        contributions.register(
            Contribution {
                kind: ContributionKind::Screen(LAYOUT),
                id: Name::Borrowed(screens::PROJECT_HOME),
                order: 0,
                payload: screen.into_payload(),
            },
            None,
        ),
        contributions.register(
            Contribution {
                kind: ContributionKind::SidebarPanel,
                id: Name::Borrowed(PANEL_ID),
                order: 0,
                payload: panel.into_payload(),
            },
            None,
        ),
        contributions.register(
            Contribution {
                kind: ContributionKind::SidebarEntry {
                    section: SidebarSection::Project,
                },
                id: Name::Borrowed(OVERVIEW_ENTRY),
                order: 0,
                payload: overview.into_payload(),
            },
            None,
        ),
        contributions.register(
            Contribution {
                kind: ContributionKind::KeyBindings,
                id: Name::Borrowed(screens::PROJECT_HOME),
                order: 0,
                payload: bindings.into_payload(),
            },
            None,
        ),
    ]
}
