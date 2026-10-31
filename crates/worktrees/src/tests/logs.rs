//! The canary test (docs/design/worktrees.md「日志与诊断」): the project name,
//! the branch names and the base reference, the worktree roots and git
//! directories, the paths of the overlaps, the session titles and the MR
//! titles carry the canary through the whole flow at trace level (the
//! plugins and the composition: opening, showing, events, usage, hiding,
//! closing, deactivation) and the views (a frame of the screen and the
//! inspector's page, ↓, ↵, the switch button, ⌘R and the project closing with
//! the views on screen), and no record carries it.

use gpui::TestAppContext;

use lectern_logging::testing;
use lectern_services::project::{ChangedParts, Head};
use lectern_services::usage::UsageState;
use lectern_services::workspace::WorkspaceCommand;

use crate::testing::{
    MINUTE, ProjectFx, branch, branch_usage, counted, mr_label, mr_ref, mr_snapshot, overlap,
    overview_of, sess, tokens, worktree_query, worktree_snapshot,
};
use lectern_services::usage::UsageQuery;

use super::stage::{click, frame, rig, window};

const CANARY: &str = "worktrees-canary-8c41e07a";

#[gpui::test]
fn canary_stays_out_of_worktrees_logs(cx: &mut TestAppContext) {
    let capture = testing::capture();
    testing::set_callsite_limit(1_000_000);
    let rig = rig(cx);
    rig.activate_main();
    rig.activate_usage();

    let fx = ProjectFx::git(&format!("app-{CANARY}"))
        .linked(&format!("feat-{CANARY}"), branch(&format!("feat/{CANARY}")))
        .linked(
            "det",
            Head::Detached(std::sync::Arc::from("0123456789abcdef")),
        )
        .prunable(&format!("gone-{CANARY}"), branch(&format!("gone/{CANARY}")));
    let key = fx.key();
    let main = fx.wt("main");
    let feat = fx.wt(&format!("feat-{CANARY}"));
    let known = sess(
        "fake",
        "11111111-2222-4333-8444-555555555555",
        &key,
        Some(&main),
    )
    .title(&format!("Fix the {CANARY}"))
    .pending()
    .open_turn()
    .ago(MINUTE)
    .build();
    let other = sess("fake", &format!("parent/agent-{CANARY}"), &key, Some(&feat))
        .title(&format!("Second {CANARY}"))
        .ago(2 * MINUTE)
        .build();
    rig.projects.set(fx.snapshot());
    rig.sessions.set(vec![known, other]);
    rig.review.set_overview(Some(overview_of(
        &key,
        Some(&format!("refs/remotes/origin/{CANARY}")),
        vec![
            counted(
                &main,
                1,
                2,
                3,
                vec![overlap(&feat, 2, &[&format!("src/{CANARY}.rs"), "b.rs"])],
            ),
            counted(
                &feat,
                1,
                2,
                3,
                vec![overlap(&main, 2, &[&format!("src/{CANARY}.rs"), "b.rs"])],
            ),
        ],
    )));
    rig.usage.set_snapshot(
        worktree_query(&key, &feat),
        worktree_snapshot(branch_usage(
            UsageState::Ready,
            Some(tokens(900, 100)),
            vec![
                mr_label(21, &format!("First {CANARY}")),
                mr_label(22, &format!("Second {CANARY}")),
            ],
        )),
    );
    rig.usage.set_snapshot(
        UsageQuery::Mr { mr: mr_ref(21) },
        mr_snapshot(21, UsageState::Ready, tokens(600, 60), vec![22]),
    );
    rig.usage.set_snapshot(
        worktree_query(&key, &main),
        worktree_snapshot(branch_usage(UsageState::Ready, Some(tokens(5, 5)), vec![])),
    );

    rig.opened(&key);
    rig.show();
    rig.run();
    // The views: a frame of both, ↓ to the next worktree, ↵, the inspector's
    // switch button, ⌘R.
    let (_host, vcx) = window(cx, &rig);
    vcx.simulate_keystrokes("down enter cmd-r");
    click(vcx, "worktrees.detail.switch");
    assert!(
        matches!(
            rig.contributions.commands().as_slice(),
            [
                WorkspaceCommand::Push(_),
                WorkspaceCommand::SetActiveWorktree(_)
            ]
        ),
        "↵ opened the review and the button switched"
    );
    assert_eq!(rig.review.refreshed().len(), 1);
    rig.sessions_changed();
    rig.changed_parts(
        &key,
        ChangedParts {
            active: true,
            ..ChangedParts::default()
        },
    );
    rig.worktrees_changed(&key);
    rig.usage_changed(worktree_query(&key, &feat));
    rig.run();
    // Left and shown again, then left while a composition runs.
    rig.hide();
    rig.show();
    rig.run();
    let kernel = rig.kernel.clone();
    rig.projects.on_next_snapshot(move || {
        kernel
            .emitter()
            .emit(lectern_services::workspace::WorkspaceEvent::ScreenChanged {
                window: lectern_services::workspace::WindowId(0),
                from: None,
                to: lectern_services::workspace::Route {
                    screen: lectern_services::workspace::screens::REVIEW.into(),
                    target: lectern_services::workspace::Target::None,
                },
            });
    });
    rig.sessions_changed();
    rig.run();
    rig.show();
    rig.run();
    rig.deactivate_usage();
    rig.run();
    frame(vcx);
    rig.projects.remove(&key);
    rig.closed(&key);
    rig.run();
    frame(vcx);
    rig.deactivate_main();

    capture.assert_absent(CANARY);
    capture.assert_absent("fixture");
    let lines = capture.lines();
    let built: Vec<&String> = lines
        .iter()
        .filter(|line| line.contains("\"msg\":\"worktrees snapshot built\""))
        .collect();
    assert!(!built.is_empty(), "the flow wrote its records: {lines:#?}");
    assert!(
        built.iter().any(|line| line.contains("\"worktrees\":4")),
        "counts are written instead of names: {built:#?}"
    );
    for message in [
        "worktrees command",
        "worktree switch requested",
        "worktrees view attached",
        "worktrees shown",
        "worktrees hidden",
        "worktrees build dropped",
        "worktrees complete requested",
    ] {
        assert!(
            lines
                .iter()
                .any(|line| line.contains(&format!("\"msg\":\"{message}\""))),
            "`{message}` is written: {lines:#?}"
        );
    }
}
