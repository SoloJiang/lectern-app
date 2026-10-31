//! What "worktrees" contributes to the window (docs/design/worktrees.md「本
//! crate 的公开项」): the `Screen(WORKTREES)`, the `InspectorTab("worktree")`
//! that only that screen shows, and the `KeyBindings`. The sidebar entry is
//! registered by the hub when a git project opens. The payloads are the types
//! of `ui::contrib`; the view factories are called by `workspace` on the main
//! thread when the screen is pushed or the page is selected.

use std::borrow::Cow;
use std::sync::Arc;

use gpui::{AppContext, Focusable};
use lectern_i18n::keys;
use lectern_kernel::Name;
use lectern_services::workspace::{
    Contribution, ContributionKind, Contributions, Registration, ScreenLayout, Visibility, screens,
};
use lectern_ui::contrib::{InspectorTab, KeyBindings, Screen};

use crate::actions;
use crate::detail::DetailTab;
use crate::hub::Hub;
use crate::screen::{self, WorktreesScreen};

/// The ID of the inspector's page.
pub(crate) const TAB_ID: &str = "worktree";

/// The screen's demands on the window (docs/design/worktrees.md「界面」): the
/// sidebar shown, a back button, the reading column, the inspector, no
/// floating bar.
pub(crate) const LAYOUT: ScreenLayout = ScreenLayout {
    sidebar: Visibility::Shown,
    back_button: true,
    reading_column: true,
    inspector: true,
    floating_bar: false,
};

/// Registers the three contributions from `activate`: no token yet, so the
/// guards go to `ctx.own_staged` and take effect at the activation's commit.
pub(crate) fn register(contributions: &dyn Contributions, hub: &Arc<Hub>) -> Vec<Registration> {
    let screen = {
        let hub = hub.clone();
        Screen {
            build: Arc::new(move |_, window, cx| {
                let hub = hub.clone();
                let view = cx.new(|cx| WorktreesScreen::new(hub, cx));
                // The screen on top takes the focus once, when it is built.
                let focus = view.focus_handle(cx);
                window.focus(&focus, cx);
                view.into()
            }),
            toolbar: Arc::new(screen::toolbar),
        }
    };
    let tab = InspectorTab {
        label: keys::worktrees::DETAIL_TITLE,
        build: Arc::new(|_, _, cx| cx.new(DetailTab::new).into()),
        screen: Some(Cow::Borrowed(screens::WORKTREES)),
    };
    let bindings = KeyBindings(actions::key_bindings());

    vec![
        contributions.register(
            Contribution {
                kind: ContributionKind::Screen(LAYOUT),
                id: Name::Borrowed(screens::WORKTREES),
                order: 0,
                payload: screen.into_payload(),
            },
            None,
        ),
        contributions.register(
            Contribution {
                kind: ContributionKind::InspectorTab,
                id: Name::Borrowed(TAB_ID),
                order: 0,
                payload: tab.into_payload(),
            },
            None,
        ),
        contributions.register(
            Contribution {
                kind: ContributionKind::KeyBindings,
                id: Name::Borrowed(screens::WORKTREES),
                order: 0,
                payload: bindings.into_payload(),
            },
            None,
        ),
    ]
}
