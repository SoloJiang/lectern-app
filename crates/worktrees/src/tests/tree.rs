//! The semantic trees (docs/design/worktrees.md「测试」「语义树」), as insta
//! snapshots: the screen with its two groups, the folder project, and the
//! inspector's page with overlaps and usage. The text has no bounds and no
//! call sites (`stage::tree_of`), which a layout change would move.

use gpui::TestAppContext;
use lectern_services::usage::UsageState;

use crate::testing::{
    ProjectFx, branch_usage, mr_label, mr_ref, mr_snapshot, tokens, worktree_query,
    worktree_snapshot,
};

use super::stage::*;

#[gpui::test]
fn worktrees_tree_two_groups(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let _world = standard(&rig);
    let (_host, vcx) = window(cx, &rig);
    insta::assert_snapshot!(tree(vcx, |id| id.starts_with("worktrees.")
        && !id.starts_with("worktrees.detail")));
}

#[gpui::test]
fn worktrees_tree_folder(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let fx = ProjectFx::folder("notes");
    let key = fx.key();
    rig.projects.set(fx.snapshot());
    rig.activate_main();
    rig.opened(&key);
    rig.show();
    rig.run();
    let (_host, vcx) = window(cx, &rig);
    insta::assert_snapshot!(tree(vcx, |id| id.starts_with("worktrees.")));
}

#[gpui::test]
fn detail_tree_with_overlaps_and_usage(cx: &mut TestAppContext) {
    // Reduced motion, so that the open item is open in the first frame.
    let rig = rig_with(cx, true);
    let world = standard(&rig);
    // `feat` has two merge requests, which share the branch.
    rig.usage.set_snapshot(
        worktree_query(&world.key, &world.feat),
        worktree_snapshot(branch_usage(
            UsageState::Backfilling { done: 1, total: 3 },
            Some(tokens(900, 100)),
            vec![mr_label(21, "First change"), mr_label(22, "Second change")],
        )),
    );
    rig.usage.set_snapshot(
        lectern_services::usage::UsageQuery::Mr { mr: mr_ref(21) },
        mr_snapshot(21, UsageState::Ready, tokens(600, 60), vec![22]),
    );
    rig.usage_changed(worktree_query(&world.key, &world.feat));
    rig.run();
    rig.run();
    let (_host, vcx) = window(cx, &rig);
    vcx.simulate_keystrokes("down");
    let item = format!("worktrees.detail.overlaps.item[{}]", world.main.0.display());
    click(vcx, &item);
    frame(vcx);
    insta::assert_snapshot!(tree(vcx, detail_part));
}
