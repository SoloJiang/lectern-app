//! `log_*` (docs/design/usage.md「日志与诊断」, logging design「对其他 crate
//! 的要求」): a run at trace level with a canary in everything that must not
//! be logged (model names, message, request and turn IDs, command text,
//! limit IDs, statusline JSON); paths may be logged, so the canary stays out
//! of them.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use lectern_services::session::{
    Agent, RateLimitSample, RateWindow, Record, Timestamp, ToolCallId, ToolFinished, ToolKind,
    ToolStarted, ToolStatus, TurnId, UsageKey,
};
use lectern_services::usage::{ProjectRef, UsageQuery, Window};

use super::limits::write_statusline;
use super::plugin::Rig;
use super::support::*;
use crate::engine::PACE;
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
    let rig = Rig::new(fake, |p| {
        let key = p.repo(Path::new("/work/app"), Path::new("/work/app/.git"));
        p.set_open(&key, true);
    });
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
    assert!(!capture.lines().is_empty(), "the run logged something");
    assert!(
        capture
            .lines()
            .iter()
            .any(|l| l.contains("statusline file ignored")),
        "the unusable files were counted"
    );
    capture.assert_absent(CANARY);
}
