//! The canary test (docs/design/welcome.md「日志与诊断」): a project path and
//! name, a session cwd, a session path, an error text and the session keys of
//! a review mark carry the canary through the whole flow at trace level, and
//! no record carries it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lectern_logging::testing;
use lectern_services::project::OpenError;

use super::support::*;
use crate::OpenSource;

const CANARY: &str = "welcome-canary-5d3a91c7";

#[test]
fn canary_stays_out_of_welcome_logs() {
    let capture = testing::capture();
    testing::set_callsite_limit(1_000_000);

    let mut rig = Rig::new();
    let repo = format!("/repos/{CANARY}");
    let repo_key = git_key(&format!("{repo}/.git"));
    // A file with the canary in a path and a name.
    // The entry carries a review mark whose session key has the canary.
    let file = format!(
        r#"{{"version":1,"entries":[{{"path":"{repo}","key":{{"Git":{{"common_dir":"{repo}/.git"}}}},"name":"{CANARY}","last_opened":1,"review":{{"sessions":[{{"source":"session-claude-code","id":"marked-{CANARY}"}}],"at":1}}}}]}}"#
    );
    rig.seed(file.as_bytes());
    rig.start_screen();
    rig.claude.set(vec![
        Sess::new(
            format!("session-{CANARY}"),
            format!("/work/{CANARY}/crates/{CANARY}"),
            resolved(
                &git_key(&format!("/work/{CANARY}/.git")),
                &format!("crates/{CANARY}"),
            ),
        )
        .running()
        .build(),
    ]);
    rig.start_discovery();
    rig.start_review();
    rig.settle();

    // An overview with other canary session keys replaces the mark (written
    // to the file, and counted against the session list).
    rig.review.set(
        &repo_key,
        &[
            (&format!("overview-{CANARY}"), true),
            (&format!("done-{CANARY}"), false),
        ],
    );
    rig.emit(rig.review.event(&repo_key));
    rig.settle();

    // The canary project opens, is removed, fails to open, and the write fails.
    let opened = git_key(&format!("/other/{CANARY}/.git"));
    rig.projects.add(snapshot(
        &opened,
        CANARY,
        &format!("/other/{CANARY}"),
        &format!("sub/{CANARY}"),
    ));
    rig.model.begin_open(
        PathBuf::from(format!("/other/{CANARY}")),
        OpenSource::Discovered,
    );
    rig.emit_opened(&opened);
    rig.settle();
    rig.model
        .remove_recent(Path::new(&format!("/other/{CANARY}/sub/{CANARY}")));
    rig.model
        .begin_open(PathBuf::from(format!("/gone/{CANARY}")), OpenSource::Recent);
    rig.emit_open_failed(OpenError::RepoUnreadable(Arc::from(format!(
        "{CANARY} broke"
    ))));
    rig.emit_open_failed(OpenError::Io(Arc::from(CANARY)));
    rig.model.language_not_saved();
    rig.settle();

    // A write into a directory that cannot be made, with a canary path.
    let blocked = Rig::new();
    crate::recent::store::testing::block_dir(&blocked.data_dir());
    blocked.start_screen();
    blocked.settle();
    blocked.projects.add(snapshot(&repo_key, CANARY, &repo, ""));
    blocked.emit_opened(&repo_key);
    blocked.settle();

    // A file that does not parse, with the canary in it.
    let broken = Rig::new();
    broken.seed(format!(r#"{{"version":1,"entries":[{{"path":"/bad/{CANARY}""#).as_bytes());
    broken.start_screen();
    broken.settle();

    rig.stop_review();
    rig.stop_discovery();
    drop(broken);
    drop(rig);
    drop(blocked);

    capture.assert_absent(CANARY);
    let lines = capture.lines();
    let has = |msg: &str| {
        lines
            .iter()
            .any(|l| l.contains("\"tg\":\"welcome\"") && l.contains(&format!("\"msg\":\"{msg}\"")))
    };
    for msg in [
        "recent list loaded",
        "recent list unreadable",
        "recent list not written",
        "project opened from welcome",
        "project open failed",
        "language override not saved",
        "discovery unavailable",
        "review marked",
    ] {
        assert!(has(msg), "{msg:?} is logged: {lines:#?}");
    }
}
