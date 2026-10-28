//! The views' behaviour (docs/design/project_home.md「测试」「`#[gpui::test]`」):
//! what the sidebar and the screen show for a snapshot, what the keys and
//! clicks send, and how they follow the services.

use std::sync::Arc;

use gpui::{TestAppContext, point, px, size};
use lectern_i18n::keys;
use lectern_services::project::Scan;
use lectern_services::review::{OutsideCount, ReviewTarget};
use lectern_services::session::Agent;
use lectern_services::workspace::{
    Route, SidebarSection, Target, WorkspaceCommand, WorkspaceEvent, screens,
};
use lectern_ui::Icon;
use lectern_ui::contrib::{SidebarItem, SidebarPanel};
use lectern_ui::motion::{LOADING_PROGRESS_AT, LOADING_SKELETON_AT};

use crate::testing::{MINUTE, ProjectFx, entry, overview, package, progress, sess};

use super::stage::*;

/// The IDs of the nodes drawn selected, outside the sidebar.
fn selected(tree: &str) -> Vec<&str> {
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

#[gpui::test]
fn sidebar_lists_recent_sessions_and_folds_the_rest(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let _world = standard(&rig);
    let (_host, vcx) = window(cx, &rig);
    let tree = tree(vcx, sidebar_part);
    // The newest three of all the worktrees (s3 is in the other one), then
    // "3 more"; no heading for a worktree.
    in_order(
        &tree,
        &[
            "project-home.sidebar.overview",
            "project-home.sidebar.sessions ",
            "sessions.item[s1]",
            "sessions.item[s2]",
            "sessions.item[s3]",
            "project-home.sidebar.sessions.more.facts Button label=[project-home-sessions-more] hidden=3 expanded=false",
            "project-home.sidebar.packages ",
            "packages.item[api]",
            "packages.item[web].facts Group label=[project-home-changed-today] changed=3 depth=0",
            "packages.item[web/ui].facts Group changed=0 depth=1",
        ],
    );
    assert!(!tree.contains("worktree"), "no worktree headings: {tree}");
    assert!(
        !tree.contains("sessions.item[s4]") && !tree.contains("sessions.item[s5]"),
        "folded away: {tree}"
    );
    assert!(tree.contains("project-home.sidebar Group sessions=6 shown=3"));
    assert!(
        tree.contains("sessions.item[s1].facts Group label=\"Fix the login redirect\" agent=claude-code running=true"),
        "{tree}"
    );
    assert!(
        tree.contains("sessions.item[s1].running Badge label=[project-home-running]"),
        "a running session wears the green dot: {tree}"
    );
    assert!(
        !tree.contains("sessions.item[s2].running"),
        "and a quiet one does not: {tree}"
    );
    assert!(
        tree.contains("project-home.sidebar.overview ListItem selected")
            && tree.contains("overview.facts Text label=[project-home-overview]"),
        "the Overview entry is selected on the home screen"
    );

    // The row unfolds the rest (14 days: s4, s6, s5 by recency) and folds it again.
    click(vcx, "project-home.sidebar.sessions.more");
    let open = super::stage::tree(vcx, sidebar_part);
    in_order(
        &open,
        &[
            "sessions.item[s3]",
            "sessions.item[s4]",
            "sessions.item[s6]",
            "sessions.item[s5].facts Group label=[project-home-untitled-session] agent=other",
            "project-home.sidebar.sessions.more.facts Button label=[project-home-sessions-less] hidden=3 expanded=true",
            "project-home.sidebar.packages ",
        ],
    );
    assert!(open.contains("project-home.sidebar Group sessions=6 shown=6"));
    click(vcx, "project-home.sidebar.sessions.more");
    let folded = super::stage::tree(vcx, sidebar_part);
    assert!(!folded.contains("sessions.item[s4]"), "{folded}");
    assert!(folded.contains("label=[project-home-sessions-more] hidden=3 expanded=false"));
    assert!(
        rig.contributions.commands().is_empty(),
        "folding opens nothing"
    );
}

#[gpui::test]
fn sidebar_builds_only_visible_rows(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let fx = ProjectFx::git("big").packages(Scan::Ready(
        (0..2000)
            .map(|i| package(&format!("pkg/p{i:04}"), &format!("p{i}"), None))
            .collect(),
    ));
    let key = fx.key();
    let main = fx.wt("main");
    rig.projects.set(fx.snapshot());
    rig.sessions.set(
        (0..1000)
            .map(|i| {
                sess("fake", &format!("s{i:04}"), &key, Some(&main))
                    .ago(i * 10 * MINUTE)
                    .build()
            })
            .collect(),
    );
    rig.activate_home();
    rig.opened(&key);
    rig.run();
    let (host, vcx) = window(cx, &rig);
    let (sidebar, _) = views(&host, vcx);
    let height = vcx.update(|window, _| window.viewport_size().height);
    let screenful = (height / px(28.0)).ceil() as usize;
    let built = sidebar.read_with(vcx, |sidebar, _| sidebar.built_rows());
    let tree = tree(vcx, sidebar_part);
    assert!(
        tree.contains("rows=2007"),
        "overview + heading + 3 sessions + \"997 more\" + heading + 2000 packages: {}",
        tree.lines().nth(1).unwrap_or_default()
    );
    assert!(built > 0, "something is drawn");
    assert!(
        built <= screenful * 2,
        "{built} rows built for a {screenful}-row window"
    );
    // Unfolded, the 1000 sessions are rows of the list, and a frame still
    // builds only the visible ones.
    click(vcx, "project-home.sidebar.sessions.more");
    let open = super::stage::tree(vcx, sidebar_part);
    assert!(
        open.contains("rows=3004"),
        "{}",
        open.lines().nth(1).unwrap_or_default()
    );
    let unfolded = sidebar.read_with(vcx, |sidebar, _| sidebar.built_rows());
    assert!(unfolded > 0 && unfolded <= screenful * 2, "{unfolded} rows");
    // Scrolling builds other rows, not more of them.
    vcx.simulate_event(gpui::ScrollWheelEvent {
        position: point(px(100.0), px(100.0)),
        delta: gpui::ScrollDelta::Pixels(point(px(0.0), px(-20_000.0))),
        ..Default::default()
    });
    frame(vcx);
    let after = sidebar.read_with(vcx, |sidebar, _| sidebar.built_rows());
    let moved = super::stage::tree(vcx, sidebar_part);
    assert!(
        !moved.contains("sessions.item[s0000]"),
        "the list scrolled: {moved}"
    );
    assert!(after > 0 && after <= screenful * 2, "{after} rows");
}

#[gpui::test]
fn home_shows_cards_outside_and_done(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let _world = standard(&rig);
    let (_host, vcx) = window(cx, &rig);
    let tree = tree(vcx, main_part);
    in_order(
        &tree,
        &[
            "project-home.title Text label=[project-home-title] count=4 ready=true",
            "project-home.needs-review List label=[project-home-needs-review]",
            "needs-review.item[s1]",
            "needs-review.item[s2]",
            "needs-review.item[s5]",
            "needs-review.item[s6]",
            "project-home.outside.group List label=[project-home-outside]",
            "project-home.outside ListItem",
            "project-home.done List label=[project-home-done]",
            "done.item[s4]",
        ],
    );
    assert!(
        tree.contains("outside.facts Group label=[project-home-outside-files] files=4"),
        "{tree}"
    );
    assert!(
        tree.contains("drifted=true seen=2 total=5") && tree.contains("critical=1"),
        "the signals' sources are on the card: {tree}"
    );
    assert!(
        tree.contains("needs-review.item[s5].facts Group label=[project-home-untitled-session]"),
        "no ring for a session without progress: {tree}"
    );
    assert!(!tree.contains("needs-review.item[s5].progress"));
    // The session of the other worktree is in the sidebar only.
    assert!(!tree.contains("s3"));
}

#[gpui::test]
fn arrow_keys_move_across_groups(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let _world = standard(&rig);
    let (_host, vcx) = window(cx, &rig);
    let pick = |vcx: &mut gpui::VisualTestContext| -> String {
        frame(vcx);
        let tree = tree(vcx, main_part);
        selected(&tree)
            .into_iter()
            .map(|id| id.trim_start_matches("project-home.").to_owned())
            .collect::<Vec<_>>()
            .join(",")
    };
    assert_eq!(pick(vcx), "", "nothing is selected at first");
    let down = [
        "needs-review.item[s1]",
        "needs-review.item[s2]",
        "needs-review.item[s5]",
        "needs-review.item[s6]",
        "outside",
        "done.item[s4]",
        "done.item[s4]",
    ];
    for expected in down {
        vcx.simulate_keystrokes("down");
        assert_eq!(pick(vcx), expected, "↓");
    }
    let up = [
        "outside",
        "needs-review.item[s6]",
        "needs-review.item[s5]",
        "needs-review.item[s2]",
        "needs-review.item[s1]",
        "needs-review.item[s1]",
    ];
    for expected in up {
        vcx.simulate_keystrokes("up");
        assert_eq!(pick(vcx), expected, "↑");
    }
    vcx.simulate_keystrokes("alt-down");
    assert_eq!(pick(vcx), "done.item[s4]", "⌥↓");
    vcx.simulate_keystrokes("alt-up");
    assert_eq!(pick(vcx), "needs-review.item[s1]", "⌥↑");
}

#[gpui::test]
fn enter_pushes_the_review_route(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard(&rig);
    let (_host, vcx) = window(cx, &rig);
    vcx.simulate_keystrokes("enter");
    assert!(
        rig.contributions.commands().is_empty(),
        "↵ with nothing selected only selects the first item"
    );
    frame(vcx);
    let tree = tree(vcx, main_part);
    assert_eq!(selected(&tree), ["project-home.needs-review.item[s1]"]);

    vcx.simulate_keystrokes("enter");
    assert_eq!(
        rig.contributions.commands(),
        [push_review(ReviewTarget::Session(world.running.clone()))]
    );

    vcx.simulate_keystrokes("down down enter");
    assert_eq!(
        rig.contributions.commands().last(),
        Some(&push_review(ReviewTarget::Session(world.other.clone()))),
        "the third card"
    );
}

#[gpui::test]
fn outside_opens_the_outside_review(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard(&rig);
    let (_host, vcx) = window(cx, &rig);
    let outside = push_review(ReviewTarget::Outside {
        project: world.key.clone(),
        worktree: world.main.clone(),
    });
    click(vcx, "project-home.outside");
    assert_eq!(rig.contributions.commands(), std::slice::from_ref(&outside));

    // By key: the first card, ↓ four times to the row, ↵.
    vcx.simulate_keystrokes("alt-up down down down down enter");
    assert_eq!(rig.contributions.commands().last(), Some(&outside));
    assert_eq!(rig.contributions.commands().len(), 2);
}

#[gpui::test]
fn the_review_button_of_the_outside_card_opens_it_once(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard(&rig);
    let (_host, vcx) = window(cx, &rig);
    click(vcx, "project-home.outside.review");
    assert_eq!(
        rig.contributions.commands(),
        [push_review(ReviewTarget::Outside {
            project: world.key.clone(),
            worktree: world.main.clone(),
        })],
        "one click on the button is one command, not one for the button and one for the card"
    );
}

#[gpui::test]
fn the_lede_leaves_out_what_is_zero(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard(&rig);
    let (_host, vcx) = window(cx, &rig);
    let tree = tree(vcx, main_part);
    assert!(
        tree.contains("project-home.lede Text label=\"1 agent is running. 4 files changed outside any session.\" running=1 outside=4"),
        "{tree}"
    );

    // Nothing runs any more and nothing changed outside: no second line.
    rig.sessions.set(Vec::new());
    rig.review.set_overview(Some(overview(
        &world.key,
        Vec::new(),
        OutsideCount::Files(0),
    )));
    rig.sessions_changed();
    rig.overview_changed(&world.key);
    rig.run();
    frame(vcx);
    let quiet = super::stage::tree(vcx, main_part);
    assert!(!quiet.contains("project-home.lede"), "{quiet}");
    // A count that is not known is not said either.
    rig.review.set_overview(Some(overview(
        &world.key,
        Vec::new(),
        OutsideCount::Capped(5000),
    )));
    rig.overview_changed(&world.key);
    rig.run();
    frame(vcx);
    let capped = super::stage::tree(vcx, main_part);
    assert!(!capped.contains("project-home.lede"), "{capped}");
}

#[gpui::test]
fn clicking_a_card_and_a_sidebar_session_pushes_review(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard(&rig);
    let (_host, vcx) = window(cx, &rig);
    click(vcx, "project-home.sidebar.sessions.item[s2]");
    assert_eq!(
        rig.contributions.commands(),
        [push_review(ReviewTarget::Session(world.codex.clone()))]
    );
    click(vcx, "project-home.done.item[s4]");
    assert_eq!(
        rig.contributions.commands().last(),
        Some(&push_review(ReviewTarget::Session(world.done.clone())))
    );
    // The session of the other worktree opens from the sidebar.
    click(vcx, "project-home.sidebar.sessions.item[s3]");
    assert_eq!(
        rig.contributions.commands().last(),
        Some(&push_review(ReviewTarget::Session(world.linked.clone())))
    );
    assert_eq!(rig.contributions.commands().len(), 3);
}

fn entry_item(label: lectern_i18n::Key, command: WorkspaceCommand) -> SidebarItem {
    SidebarItem {
        label,
        icon: None,
        on_select: command,
    }
}

#[gpui::test]
fn views_entries_dispatch_their_command(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let _world = standard(&rig);
    rig.contributions.add_entry(
        SidebarSection::Views,
        "worktrees",
        0,
        SidebarItem {
            label: keys::workspace::BACK,
            icon: Some(Icon::Folder),
            on_select: WorkspaceCommand::Push(Route {
                screen: screens::WORKTREES.into(),
                target: Target::None,
            }),
        },
    );
    rig.contributions.add_entry(
        SidebarSection::Views,
        "files",
        1,
        entry_item(
            keys::workspace::INSPECTOR_TOGGLE,
            WorkspaceCommand::ShowSidebarPanel(Some("files".into())),
        ),
    );
    let (_host, vcx) = window(cx, &rig);
    let tree = tree(vcx, sidebar_part);
    in_order(
        &tree,
        &[
            "packages.item[web/ui]",
            "project-home.sidebar.views Text label=[project-home-views]",
            "views.item[worktrees].facts Text label=[workspace-back]",
            "views.item[files].facts Text label=[workspace-inspector-toggle]",
        ],
    );
    click(vcx, "project-home.sidebar.views.item[files]");
    click(vcx, "project-home.sidebar.views.item[worktrees]");
    assert_eq!(
        rig.contributions.commands(),
        [
            WorkspaceCommand::ShowSidebarPanel(Some("files".into())),
            WorkspaceCommand::Push(Route {
                screen: screens::WORKTREES.into(),
                target: Target::None,
            }),
        ]
    );
    // The Overview entry goes back to the base.
    click(vcx, "project-home.sidebar.overview");
    assert_eq!(
        rig.contributions.commands().last(),
        Some(&WorkspaceCommand::PopToBase)
    );
}

#[gpui::test]
fn late_entries_appear_without_a_new_snapshot(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let _world = standard(&rig);
    let (_host, vcx) = window(cx, &rig);
    let before = rig.snapshot().expect("a snapshot");
    let builds = rig.projects.snapshot_calls();

    rig.contributions.add_entry(
        SidebarSection::Project,
        "reviews",
        5,
        entry_item(keys::workspace::BACK, WorkspaceCommand::Pop),
    );
    rig.contributions.add_entry(
        SidebarSection::Views,
        "files",
        0,
        entry_item(keys::workspace::INSPECTOR_TOGGLE, WorkspaceCommand::Pop),
    );
    rig.workspace_event(WorkspaceEvent::ContributionsChanged);
    frame(vcx);

    assert!(Arc::ptr_eq(&before, &rig.snapshot().unwrap()));
    assert_eq!(
        rig.projects.snapshot_calls(),
        builds,
        "nothing was composed"
    );
    let tree = tree(vcx, sidebar_part);
    in_order(
        &tree,
        &[
            "sidebar.overview",
            "sidebar.project.item[reviews]",
            "sidebar.sessions ",
            "sidebar.views.item[files]",
        ],
    );
}

#[gpui::test]
fn the_overview_entry_follows_the_current_screen(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard(&rig);
    let (_host, vcx) = window(cx, &rig);
    let route = |screen: &str| Route {
        screen: screen.to_owned().into(),
        target: Target::Project(world.key.clone()),
    };
    rig.workspace_event(WorkspaceEvent::ScreenChanged {
        window: lectern_services::workspace::WindowId(0),
        from: Some(route(screens::PROJECT_HOME)),
        to: route(screens::USAGE),
    });
    frame(vcx);
    let tree = tree(vcx, sidebar_part);
    assert!(
        tree.contains("sidebar.overview ListItem\n")
            && !tree.contains("sidebar.overview ListItem selected"),
        "{tree}"
    );
    rig.workspace_event(WorkspaceEvent::ScreenChanged {
        window: lectern_services::workspace::WindowId(0),
        from: Some(route(screens::USAGE)),
        to: route(screens::PROJECT_HOME),
    });
    frame(vcx);
    let tree = super::stage::tree(vcx, sidebar_part);
    assert!(tree.contains("sidebar.overview ListItem selected"));
}

#[gpui::test]
fn usage_row_shows_tokens_and_the_tightest_limit(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard(&rig);
    // The entry is registered by `usage_view`, but the usage plugin is off:
    // only the label.
    rig.contributions.add_entry(
        SidebarSection::Bottom,
        "usage",
        0,
        entry_item(keys::workspace::BACK, WorkspaceCommand::Pop),
    );
    let (_host, vcx) = window(cx, &rig);
    let bare = tree(vcx, sidebar_part);
    assert!(
        bare.contains("project-home.sidebar.usage ListItem tokens=\n"),
        "{bare}"
    );
    assert!(!bare.contains("usage.limit"));

    // The usage plugin arrives later: the row gets its figures.
    rig.usage.set_snapshot(Some(
        (*crate::testing::project_usage(lectern_services::usage::UsageState::Ready, 12_345))
            .clone(),
    ));
    rig.usage.set_limits(
        (*crate::testing::usage_limits(vec![(Agent::ClaudeCode, 61.4)], Some(0))).clone(),
    );
    rig.activate_usage();
    rig.run();
    frame(vcx);
    let tree = super::stage::tree(vcx, sidebar_part);
    assert!(
        tree.contains("project-home.sidebar.usage ListItem tokens=12345 percent=61"),
        "{tree}"
    );
    assert!(
        tree.contains("usage.limit Progress fraction=0.610"),
        "{tree}"
    );

    click(vcx, "project-home.sidebar.usage");
    assert_eq!(
        rig.contributions.commands(),
        [WorkspaceCommand::Push(Route {
            screen: screens::USAGE.into(),
            target: Target::Project(world.key.clone()),
        })]
    );
}

#[gpui::test]
fn new_sidebar_view_reads_the_current_snapshot(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let _world = standard(&rig);
    let (host, vcx) = window(cx, &rig);
    let home = rig.home();
    assert_eq!(home.view_count(), 2, "the sidebar and the screen");

    // Files replaces the panel, then it comes back: a new view.
    host.update(vcx, |host, cx| {
        host.sidebar = None;
        cx.notify();
    });
    frame(vcx);
    // A composition while no sidebar exists: the dropped view is forgotten.
    rig.sessions_changed();
    rig.run();
    frame(vcx);
    assert_eq!(home.view_count(), 1, "only the screen");

    let panel: SidebarPanel = rig.contributions.payload("project").unwrap();
    host.update_in(vcx, |host, window, cx| {
        host.sidebar = Some((panel.build)(&(), window, cx));
        cx.notify();
    });
    // No event arrives, and the rows are there at once.
    let tree = tree(vcx, sidebar_part);
    assert!(tree.contains("sessions.item[s1]"), "{tree}");
    assert_eq!(home.view_count(), 2);
}

#[gpui::test]
fn progress_ring_transitions_between_snapshots(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard(&rig);
    let (host, vcx) = window(cx, &rig);
    let (_, screen) = views(&host, vcx);
    let ring = |vcx: &mut gpui::VisualTestContext| {
        screen.read_with(vcx, |screen, _| screen.ring(&world.running))
    };
    assert_eq!(
        ring(vcx),
        Some((0.4, 0.4)),
        "a new ring starts at its value"
    );

    // 2 of 5 become 4 of 5.
    let reviewing = |seen: u32, codex: bool| {
        let mut entries = vec![entry(&world.running, progress(seen, 5, 0), Some(true), 1)];
        if codex {
            entries.push(entry(&world.codex, progress(0, 3, 0), Some(false), 0));
        }
        overview(&world.key, entries, OutsideCount::Files(4))
    };
    rig.review.set_overview(Some(reviewing(4, true)));
    rig.overview_changed(&world.key);
    rig.run();
    vcx.run_until_parked();
    let (value, target) = ring(vcx).expect("the card is still there");
    assert_eq!(target, 0.8);
    assert!(
        (0.4..0.8).contains(&value),
        "the ring goes on from 0.4 to 0.8, not from 0 or at once: {value}"
    );

    // 5 of 5: the card moves to Recently Done, whose rows have no ring.
    rig.review.set_overview(Some(reviewing(5, true)));
    rig.overview_changed(&world.key);
    rig.run();
    vcx.run_until_parked();
    frame(vcx);
    let tree = tree(vcx, main_part);
    assert!(tree.contains("done.item[s1]"), "{tree}");
    assert!(!tree.contains("done.item[s1].progress"), "{tree}");
    assert_eq!(ring(vcx), None, "a row of Recently Done keeps no ring");
    // Cards that left the snapshot lose their rings too.
    assert_eq!(
        screen.read_with(vcx, |screen, _| screen.ring(&world.changed)),
        None
    );
}

#[gpui::test]
fn milestone_is_reported_on_the_first_settled_frame(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let fx = ProjectFx::git("app").files(Scan::Pending);
    let key = fx.key();
    rig.projects.set(fx.snapshot());
    rig.sessions
        .set(vec![sess("fake", "s1", &key, Some(&fx.wt("main"))).build()]);
    rig.activate_home();
    rig.opened(&key);
    rig.run();
    let (_host, vcx) = window(cx, &rig);
    frame(vcx);
    assert!(
        rig.contributions.milestones().is_empty(),
        "the files are pending: the frame is not the milestone"
    );

    rig.projects
        .set(ProjectFx::git("app").files(Scan::Ready(())).snapshot());
    rig.changed(&key);
    rig.run();
    frame(vcx);
    assert_eq!(
        rig.contributions.milestones(),
        [lectern_services::workspace::Milestone::ProjectHome]
    );
    rig.changed(&key);
    rig.run();
    frame(vcx);
    assert_eq!(rig.contributions.milestones().len(), 1, "once");
}

#[gpui::test]
fn skeleton_appears_after_100ms_only(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let fx = ProjectFx::git("app");
    let key = fx.key();
    rig.projects.set(fx.snapshot());
    rig.sessions
        .set(vec![sess("fake", "s1", &key, Some(&fx.wt("main"))).build()]);
    // `Review::overview` has no result yet.
    rig.review.set_overview(None);
    rig.activate_home();
    rig.opened(&key);
    rig.run();
    let (_host, vcx) = window(cx, &rig);

    let tree0 = tree(vcx, main_part);
    assert!(
        tree0.contains("project-home.title Text label=[project-home-title] count=0 ready=false")
    );
    assert!(tree0.contains("outside.facts Group pending=true"));
    assert!(
        !tree0.contains("loading"),
        "nothing in the first 100 ms: {tree0}"
    );

    vcx.executor().advance_clock(LOADING_SKELETON_AT);
    vcx.run_until_parked();
    let tree1 = tree(vcx, main_part);
    assert!(
        tree1.contains("project-home.loading Group phase=skeleton"),
        "{tree1}"
    );

    vcx.executor()
        .advance_clock(LOADING_PROGRESS_AT - LOADING_SKELETON_AT);
    vcx.run_until_parked();
    let tree2 = tree(vcx, main_part);
    assert!(
        tree2.contains("project-home.loading Group phase=progress"),
        "{tree2}"
    );

    // The overview arrives: the placeholder is gone.
    rig.review
        .set_overview(Some(overview(&key, Vec::new(), OutsideCount::Files(0))));
    rig.overview_changed(&key);
    rig.run();
    frame(vcx);
    let tree3 = tree(vcx, main_part);
    assert!(!tree3.contains("loading"), "{tree3}");
    // Nothing changed outside the sessions: no Outside card.
    assert!(!tree3.contains("project-home.outside"), "{tree3}");
}

#[gpui::test]
fn closed_project_clears_the_views(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard(&rig);
    let (_host, vcx) = window(cx, &rig);
    rig.projects.remove(&world.key);
    rig.closed(&world.key);
    frame(vcx);
    let sidebar = tree(vcx, sidebar_part);
    assert!(!sidebar.contains("sessions.item"), "{sidebar}");
    assert!(!sidebar.contains("packages"), "{sidebar}");
    assert_eq!(tree(vcx, main_part), "", "the screen shows nothing");
    // A key press on the empty screen does nothing.
    vcx.simulate_keystrokes("down enter");
    assert!(rig.contributions.commands().is_empty());
}

#[gpui::test]
fn selection_scrolls_into_view(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let fx = ProjectFx::git("app");
    let key = fx.key();
    let main = fx.wt("main");
    rig.projects.set(fx.snapshot());
    let sessions: Vec<_> = (0..40)
        .map(|i| {
            sess("fake", &format!("s{i:02}"), &key, Some(&main))
                .ago(i * MINUTE)
                .build()
        })
        .collect();
    let entries = sessions
        .iter()
        .map(|s| entry(&s.key, progress(0, 3, 0), None, 0))
        .collect();
    rig.sessions.set(sessions);
    rig.review
        .set_overview(Some(overview(&key, entries, OutsideCount::Files(1))));
    rig.activate_home();
    rig.opened(&key);
    rig.run();
    let (host, vcx) = window(cx, &rig);
    vcx.simulate_resize(size(px(1100.0), px(400.0)));
    frame(vcx);
    let (_, screen) = views(&host, vcx);
    assert_eq!(screen.read_with(vcx, |s, _| s.scrolled()), px(0.0));

    vcx.simulate_keystrokes("alt-down");
    frame(vcx);
    frame(vcx);
    let records = record(vcx);
    // The Outside row is the last item (no card is done): the list scrolled
    // to the end and the row is on screen.
    let last = records
        .iter()
        .find(|r| r.semantic.id.as_str() == "project-home.outside")
        .expect("the Outside row exists");
    assert!(screen.read_with(vcx, |s, _| s.scrolled()) < px(0.0));
    assert!(last.visible, "the last item is scrolled into view");
}

#[gpui::test]
fn a_new_round_makes_the_dot_blink(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard(&rig);
    let (host, vcx) = window(cx, &rig);
    let (sidebar, _) = views(&host, vcx);
    let rounds = |vcx: &mut gpui::VisualTestContext| {
        sidebar.read_with(vcx, |sidebar, _| sidebar.rounds_of(&world.running))
    };
    let set = |running: bool, ago: i64| {
        let mut session = sess("fake", "s1", &world.key, Some(&world.main)).ago(ago);
        if running {
            session = session.open_turn();
        }
        rig.sessions.set(vec![session.build()]);
        rig.sessions_changed();
        rig.run();
    };
    // The session was running when the view was built: no round yet.
    assert_eq!(rounds(vcx), Some(0));
    set(true, 2 * MINUTE);
    vcx.run_until_parked();
    assert_eq!(rounds(vcx), Some(0), "the same activity is the same round");
    set(true, MINUTE);
    vcx.run_until_parked();
    assert_eq!(rounds(vcx), Some(1), "it moved while running");
    set(false, MINUTE);
    vcx.run_until_parked();
    assert_eq!(rounds(vcx), Some(1), "it stopped: no new round");
    set(true, 0);
    vcx.run_until_parked();
    assert_eq!(rounds(vcx), Some(2), "it started running again");
}

#[gpui::test]
fn packages_follow_the_scan_state(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let fx = ProjectFx::git("app").packages(Scan::Pending);
    let key = fx.key();
    rig.projects.set(fx.snapshot());
    rig.sessions
        .set(vec![sess("fake", "s1", &key, Some(&fx.wt("main"))).build()]);
    rig.activate_home();
    rig.opened(&key);
    rig.run();
    let (_host, vcx) = window(cx, &rig);
    let pending = tree(vcx, sidebar_part);
    assert!(
        pending.contains("sidebar.packages Text label=[project-home-packages] counts=false"),
        "the heading stays while the sub-projects are scanned: {pending}"
    );
    assert!(!pending.contains("packages.item"));
    assert!(
        pending.contains("rows=5"),
        "overview, Sessions, session, Packages, block: {pending}"
    );

    rig.projects.set(
        ProjectFx::git("app")
            .packages(Scan::Ready(vec![package("a", "a", None)]))
            .snapshot(),
    );
    rig.changed(&key);
    rig.run();
    frame(vcx);
    let ready = super::stage::tree(vcx, sidebar_part);
    assert!(
        ready.contains("packages.item[a].facts Group changed=0 depth=0"),
        "{ready}"
    );

    rig.projects.set(
        ProjectFx::git("app")
            .packages(Scan::Unavailable(crate::testing::unavailable()))
            .snapshot(),
    );
    rig.changed(&key);
    rig.run();
    frame(vcx);
    let unavailable = super::stage::tree(vcx, sidebar_part);
    assert!(
        !unavailable.contains("packages"),
        "no section: {unavailable}"
    );
}

#[gpui::test]
fn a_folder_project_has_no_worktrees_and_no_outside_row(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let fx = ProjectFx::folder("notes");
    let key = fx.key();
    rig.projects.set(fx.snapshot());
    let session = sess("fake", "s1", &key, None).build();
    rig.review.set_overview(Some(overview(
        &key,
        vec![entry(&session.key, None, None, 0)],
        OutsideCount::Pending,
    )));
    rig.sessions.set(vec![session]);
    rig.activate_home();
    rig.opened(&key);
    rig.run();
    let (_host, vcx) = window(cx, &rig);
    let sidebar = tree(vcx, sidebar_part);
    assert!(sidebar.contains("sessions.item[s1]"));
    assert!(!sidebar.contains("worktree"), "{sidebar}");
    let screen: lectern_ui::contrib::Screen = rig.contributions.payload("project-home").unwrap();
    let toolbar = vcx.update(|_, cx| (screen.toolbar)(cx));
    assert_eq!(toolbar.title.as_str(), "notes");
    assert!(
        toolbar.subtitle.is_none(),
        "a folder has neither a branch nor worktrees"
    );
    let main = super::stage::tree(vcx, main_part);
    assert!(main.contains("needs-review.item[s1]"), "{main}");
    assert!(!main.contains("project-home.outside"), "{main}");
    // No base to compare with: ↵ on an empty selection picks the card only.
    vcx.simulate_keystrokes("down down enter");
    assert!(
        matches!(
            rig.contributions.commands().as_slice(),
            [WorkspaceCommand::Push(Route {
                target: Target::Review(ReviewTarget::Session(_)),
                ..
            })]
        ),
        "{:?}",
        rig.contributions.commands()
    );
}

#[gpui::test]
fn other_worktrees_and_a_capped_overview(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let fx = ProjectFx::git("app").prunable("old", crate::testing::branch("old"));
    let key = fx.key();
    rig.projects.set(fx.snapshot());
    let gone = sess("fake", "s1", &key, Some(&fx.wt("old"))).build();
    rig.review.set_overview(Some(overview(
        &key,
        vec![entry(&gone.key, progress(1, 2, 0), None, 0)],
        OutsideCount::Capped(1234),
    )));
    rig.sessions.set(vec![gone]);
    rig.activate_home();
    rig.opened(&key);
    rig.run();
    let (_host, vcx) = window(cx, &rig);
    let sidebar = tree(vcx, sidebar_part);
    assert!(
        sidebar.contains("sessions.item[s1]") && !sidebar.contains("worktree"),
        "a session of a worktree that is gone is a row like the others: {sidebar}"
    );
    let main = super::stage::tree(vcx, main_part);
    assert!(
        main.contains("outside.facts Group label=[project-home-outside-capped] capped=1234"),
        "{main}"
    );
}

#[gpui::test]
fn the_toolbar_shows_the_name_the_branch_and_the_worktrees(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let world = standard(&rig);
    let (_host, vcx) = window(cx, &rig);
    let screen: lectern_ui::contrib::Screen = rig.contributions.payload("project-home").unwrap();
    let items = vcx.update(|_, cx| (screen.toolbar)(cx));
    assert_eq!(items.title.as_str(), "app");
    assert_eq!(
        items.title.message_id(),
        None,
        "the name is shown as written"
    );
    let subtitle = items.subtitle.expect("a subtitle");
    assert_eq!(subtitle.message_id(), Some("project-home-subtitle"));
    assert_eq!(
        subtitle.as_str().replace(['\u{2068}', '\u{2069}'], ""),
        "main · 2 worktrees",
        "the branch and the number of worktrees"
    );
    assert!(items.controls.is_empty());

    rig.projects.remove(&world.key);
    rig.closed(&world.key);
    let items = vcx.update(|_, cx| (screen.toolbar)(cx));
    assert_eq!(items.title.message_id(), Some("project-home-overview"));
    assert!(items.subtitle.is_none());
}

#[gpui::test]
fn the_screen_takes_the_focus_back_from_the_root_only(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let _world = standard(&rig);
    let (host, vcx) = window(cx, &rig);
    let (root, overlay) = host.read_with(vcx, |host, _| (host.focus.clone(), host.overlay.clone()));
    let picked = |vcx: &mut gpui::VisualTestContext| {
        frame(vcx);
        selected(&tree(vcx, main_part))
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };

    // A screen above was popped: the workspace's root has the focus.
    vcx.update(|window, cx| window.focus(&root, cx));
    frame(vcx);
    vcx.simulate_keystrokes("down");
    assert_eq!(
        picked(vcx),
        ["project-home.needs-review.item[s1]"],
        "↓ works after the focus went to the root"
    );

    // An overlay has the focus: the screen leaves it alone, and its keys
    // do not reach the screen.
    vcx.update(|window, cx| window.focus(&overlay, cx));
    frame(vcx);
    frame(vcx);
    assert!(vcx.update(|window, _| overlay.is_focused(window)));
    vcx.simulate_keystrokes("down");
    assert_eq!(picked(vcx), ["project-home.needs-review.item[s1]"]);
}
