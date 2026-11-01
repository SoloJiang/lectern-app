//! The semantic trees in the reference states (docs/design/project_home.md
//! 「测试」「语义树」), as insta snapshots. A tree is the compact text of
//! `stage::tree_of`: IDs, roles, labels (message keys), state flags and
//! attributes of one frame, without bounds and call sites.

use gpui::TestAppContext;
use lectern_i18n::keys;
use lectern_services::workspace::{SidebarSection, Target, WorkspaceCommand, screens};
use lectern_ui::Icon;
use lectern_ui::contrib::SidebarItem;

use crate::testing::ProjectFx;

use super::stage::*;

#[gpui::test]
fn home_tree_with_cards(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let _world = standard(&rig);
    let (_host, vcx) = window(cx, &rig);
    // The second card is picked: the selected row carries the flag.
    vcx.simulate_keystrokes("down down");
    frame(vcx);
    insta::assert_snapshot!("home_tree_with_cards", tree(vcx, main_part));
}

#[gpui::test]
fn home_tree_empty(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let fx = ProjectFx::git("empty");
    let key = fx.key();
    rig.projects.set(fx.snapshot());
    rig.activate_home();
    rig.opened(&key);
    rig.run();
    let (_host, vcx) = window(cx, &rig);
    insta::assert_snapshot!("home_tree_empty", tree(vcx, |_| true));
}

#[gpui::test]
fn sidebar_tree_with_views_and_usage(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let _world = standard(&rig);
    with_usage(&rig);
    rig.contributions.add_entry(
        SidebarSection::Views,
        "worktrees",
        0,
        SidebarItem {
            label: keys::workspace::BACK,
            icon: Some(Icon::Folder),
            on_select: WorkspaceCommand::Push(lectern_services::workspace::Route {
                screen: screens::WORKTREES.into(),
                target: Target::None,
            }),
        },
    );
    rig.contributions.add_entry(
        SidebarSection::Views,
        "files",
        1,
        SidebarItem {
            label: keys::workspace::INSPECTOR_TOGGLE,
            icon: None,
            on_select: WorkspaceCommand::ShowSidebarPanel(Some("files".into())),
        },
    );
    rig.workspace_event(lectern_services::workspace::WorkspaceEvent::ContributionsChanged);
    let (_host, vcx) = window(cx, &rig);
    insta::assert_snapshot!("sidebar_tree_with_views_and_usage", tree(vcx, sidebar_part));
}

#[gpui::test]
fn trees_with_waiting_sessions(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard(&rig);
    rig.activate_waiting();
    let (_host, vcx) = window(cx, &rig);
    // The running session and the Codex one wait for the user.
    rig.set_waiting(vec![world.running.clone(), world.codex.clone()]);
    frame(vcx);
    insta::assert_snapshot!(
        "sidebar_tree_with_waiting_sessions",
        tree(vcx, |id| sidebar_part(id) && id.contains(".sessions"))
    );
    insta::assert_snapshot!(
        "home_tree_with_waiting_cards",
        tree(vcx, |id| main_part(id) && id.contains("needs-review.item"))
    );
}
