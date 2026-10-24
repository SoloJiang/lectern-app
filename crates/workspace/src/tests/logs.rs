//! The canary test (docs/design/workspace.md「日志与诊断」): a project path,
//! a screen title and a sidebar label with the canary go through the whole
//! flow at trace level, and no record carries it.

use std::path::PathBuf;

use lectern_logging::testing;
use lectern_services::workspace::{Route, SidebarSection, Target, WorkspaceCommand, screens};

use super::support::*;

const CANARY: &str = "workspace-canary-7c21e4a9";

#[test]
fn canary_stays_out_of_workspace_logs() {
    let capture = testing::capture();
    testing::set_callsite_limit(1_000_000);
    let mut rig = Rig::new();
    let kernel = contributor(
        &rig.hub,
        Contributor::staged(vec![
            Box::new(|c| {
                c.register(screen(
                    screens::PROJECT_HOME,
                    home_layout(),
                    &format!("Home {CANARY}"),
                ))
            }),
            Box::new(|c| {
                c.register(screen(
                    screens::REVIEW,
                    review_layout(),
                    &format!("Review {CANARY}"),
                ))
            }),
            Box::new(|c| {
                c.register(item(
                    "overview",
                    SidebarSection::Project,
                    0,
                    &format!("Overview {CANARY}"),
                ))
            }),
            Box::new(|c| c.register_leave_guard(FixedGuard::new(true))),
        ]),
    );
    kernel.activate().unwrap();
    rig.drain();

    let path = format!("/repos/{CANARY}");
    rig.open(&path, &project_key(&path));
    rig.dispatch(WorkspaceCommand::Push(Route {
        screen: name(screens::REVIEW),
        target: Target::None,
    }));
    rig.dispatch(WorkspaceCommand::SetInspector(true));
    rig.dispatch(WorkspaceCommand::Pop);
    rig.dispatch(WorkspaceCommand::Push(Route {
        screen: name("not-registered"),
        target: Target::None,
    }));
    rig.dispatch(WorkspaceCommand::OpenProject(PathBuf::from(format!(
        "/elsewhere/{CANARY}"
    ))));
    rig.emit(closed(&project_key(&path)));
    kernel.deactivate();
    rig.drain();
    assert_eq!(rig.driver.nav().state().project, None);

    capture.assert_absent(CANARY);
    let lines = capture.lines();
    let changed: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains("\"tg\":\"workspace\"") && l.contains("\"msg\":\"screen changed\""))
        .collect();
    assert!(
        changed.iter().any(|l| l.contains("\"to\":\"review\"")),
        "the screen change to review is logged by ID: {changed:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("\"msg\":\"contribution applied\"")
                && l.contains("\"id\":\"overview\"")),
        "applied contributions are logged by ID"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("\"msg\":\"screen not registered\"")
                && l.contains("\"id\":\"not-registered\"")),
    );
}
