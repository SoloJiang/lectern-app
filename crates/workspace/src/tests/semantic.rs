//! The regions' semantic tree in the three reference states
//! (docs/design/workspace.md「测试」「语义树」), as insta snapshots.

use gpui::TestAppContext;
use lectern_services::workspace::{WorkspaceCommand, screens};

use super::support::Contributor;
use super::ui_support::*;

#[gpui::test]
fn workspace_tree_welcome(cx: &mut TestAppContext) {
    let rig = UiRig::new(cx);
    let _plugins = rig.contribute(Contributor::staged(standard_screens()), cx);
    insta::assert_snapshot!("workspace_tree_welcome", rig.tree(cx));
}

#[gpui::test]
fn workspace_tree_project_home(cx: &mut TestAppContext) {
    let rig = UiRig::new(cx);
    let _plugins = rig.contribute(Contributor::staged(standard_screens()), cx);
    rig.open(project_path(), &project(), cx);
    insta::assert_snapshot!("workspace_tree_project_home", rig.tree(cx));
}

#[gpui::test]
fn workspace_tree_review_with_inspector(cx: &mut TestAppContext) {
    let rig = UiRig::new(cx);
    let _plugins = rig.contribute(Contributor::staged(standard_screens()), cx);
    rig.open(project_path(), &project(), cx);
    rig.push(screens::REVIEW, cx);
    rig.dispatch(WorkspaceCommand::SetInspector(true), cx);
    insta::assert_snapshot!("workspace_tree_review_with_inspector", rig.tree(cx));
}
