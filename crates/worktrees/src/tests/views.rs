//! The views' behaviour (docs/design/worktrees.md「测试」「`#[gpui::test]`」):
//! what the screen and the inspector's page show for a snapshot, what the keys
//! and clicks send, and how they follow the services. No real window opens:
//! the windows are GPUI's test windows.

use std::time::Duration;

use gpui::{TestAppContext, VisualTestContext};
use lectern_i18n::Locale;
use lectern_services::changes::BaseSpec;
use lectern_services::project::{ChangedParts, WorktreeKey};
use lectern_services::review::{ChangeCount, ReviewTarget};
use lectern_services::usage::UsageState;
use lectern_services::workspace::{Milestone, Route, Target, WorkspaceCommand, screens};
use lectern_ui::contrib::Screen;
use lectern_ui::motion::{LOADING_PROGRESS_AT, LOADING_SKELETON_AT};

use crate::actions::{OpenSession, OpenWorktree, Refresh, ReviewChanges, SwitchTo, ToggleOverlap};
use crate::testing::{
    MINUTE, ProjectFx, branch, branch_usage, changes_of, counted, mr_label, mr_snapshot,
    overview_of, sess, tokens, worktree_query, worktree_snapshot,
};

use super::stage::*;

/// The IDs of the nodes drawn selected, outside the inspector.
fn selected_nodes(tree: &str) -> Vec<&str> {
    tree.lines()
        .filter(|line| line.contains(" selected"))
        .filter_map(|line| line.split_whitespace().next())
        .collect()
}

/// The position of each needle in the text, in order; panics when one is
/// missing or out of order.
fn in_order(text: &str, needles: &[&str]) {
    let mut from = 0;
    for needle in needles {
        match text[from..].find(needle) {
            Some(at) => from += at + needle.len(),
            None => panic!("`{needle}` is missing or out of order in:\n{text}"),
        }
    }
}

fn push_review(target: ReviewTarget) -> WorkspaceCommand {
    WorkspaceCommand::Push(Route {
        screen: screens::REVIEW.into(),
        target: Target::Review(target),
    })
}

fn range(world: &World, worktree: &WorktreeKey, base: BaseSpec) -> WorkspaceCommand {
    push_review(ReviewTarget::Range {
        project: world.key.clone(),
        worktree: worktree.clone(),
        base,
    })
}

fn merge_base() -> BaseSpec {
    BaseSpec::MergeBase(BASE.into())
}

fn active_parts() -> ChangedParts {
    ChangedParts {
        active: true,
        ..ChangedParts::default()
    }
}

#[gpui::test]
fn list_groups_active_and_idle(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard(&rig);
    let (_host, vcx) = window(cx, &rig);
    let tree = tree(vcx, main_part);
    in_order(
        &tree,
        &[
            "worktrees.list List rows=4 active=1 idle=3 ready=true phase=nothing",
            "worktrees.active List label=[worktrees-active]",
            &format!("{} ListItem selected", row_id(&world.main)),
            "worktrees.idle List label=[worktrees-idle]",
            &row_id(&world.feat),
            &row_id(&world.fix),
            &row_id(&world.old),
        ],
    );
    // The current worktree is selected when the screen opens.
    assert_eq!(selected_nodes(&tree), [row_id(&world.main)]);
    // What the probe reads off the rows.
    assert!(
        tree.contains(&format!(
            "{}.facts Group label=\"main\" current=true main=true locked=false prunable=false \
             sessions=1 running=1 uncommitted=2 vs-base=5",
            row_id(&world.main)
        )),
        "{tree}"
    );
    assert!(
        tree.contains(&format!(
            "{}.facts Group label=\"feat/search\" current=false main=false locked=false \
             prunable=false sessions=2 running=0 uncommitted=1 vs-base=5",
            row_id(&world.feat)
        )),
        "{tree}"
    );
    assert!(
        tree.contains(&format!("{}.tokens Text tokens=12345", row_id(&world.feat))),
        "{tree}"
    );
    assert!(
        !tree.contains(&format!("{}.tokens", row_id(&world.main))),
        "the default branch is not counted: {tree}"
    );
    assert!(
        tree.contains(&format!(
            "{}.facts Group label=\"01234567\" current=false main=false locked=true \
             prunable=false sessions=0 running=0 uncommitted=0 vs-base=0",
            row_id(&world.fix)
        )),
        "{tree}"
    );
    assert!(
        tree.contains(&format!(
            "{}.facts Group label=\"old/idea\" current=false main=false locked=false \
             prunable=true sessions=0 running=0 uncommitted=unavailable vs-base=unavailable",
            row_id(&world.old)
        )),
        "{tree}"
    );
    // The marks.
    for (row, which, label) in [
        (&world.main, "current", "worktrees-current"),
        (&world.fix, "locked", "worktrees-locked"),
        (&world.old, "missing", "worktrees-missing"),
    ] {
        assert!(
            tree.contains(&format!(
                "{}.badge.item[{which}] Badge label=[{label}]",
                row_id(row)
            )),
            "{tree}"
        );
    }
    assert!(!tree.contains("worktrees.only-one"), "four worktrees");
    // The toolbar: the title, the counts, one control.
    let screen: Screen = rig.contributions.payload("worktrees").unwrap();
    let items = vcx.update(|_, cx| (screen.toolbar)(cx));
    assert_eq!(items.title.message_id(), Some("worktrees-title"));
    assert_eq!(
        items.subtitle.expect("a subtitle").as_str(),
        "1 active \u{00B7} 4 worktrees"
    );
    assert_eq!(items.controls.len(), 1);
}

#[gpui::test]
fn the_page_and_the_text_are_in_the_language_of_the_interface(cx: &mut TestAppContext) {
    let rig = rig_in(cx, Locale::ZhCn);
    let _world = standard(&rig);
    let (_host, vcx) = window(cx, &rig);
    let screen: Screen = rig.contributions.payload("worktrees").unwrap();
    let items = vcx.update(|_, cx| (screen.toolbar)(cx));
    assert_eq!(items.subtitle.unwrap().as_str(), "1 个活动 · 共 4 个");
    let tree = tree(vcx, main_part);
    assert!(tree.contains("worktrees.active List label=[worktrees-active]"));
}

#[gpui::test]
fn arrow_keys_cross_groups(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard(&rig);
    let (host, vcx) = window(cx, &rig);
    let model = model_of(&host, vcx);
    assert_eq!(selected(&model, vcx), Some(world.main.clone()));
    // Active → Idle, one row at a time, in the list's order.
    for expected in [&world.feat, &world.fix, &world.old, &world.old] {
        vcx.simulate_keystrokes("down");
        assert_eq!(selected(&model, vcx).as_ref(), Some(expected));
    }
    vcx.simulate_keystrokes("up up");
    assert_eq!(selected(&model, vcx), Some(world.feat.clone()));
    vcx.simulate_keystrokes("up up up");
    assert_eq!(
        selected(&model, vcx),
        Some(world.main.clone()),
        "stops at the start"
    );
    vcx.simulate_keystrokes("alt-down");
    assert_eq!(selected(&model, vcx), Some(world.old.clone()));
    vcx.simulate_keystrokes("alt-up");
    assert_eq!(selected(&model, vcx), Some(world.main.clone()));
    let tree = super::stage::tree(vcx, main_part);
    assert_eq!(selected_nodes(&tree), [row_id(&world.main)]);
    assert!(
        rig.contributions.commands().is_empty(),
        "moving the selection opens nothing"
    );
}

#[gpui::test]
fn click_and_enter_push_the_range_review(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard(&rig);
    let (host, vcx) = window(cx, &rig);
    let model = model_of(&host, vcx);

    vcx.simulate_keystrokes("enter");
    assert_eq!(
        rig.contributions.commands(),
        [range(&world, &world.main, merge_base())],
        "↵ reviews the selected worktree against the base's full name"
    );

    click(vcx, &row_id(&world.feat));
    assert_eq!(selected(&model, vcx), Some(world.feat.clone()));
    assert_eq!(
        rig.contributions.commands().last(),
        Some(&range(&world, &world.feat, merge_base())),
        "a click selects and opens"
    );

    // A prunable worktree is only selected.
    let before = rig.contributions.commands().len();
    click(vcx, &row_id(&world.old));
    assert_eq!(selected(&model, vcx), Some(world.old.clone()));
    vcx.simulate_keystrokes("enter");
    assert_eq!(
        rig.contributions.commands().len(),
        before,
        "nothing for a missing worktree"
    );
    // The probe's actions land on the screen as well.
    vcx.update(|window, cx| window.dispatch_action(Box::new(ReviewChanges), cx));
    assert_eq!(rig.contributions.commands().len(), before);
    vcx.update(|window, cx| {
        window.dispatch_action(
            Box::new(OpenWorktree {
                key: world.fix.clone(),
            }),
            cx,
        );
    });
    assert_eq!(selected(&model, vcx), Some(world.fix.clone()));
    assert_eq!(
        rig.contributions.commands().last(),
        Some(&range(&world, &world.fix, merge_base()))
    );
}

#[gpui::test]
fn without_a_base_the_range_is_against_head(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard_with(&rig, None);
    let (_host, vcx) = window(cx, &rig);
    vcx.simulate_keystrokes("enter");
    assert_eq!(
        rig.contributions.commands(),
        [range(&world, &world.main, BaseSpec::Head)]
    );
    // No count against a base, and the page compares the uncommitted ones.
    let tree = tree(vcx, |_| true);
    assert!(!tree.contains("vs-base="), "{tree}");
    assert!(
        tree.contains("worktrees.detail.compare Group uncommitted=2\n"),
        "{tree}"
    );
}

#[gpui::test]
fn detail_follows_the_selection(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard(&rig);
    let (_host, vcx) = window(cx, &rig);
    let tree_main = tree(vcx, detail_part);
    in_order(
        &tree_main,
        &[
            "worktrees.detail Panel selected=true",
            "worktrees.detail.title Group label=\"main\"",
            "worktrees.detail.usage Group label=[worktrees-usage-not-counted] kind=not-counted",
            "worktrees.detail.compare Group uncommitted=2 committed=3 total=5",
            "worktrees.detail.compare.open",
            "worktrees.detail.overlaps List count=1",
            &format!("worktrees.detail.overlaps.item[{}]", world.feat.0.display()),
            "worktrees.detail.sessions List count=1",
        ],
    );
    assert!(
        !tree_main.contains("worktrees.detail.switch"),
        "the current worktree has no switch button: {tree_main}"
    );

    vcx.simulate_keystrokes("down");
    let tree_feat = tree(vcx, detail_part);
    in_order(
        &tree_feat,
        &[
            "worktrees.detail.title Group label=\"feat/search\"",
            "worktrees.detail.usage Group kind=branch tokens=12345",
            "worktrees.detail.compare Group uncommitted=1 committed=4 total=5",
            "worktrees.detail.overlaps List count=1",
            "worktrees.detail.sessions List count=2",
            "worktrees.detail.switch",
        ],
    );
    assert!(tree_feat.contains(
        "worktrees.detail.sessions.item[s2].facts Group label=\"Add the search index\" agent=codex running=false"
    ));
    assert!(tree_feat.contains(
        "worktrees.detail.sessions.item[s3].facts Group label=[worktrees-untitled-session] agent=claude-code running=false"
    ));

    // A missing worktree: the comparison is replaced, nothing can be done.
    vcx.simulate_keystrokes("down down");
    let tree_old = tree(vcx, detail_part);
    assert!(
        tree_old.contains("worktrees.detail.compare Group label=[worktrees-missing-detail]"),
        "{tree_old}"
    );
    assert!(
        !tree_old.contains("worktrees.detail.overlaps"),
        "{tree_old}"
    );
    assert!(!tree_old.contains("worktrees.detail.switch"), "{tree_old}");
    assert!(!tree_old.contains("compare.open"), "{tree_old}");
}

#[gpui::test]
fn detail_session_pushes_session_review(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard(&rig);
    let (_host, vcx) = window(cx, &rig);
    click(vcx, "worktrees.detail.sessions.item[s1]");
    assert_eq!(
        rig.contributions.commands(),
        [push_review(ReviewTarget::Session(world.running.clone()))]
    );
    // The probe's action reaches the same place.
    vcx.update(|window, cx| {
        window.dispatch_action(
            Box::new(OpenSession {
                key: world.codex.clone(),
            }),
            cx,
        );
    });
    assert_eq!(
        rig.contributions.commands().last(),
        Some(&push_review(ReviewTarget::Session(world.codex.clone())))
    );
    // The inspector's "Review changes" is the row's command.
    click(vcx, "worktrees.detail.compare.open");
    assert_eq!(
        rig.contributions.commands().last(),
        Some(&range(&world, &world.main, merge_base()))
    );
}

#[gpui::test]
fn switch_dispatches_set_active_worktree(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard(&rig);
    let (_host, vcx) = window(cx, &rig);
    // The current and the missing worktree ignore the action.
    vcx.update(|window, cx| window.dispatch_action(Box::new(SwitchTo), cx));
    assert!(rig.contributions.commands().is_empty());
    vcx.simulate_keystrokes("alt-down");
    vcx.update(|window, cx| window.dispatch_action(Box::new(SwitchTo), cx));
    assert!(rig.contributions.commands().is_empty());

    vcx.simulate_keystrokes("alt-up down");
    click(vcx, "worktrees.detail.switch");
    assert_eq!(
        rig.contributions.commands(),
        [WorkspaceCommand::SetActiveWorktree(world.feat.clone())],
        "the screen stays: no navigation"
    );
}

#[gpui::test]
fn refresh_calls_review(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard(&rig);
    let (_host, vcx) = window(cx, &rig);
    vcx.simulate_keystrokes("cmd-r");
    assert_eq!(rig.review.refreshed(), std::slice::from_ref(&world.key));
    click(vcx, "worktrees.toolbar.refresh");
    assert_eq!(
        rig.review.refreshed(),
        [world.key.clone(), world.key.clone()]
    );
    vcx.update(|window, cx| window.dispatch_action(Box::new(Refresh), cx));
    assert_eq!(rig.review.refreshed().len(), 3);
    assert!(
        rig.contributions.commands().is_empty(),
        "refreshing does not go through the workspace"
    );
}

/// Switches the project's current worktree to `feat`, the way `project` does:
/// first with the file list still being scanned, then ready.
fn switch_to_feat(rig: &crate::testing::Rig, vcx: &mut VisualTestContext, ready: bool) {
    let mut fx = project_fx().active_in("feat").generation(2);
    if !ready {
        fx = fx.files_pending();
    }
    rig.projects.set(fx.snapshot());
    rig.changed_parts(&fx.key(), active_parts());
    rig.run();
    frame(vcx);
}

#[gpui::test]
fn milestone_after_a_switch_on_the_settled_frame(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard(&rig);
    let (_host, vcx) = window(cx, &rig);
    assert!(
        rig.contributions.milestones().is_empty(),
        "the first frame is not a switch"
    );

    // `Pending` generation: the current mark has moved, the files are not
    // scanned yet.
    switch_to_feat(&rig, vcx, false);
    let snapshot = rig.snapshot().unwrap();
    assert_eq!(snapshot.active.as_ref(), Some(&world.feat));
    assert!(!snapshot.files_settled);
    assert!(
        rig.contributions.milestones().is_empty(),
        "not on a pending frame"
    );
    let tree_pending = tree(vcx, main_part);
    assert!(
        tree_pending.contains(&format!(
            "{}.facts Group label=\"feat/search\" current=true",
            row_id(&world.feat)
        )),
        "the mark moved at once: {tree_pending}"
    );

    // `Ready` generation: reported once, on the frame that shows it.
    switch_to_feat(&rig, vcx, true);
    assert_eq!(
        rig.contributions.milestones(),
        [Milestone::WorktreeSwitched]
    );
    frame(vcx);
    frame(vcx);
    assert_eq!(
        rig.contributions.milestones().len(),
        1,
        "later frames of the same snapshot report nothing"
    );

    // A second switch reports again.
    let fx = project_fx().generation(4);
    rig.projects.set(fx.snapshot());
    rig.changed_parts(&fx.key(), active_parts());
    rig.run();
    frame(vcx);
    assert_eq!(rig.contributions.milestones().len(), 2);
}

#[gpui::test]
fn no_milestone_without_a_switch(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard(&rig);
    let (_host, vcx) = window(cx, &rig);
    // Everything else that recomposes the snapshot is not a switch.
    rig.sessions_changed();
    rig.worktrees_changed(&world.key);
    rig.changed(&world.key);
    rig.run();
    frame(vcx);
    rig.show();
    rig.run();
    frame(vcx);
    assert!(rig.contributions.milestones().is_empty());
}

#[gpui::test]
fn a_switch_before_the_screen_is_built_is_not_reported(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let _world = standard(&rig);
    // The worktree changed while the screen was not the current one: the
    // model that is built next takes that count as already seen.
    rig.hide();
    let fx = project_fx().active_in("feat").generation(2);
    rig.projects.set(fx.snapshot());
    rig.changed_parts(&fx.key(), active_parts());
    assert_eq!(rig.hub().switches(), 1, "counted while hidden");
    rig.show();
    rig.run();
    let (_host, _vcx) = window(cx, &rig);
    assert!(rig.contributions.milestones().is_empty());
}

/// The measurement build pushes the screen and switches the worktree at
/// once, and `workspace` builds the view in the frame after the push: the
/// switch can arrive before the model exists. It counts, because the model
/// takes the value from when the screen became the current one.
#[gpui::test]
fn a_switch_between_showing_and_building_is_reported(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard(&rig);
    rig.hide();
    rig.show();
    let fx = project_fx().active_in("feat").generation(2);
    rig.projects.set(fx.snapshot());
    rig.changed_parts(&fx.key(), active_parts());
    assert_eq!(rig.hub().switches(), 1);
    assert_eq!(rig.hub().switches_when_shown(), 0);
    rig.run();
    let (_host, vcx) = window(cx, &rig);
    frame(vcx);
    assert_eq!(rig.snapshot().unwrap().active.as_ref(), Some(&world.feat));
    assert_eq!(
        rig.contributions.milestones(),
        [Milestone::WorktreeSwitched]
    );
}

#[gpui::test]
fn timer_moves_an_expired_session_to_idle(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard(&rig);
    let (_host, vcx) = window(cx, &rig);
    let tree = tree(vcx, main_part);
    assert!(tree.contains("worktrees.list List rows=4 active=1 idle=3"));
    // The running session is 2 minutes old: it runs out 8 minutes from now.
    rig.advance(8 * MINUTE + 2);
    vcx.executor()
        .advance_clock(Duration::from_secs(8 * 60 + 1));
    vcx.run_until_parked();
    assert!(rig.run() > 0, "the timer asked for a composition");
    frame(vcx);
    let tree = super::stage::tree(vcx, main_part);
    in_order(
        &tree,
        &[
            "worktrees.list List rows=4 active=0 idle=4",
            "worktrees.idle List",
            &row_id(&world.main),
        ],
    );
    assert!(
        !tree.contains("worktrees.active"),
        "an empty group is not shown: {tree}"
    );
}

#[gpui::test]
fn the_dot_blinks_when_a_worktree_starts_running(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard(&rig);
    let (host, vcx) = window(cx, &rig);
    let model = model_of(&host, vcx);
    let rounds = |vcx: &mut VisualTestContext| {
        vcx.update(|_, cx| {
            let model = model.read(cx);
            (
                model.blink_round(&world.main),
                model.blink_round(&world.feat),
            )
        })
    };
    assert_eq!(
        rounds(vcx),
        (0, 0),
        "what runs when the model is built does not blink"
    );
    // An agent starts working in `feat`.
    let running = sess("fake", "s4", &world.key, Some(&world.feat))
        .title("New work")
        .open_turn()
        .ago(MINUTE);
    let mut all = rig.sessions.current();
    all.push(running.build());
    rig.sessions.set(all);
    rig.sessions_changed();
    rig.run();
    frame(vcx);
    assert_eq!(rounds(vcx), (0, 1));
    // Another snapshot while it still runs does not blink again.
    rig.sessions_changed();
    rig.run();
    frame(vcx);
    assert_eq!(rounds(vcx), (0, 1));
}

#[gpui::test]
fn selection_survives_rebuilds_and_removal(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard(&rig);
    let (host, vcx) = window(cx, &rig);
    let model = model_of(&host, vcx);
    vcx.simulate_keystrokes("down down");
    assert_eq!(selected(&model, vcx), Some(world.fix.clone()));

    // A rebuild that moves rows between groups keeps the selection by key:
    // `fix` gets a running session and joins Active.
    let mut all = rig.sessions.current();
    all.push(
        sess("fake", "s5", &world.key, Some(&world.fix))
            .open_turn()
            .ago(MINUTE)
            .build(),
    );
    rig.sessions.set(all);
    rig.sessions_changed();
    rig.run();
    frame(vcx);
    assert_eq!(selected(&model, vcx), Some(world.fix.clone()));
    let tree = tree(vcx, main_part);
    assert_eq!(selected_nodes(&tree), [row_id(&world.fix)]);
    assert!(tree.contains("active=2 idle=2"), "{tree}");

    // The selected worktree is deleted: the row at the same position is
    // selected (Active: main, fix; Idle: feat, old → the second row is `old`'s
    // neighbour after the removal).
    let fx = ProjectFx::git("app")
        .linked("feat", branch("feat/search"))
        .prunable("old", branch("old/idea"))
        .generation(2);
    rig.projects.set(fx.snapshot());
    rig.changed(&fx.key());
    rig.run();
    frame(vcx);
    let after = selected(&model, vcx).expect("a selection");
    assert_eq!(
        after, world.feat,
        "the position (2nd row of the old order) holds `feat` now"
    );

    // The last row goes: the new last row is selected.
    vcx.simulate_keystrokes("alt-down");
    assert_eq!(selected(&model, vcx), Some(world.old.clone()));
    let fx = ProjectFx::git("app")
        .linked("feat", branch("feat/search"))
        .generation(3);
    rig.projects.set(fx.snapshot());
    rig.changed(&fx.key());
    rig.run();
    frame(vcx);
    assert_eq!(selected(&model, vcx), Some(world.feat.clone()));
}

#[gpui::test]
fn overlap_expands(cx: &mut TestAppContext) {
    // Reduced motion: the height arrives at once.
    let rig = rig_with(cx, true);
    let world = standard(&rig);
    let (host, vcx) = window(cx, &rig);
    let model = model_of(&host, vcx);
    let item = format!("worktrees.detail.overlaps.item[{}]", world.feat.0.display());
    let tree_closed = tree(vcx, detail_part);
    assert!(
        tree_closed.contains(&format!(
            "{item}.facts Group label=\"feat/search\" files=12 expanded=false"
        )),
        "{tree_closed}"
    );
    assert!(
        tree_closed.contains(&format!("{item}.paths Group expanded=false")),
        "{tree_closed}"
    );

    click(vcx, &item);
    frame(vcx);
    let opened = super::stage::tree(vcx, detail_part);
    assert!(opened.contains("files=12 expanded=true"), "{opened}");
    assert!(
        opened.contains(&format!("{item}.paths Group expanded=true")),
        "{opened}"
    );
    assert_eq!(vcx.update(|_, cx| model.read(cx).expanded.len()), 1);

    // The action of the probe closes it again.
    vcx.update(|window, cx| {
        window.dispatch_action(
            Box::new(ToggleOverlap {
                key: world.feat.clone(),
            }),
            cx,
        );
    });
    frame(vcx);
    let closed = super::stage::tree(vcx, detail_part);
    assert!(closed.contains("files=12 expanded=false"), "{closed}");

    // Moving the selection to another worktree closes what was open.
    click(vcx, &item);
    vcx.simulate_keystrokes("down");
    assert_eq!(vcx.update(|_, cx| model.read(cx).expanded.len()), 0);
}

#[gpui::test]
fn skeleton_appears_after_100ms_only(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard(&rig);
    // The counts of `feat` have not arrived.
    rig.review.set_overview(Some(overview_of(
        &world.key,
        Some(BASE),
        vec![
            counted(&world.main, 2, 3, 5, vec![]),
            changes_of(
                &world.feat,
                ChangeCount::Pending,
                ChangeCount::Pending,
                ChangeCount::Pending,
                None,
            ),
        ],
    )));
    rig.worktrees_changed(&world.key);
    rig.run();
    let (_host, vcx) = window(cx, &rig);
    let early = tree(vcx, main_part);
    assert!(
        early.contains("ready=false phase=nothing"),
        "nothing before the wait is noticed: {early}"
    );
    assert!(
        early.contains(&format!("{}.facts Group label=\"feat/search\" current=false main=false locked=false prunable=false sessions=2 running=0 uncommitted=pending vs-base=pending", row_id(&world.feat))),
        "{early}"
    );
    vcx.executor().advance_clock(LOADING_SKELETON_AT);
    frame(vcx);
    assert!(tree(vcx, main_part).contains("ready=false phase=skeleton"));
    vcx.executor()
        .advance_clock(LOADING_PROGRESS_AT - LOADING_SKELETON_AT);
    frame(vcx);
    assert!(tree(vcx, main_part).contains("ready=false phase=progress"));

    // The counts arrive: the wait is over.
    rig.review.set_overview(Some(overview_of(
        &world.key,
        Some(BASE),
        vec![
            counted(&world.main, 2, 3, 5, vec![]),
            counted(&world.feat, 1, 4, 5, vec![]),
            counted(&world.fix, 0, 0, 0, vec![]),
        ],
    )));
    rig.worktrees_changed(&world.key);
    rig.run();
    frame(vcx);
    assert!(tree(vcx, main_part).contains("ready=true phase=nothing"));
}

#[gpui::test]
fn the_page_shows_usage_of_merge_requests(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard(&rig);
    // `feat` has two merge requests on its branch.
    rig.usage.set_snapshot(
        worktree_query(&world.key, &world.feat),
        worktree_snapshot(branch_usage(
            UsageState::Ready,
            Some(tokens(900, 100)),
            vec![mr_label(21, "First change"), mr_label(22, "Second change")],
        )),
    );
    rig.usage.set_snapshot(
        lectern_services::usage::UsageQuery::Mr {
            mr: crate::testing::mr_ref(21),
        },
        mr_snapshot(21, UsageState::Ready, tokens(600, 60), vec![22]),
    );
    rig.usage_changed(worktree_query(&world.key, &world.feat));
    rig.run();
    rig.run();
    let (_host, vcx) = window(cx, &rig);
    vcx.simulate_keystrokes("down");
    let tree = tree(vcx, |_| true);
    assert!(
        tree.contains(&format!("{}.tokens Text tokens=660", row_id(&world.feat))),
        "the row shows the first merge request's tokens: {tree}"
    );
    in_order(
        &tree,
        &[
            "worktrees.detail.usage Group kind=mr",
            "worktrees.detail.usage.mr.item[21] ListItem label=\"First change\" shared=1 tokens=660",
            "worktrees.detail.usage.mr.item[22] ListItem label=\"Second change\" shared=0",
        ],
    );
}

#[gpui::test]
fn a_screen_built_before_the_snapshot_follows_it(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let fx = project_fx();
    let key = fx.key();
    rig.projects.set(fx.snapshot());
    rig.review.set_overview(None);
    rig.activate_main();
    rig.opened(&key);
    // The screen is pushed (and built) before the plugin hears about it.
    let (host, vcx) = window(cx, &rig);
    let model = model_of(&host, vcx);
    assert!(vcx.update(|_, cx| model.read(cx).snapshot.is_none()));
    let before = tree(vcx, |_| true);
    assert!(
        !before.contains("worktrees.list"),
        "nothing to show yet: {before}"
    );
    assert!(before.contains("worktrees.detail.empty"), "{before}");

    rig.show();
    rig.run();
    frame(vcx);
    let tree = tree(vcx, main_part);
    assert!(tree.contains("worktrees.list List rows=4"), "{tree}");
    assert_eq!(
        selected(&model, vcx),
        Some(fx.wt("main")),
        "the current worktree is selected when the first snapshot arrives"
    );
}

#[gpui::test]
fn a_repository_with_one_worktree_says_so(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let fx = ProjectFx::git("solo");
    let key = fx.key();
    rig.projects.set(fx.snapshot());
    rig.activate_main();
    rig.opened(&key);
    rig.show();
    rig.run();
    let (_host, vcx) = window(cx, &rig);
    let tree = tree(vcx, main_part);
    in_order(
        &tree,
        &[
            "worktrees.list List rows=1",
            "worktrees.only-one Text label=[worktrees-only-one]",
        ],
    );
}

#[gpui::test]
fn a_folder_project_says_it_is_not_a_repository(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let fx = ProjectFx::folder("notes");
    let key = fx.key();
    rig.projects.set(fx.snapshot());
    rig.activate_main();
    rig.opened(&key);
    rig.show();
    rig.run();
    let (_host, vcx) = window(cx, &rig);
    let tree = tree(vcx, |_| true);
    assert!(
        tree.contains("worktrees.empty Text label=[worktrees-not-git]"),
        "{tree}"
    );
    assert!(!tree.contains("worktrees.list"), "{tree}");
    let screen: Screen = rig.contributions.payload("worktrees").unwrap();
    let items = vcx.update(|_, cx| (screen.toolbar)(cx));
    assert!(items.subtitle.is_none(), "no counts for a folder");
}

#[gpui::test]
fn closed_project_clears_the_views(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard(&rig);
    let (host, vcx) = window(cx, &rig);
    let model = model_of(&host, vcx);
    assert!(tree(vcx, |_| true).contains("worktrees.list List rows=4"));
    rig.projects.remove(&world.key);
    rig.closed(&world.key);
    rig.run();
    frame(vcx);
    let tree = tree(vcx, |_| true);
    assert!(!tree.contains("worktrees.list"), "{tree}");
    assert!(
        !tree.contains("worktrees.empty"),
        "closed is not a folder: {tree}"
    );
    assert!(tree.contains("worktrees.detail.empty"), "{tree}");
    assert_eq!(selected(&model, vcx), None);
}
