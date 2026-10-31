//! 运行日志（`logs_*`，docs/design/agents.md「ACP 客户端」的「日志与诊断」）：提示词、agent 说明、
//! 命令、文件名、环境变量的名字与值、会话的 cwd 都不进日志。全部是合成数据。

use std::path::PathBuf;

use lectern_logging::testing;
use lectern_services::conversation::{ConversationCommand, Conversations, PromptOrigin};
use lectern_services::session::{Agent, ToolKind, ToolStatus};

use super::support::*;

const CANARY_PROMPT: &str = "canary-pr0mpt-91be42";
const CANARY_REPLY: &str = "canary-reply-5d3c1a";
const CANARY_COMMAND: &str = "canary-c0mmand-7e20f4";
const CANARY_FILE: &str = "canary-file-a81b9c.rs";
const CANARY_CWD: &str = "canary-cwd-33f0d2";
const CANARY_VAR: &str = "CANARY_VAR_NAME_6b17e9";
const CANARY_VALUE: &str = "canary-value-0c4aa8";
const CANARY_TOKEN: &str = "sk-ant-api03-Zq7XbR2mK9vT4wLp8NcY3hJd6FsA1gUe5oIkQ0rVxW-Bn2Ct7Mz4Ly9Pa";

#[test]
fn logs_hold_no_conversation_contents() {
    let log = testing::capture();
    let dir = tempfile::tempdir().unwrap();
    install(dir.path(), "claude-agent-acp");
    // The lookup reads these; neither the names nor the values may be logged.
    let rig = Rig::with_vars(vec![
        ("PATH".into(), dir.path().into()),
        (CANARY_VAR.into(), CANARY_VALUE.into()),
    ]);
    let session = rig.claude_session();
    rig.claude.set_spec(&session, |spec| {
        spec.cwd = PathBuf::from(format!("/work/{CANARY_CWD}"));
    });
    let s = &rig.claude;
    let turns: Vec<_> = (1..=30)
        .map(|n| {
            s.turn(
                n,
                &format!("{CANARY_PROMPT} {CANARY_TOKEN}"),
                vec![
                    s.agent_text(&format!("{CANARY_REPLY} {CANARY_TOKEN}")),
                    s.tool(
                        "t1",
                        "Edit",
                        ToolKind::Edit {
                            paths: vec![PathBuf::from(format!("/work/{CANARY_FILE}"))].into(),
                        },
                        Some(ToolStatus::Completed),
                    ),
                    s.tool(
                        "t2",
                        "Bash",
                        ToolKind::Execute {
                            command: CANARY_COMMAND.into(),
                            workdir: PathBuf::from(format!("/work/{CANARY_CWD}")).into(),
                            exec_session: None,
                            workdir_exact: true,
                        },
                        Some(ToolStatus::Failed),
                    ),
                ],
            )
        })
        .collect();
    s.set_turns(&session, turns);

    let interest = rig.engine.watch(&session);
    rig.engine
        .command(&session, ConversationCommand::LoadEarlier);
    rig.engine.command(
        &session,
        ConversationCommand::Prompt {
            text: format!("{CANARY_PROMPT} {CANARY_TOKEN}").into(),
            origin: PromptOrigin::User,
        },
    );
    rig.engine
        .on_sessions_changed(&changed(&claude_source(), &session));
    // A session whose source is gone, and a missing program.
    let gone = key(
        &lectern_services::session::SourceId("session-gone".into()),
        CLAUDE_SESSION,
    );
    let _gone = rig.engine.watch(&gone);
    assert_eq!(
        rig.engine.snapshot(&session).unwrap().agent,
        Agent::ClaudeCode
    );
    drop(interest);

    for canary in [
        CANARY_PROMPT,
        CANARY_REPLY,
        CANARY_COMMAND,
        CANARY_FILE,
        CANARY_CWD,
        CANARY_VAR,
        CANARY_VALUE,
        CANARY_TOKEN,
    ] {
        log.assert_absent(canary);
    }

    // What is logged: counts, kinds and timings, in the session's scope.
    let lines = log.lines();
    let read: Vec<_> = lines
        .iter()
        .filter(|l| l.contains("\"msg\":\"conversation history read\""))
        .collect();
    assert!(!read.is_empty(), "{lines:?}");
    for line in read {
        for field in [
            "\"turns\":",
            "\"entries\":",
            "\"earlier\":",
            "\"took_us\":",
            "\"session\":",
        ] {
            assert!(line.contains(field), "{field} in {line}");
        }
    }
    assert!(lines.iter().any(|l| {
        l.contains("\"msg\":\"conversation command ignored\"")
            && l.contains("\"command\":\"prompt\"")
    }));
    assert!(lines.iter().any(|l| {
        l.contains("\"msg\":\"conversation program lookup\"")
            && l.contains("\"program\":\"claude_code\"")
    }));
    assert!(lines.iter().any(|l| {
        l.contains("\"msg\":\"conversation history failed\"")
            && l.contains("\"error\":\"not_found\"")
    }));
}
