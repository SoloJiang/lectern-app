//! `log_*` (docs/design/usage.md「日志与诊断」, logging design「对其他 crate
//! 的要求」): a run at trace level with a canary in everything that must not
//! be logged (model names, message, request and turn IDs, command text,
//! limit IDs, statusline JSON); paths may be logged, so the canary stays out
//! of them.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lectern_services::session::{
    Agent, EditOp, FileEdit, RateLimitSample, RateWindow, Record, Timestamp, ToolCallId,
    ToolFinished, ToolKind, ToolStarted, ToolStatus, TurnId, UsageKey,
};
use lectern_services::usage::{ForgeRepo, ProjectRef, UsageCommand, UsageQuery, Window};

use super::limits::write_statusline;
use super::plugin::Rig;
use super::support::*;
use crate::engine::{FactsReader, PACE};
use crate::identity::RepoFacts;
use crate::testing::FakeSessions;

const CANARY: &str = "c4n4ry-9f1e";

#[test]
fn log_canary_never_reaches_a_record() {
    let capture = lectern_logging::testing::capture();
    let fake = FakeSessions::new("session-claude-code", Agent::ClaudeCode);
    let now = OCT_20 + 2 * DAY + 12 * HOUR;
    for (id, age) in [("live", 60_000), ("old", 2 * DAY)] {
        let at = now - age;
        fake.add(id, Path::new("/work/app"), at, None);
        let turn = format!("turn-{CANARY}");
        fake.push(
            id,
            vec![
                context(0, at, "/work/app"),
                rec(
                    100,
                    Record::TurnStarted {
                        turn: TurnId(turn.as_str().into()),
                        at: Timestamp(at),
                    },
                ),
                rec(
                    200,
                    Record::ToolStarted(ToolStarted {
                        call: ToolCallId(CANARY.into()),
                        turn: Some(TurnId(turn.as_str().into())),
                        at: Timestamp(at),
                        name: CANARY.into(),
                        kind: ToolKind::Execute {
                            command: format!("echo {CANARY}").into(),
                            workdir: Arc::from(Path::new("/work/app")),
                            exec_session: None,
                            workdir_exact: true,
                        },
                    }),
                ),
                rec(
                    250,
                    Record::ToolStarted(ToolStarted {
                        call: ToolCallId(format!("edit-{CANARY}").into()),
                        turn: Some(TurnId(turn.as_str().into())),
                        at: Timestamp(at),
                        name: "Edit".into(),
                        kind: ToolKind::Edit {
                            paths: vec![PathBuf::from("/work/other/f.rs")].into(),
                        },
                    }),
                ),
                rec(
                    260,
                    Record::ToolFinished(ToolFinished {
                        call: ToolCallId(format!("edit-{CANARY}").into()),
                        at: Timestamp(at),
                        status: ToolStatus::Completed,
                        exec_session: None,
                    }),
                ),
                rec(
                    300,
                    Record::ToolFinished(ToolFinished {
                        call: ToolCallId(CANARY.into()),
                        at: Timestamp(at),
                        status: ToolStatus::Completed,
                        exec_session: None,
                    }),
                ),
                rec(
                    350,
                    Record::RateLimits(RateLimitSample {
                        at: Timestamp(at),
                        limit_id: Some(format!("limit-{CANARY}").into()),
                        plan_type: Some(format!("plan-{CANARY}").into()),
                        primary: Some(RateWindow {
                            used_percent: 12.5,
                            window_minutes: Some(300),
                            resets_at: Some(Timestamp(at + HOUR)),
                        }),
                        secondary: None,
                    }),
                ),
                rec(
                    400,
                    Record::Usage(usage(
                        at,
                        Some(&turn),
                        &format!("model-{CANARY}"),
                        UsageKey::Claude {
                            message_id: format!("msg-{CANARY}-{id}").into(),
                            request_id: Some(format!("req-{CANARY}").into()),
                        },
                        anthropic([1, 2, 3, 4]),
                    )),
                ),
            ],
        );
    }
    // The text an edit wrote is read when its turn is settled; it carries the
    // canary too.
    for id in ["live", "old"] {
        let path = PathBuf::from("/work/other/f.rs");
        fake.edit_call(
            id,
            &format!("turn-{CANARY}"),
            &format!("edit-{CANARY}"),
            std::slice::from_ref(&path),
            vec![FileEdit {
                path: path.clone(),
                op: EditOp::Write {
                    content: format!("secret {CANARY}\n").into(),
                    created: true,
                },
            }],
        );
    }
    // What identifies the repositories carries the canary too: roots,
    // remote addresses, the primary remote's name and the platform's ID.
    // One repository cannot be read at all, with the canary in the error.
    let facts: FactsReader = Arc::new(|dir, _, _| {
        if dir.ends_with("lib/.git") {
            return Err(lectern_git::GitError::Corrupt(format!("broken {CANARY}")));
        }
        Ok(RepoFacts {
            roots: [format!("root-{CANARY}")].into(),
            remotes: [format!("github.com/{CANARY}/app")].into(),
            primary: Some(format!("github.com/{CANARY}/app")),
            primary_name: Some(CANARY.to_owned()),
            forge: Some(ForgeRepo {
                host: format!("host-{CANARY}").into(),
                id: CANARY.into(),
            }),
            unborn: false,
            tip: Some(format!("tip-{CANARY}")),
        })
    });
    let rig = Rig::with_facts(
        fake,
        |p| {
            for root in ["app", "lib", "other"] {
                let root = Path::new("/work").join(root);
                let key = p.repo(&root, &root.join(".git"));
                p.set_open(&key, true);
            }
        },
        facts,
    );
    let query = UsageQuery::Project {
        project: ProjectRef::Open(lectern_services::project::ProjectKey::Git {
            common_dir: Path::new("/work/app/.git").into(),
        }),
        window: Window::D7,
    };
    let _interest = rig.usage().watch(query.clone());
    rig.settle();
    // Backfill, a replay and a removed session, all at trace level.
    rig.advance(PACE + Duration::from_millis(1));
    rig.sessions.replace("live", Vec::new());
    rig.emit_changed(&["live"]);
    rig.sessions.remove("old");
    rig.emit_changed(&["old"]);
    let _ = rig.usage().snapshot(&query);
    // The statusline file: a good one, one with a value of the wrong type
    // and one that is not JSON, each with the canary in it. The parser's
    // own messages quote values; none of them is logged.
    let modified = OCT_20 + 2 * DAY + 11 * HOUR;
    let good = format!(
        r#"{{"model": {{"display_name": "{CANARY}"}}, "cwd": "/work/{CANARY}",
            "rate_limits": {{"five_hour": {{"used_percentage": 3, "resets_at": 1792680000}}}}}}"#
    );
    write_statusline(&rig, &good, modified);
    let wrong = format!(r#"{{"rate_limits": {{"five_hour": {{"used_percentage": "{CANARY}"}}}}}}"#);
    write_statusline(&rig, &wrong, modified + 1000);
    write_statusline(&rig, &format!("not json {CANARY}"), modified + 2000);
    write_statusline(&rig, &format!("[\"{CANARY}\"]"), modified + 3000);
    assert_eq!(
        rig.usage().limits().windows.len(),
        2,
        "both samples came in"
    );
    // The Usage page's commands: a merge, a refused one and an undo.
    let all = rig.all();
    let ids: Vec<_> = all.projects.iter().map(|(p, _)| p.id).collect();
    assert!(ids.len() >= 3, "the three repositories are listed");
    rig.usage().command(UsageCommand::Merge {
        from: ids[0],
        into: ids[1],
    });
    rig.settle();
    rig.usage().command(UsageCommand::Merge {
        from: ids[1],
        into: ids[0],
    });
    rig.settle();
    let merged = rig.all();
    if let Some(info) = merged.merged.first()
        && let lectern_services::usage::ProjectStatus::Merged { alias, .. } = info.status
    {
        rig.usage().command(UsageCommand::Undo { alias });
        rig.settle();
    }
    assert!(!capture.lines().is_empty(), "the run logged something");
    let logged = capture.lines();
    for expected in [
        "project identified",
        "repository facts could not be read",
        "project command applied",
        "project command rejected",
        "turns settled",
        "turn settled",
    ] {
        assert!(
            logged.iter().any(|l| l.contains(expected)),
            "no record says {expected:?}"
        );
    }
    assert!(
        capture
            .lines()
            .iter()
            .any(|l| l.contains("statusline file ignored")),
        "the unusable files were counted"
    );
    capture.assert_absent(CANARY);
}
