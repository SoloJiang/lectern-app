//! The canary test (docs/design/project_home.md「日志与诊断」): a session
//! title, a project name, a branch name, sub-project names and a worktree
//! root with the canary go through the whole flow at trace level (the
//! composition, the plugins, a frame of both views, ↓ and ↵, and the project
//! closing with the views on screen), and no record carries it.

use gpui::TestAppContext;
use lectern_logging::testing;
use lectern_services::project::Scan;
use lectern_services::review::OutsideCount;
use lectern_services::usage::UsageState;

use crate::testing::{
    MINUTE, ProjectFx, branch, entry, overview, package, progress, project_usage, sess,
    usage_limits,
};
use lectern_services::session::Agent;
use lectern_services::workspace::WorkspaceCommand;

use super::stage::{frame, rig, window};

const CANARY: &str = "home-canary-5d90b3e1";

#[gpui::test]
fn canary_stays_out_of_home_logs(cx: &mut TestAppContext) {
    let capture = testing::capture();
    testing::set_callsite_limit(1_000_000);
    let rig = rig(cx);
    rig.activate_home();
    rig.activate_usage();

    let fx = ProjectFx::git(&format!("app-{CANARY}"))
        .linked("feat", branch(&format!("feat/{CANARY}")))
        .packages(Scan::Ready(vec![
            package("", &format!("root-{CANARY}"), None),
            package(&format!("dir-{CANARY}"), &format!("pkg-{CANARY}"), Some(0)),
        ]));
    let key = fx.key();
    let main = fx.wt("main");
    let feat = fx.wt("feat");
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
    let other = sess("fake", "parent/agent-9", &key, Some(&feat))
        .title(&format!("Second {CANARY}"))
        .ago(2 * MINUTE)
        .build();
    let vanished = sess("fake", "vanished", &key, Some(&main)).build();
    rig.projects.set(fx.snapshot());
    rig.sessions.set(vec![known.clone(), other.clone()]);
    rig.review.set_overview(Some(overview(
        &key,
        vec![
            entry(&known.key, progress(1, 3, 0), Some(true), 2),
            entry(&vanished.key, progress(1, 3, 0), None, 0),
            entry(&other.key, progress(3, 3, 0), None, 0),
        ],
        OutsideCount::Files(2),
    )));
    rig.usage
        .set_snapshot(Some((*project_usage(UsageState::Ready, 4_200)).clone()));
    rig.usage
        .set_limits((*usage_limits(vec![(Agent::ClaudeCode, 40.0)], Some(0))).clone());

    rig.opened(&key);
    rig.run();
    let (_host, vcx) = window(cx, &rig);
    // The first card is the session with the canary in its title.
    vcx.simulate_keystrokes("down enter");
    assert!(
        matches!(
            rig.contributions.commands().as_slice(),
            [WorkspaceCommand::Push(_)]
        ),
        "↵ opened the review of the picked session"
    );
    rig.sessions_changed();
    rig.changed(&key);
    rig.overview_changed(&key);
    rig.run();
    frame(vcx);
    rig.deactivate_usage();
    rig.run();
    frame(vcx);
    rig.closed(&key);
    rig.run();
    frame(vcx);

    capture.assert_absent(CANARY);
    capture.assert_absent("fixture");
    let lines = capture.lines();
    let built: Vec<&String> = lines
        .iter()
        .filter(|line| line.contains("\"msg\":\"home snapshot built\""))
        .collect();
    assert!(!built.is_empty(), "the flow wrote its records: {lines:#?}");
    assert!(
        built.iter().any(|line| line.contains("\"cards\":1")),
        "counts are written instead of names: {built:#?}"
    );
    for message in [
        "home command",
        "home milestone reported",
        "home view attached",
    ] {
        assert!(
            lines
                .iter()
                .any(|line| line.contains(&format!("\"msg\":\"{message}\""))),
            "`{message}` is written: {lines:#?}"
        );
    }
    assert!(
        lines
            .iter()
            .any(|line| line.contains("\"msg\":\"overview session not in list\"")),
        "the session that is not in any list is reported by its tag"
    );
}
