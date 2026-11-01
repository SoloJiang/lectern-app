//! The canary test for the live ACP paths (`logs_*`, docs/design/agents.md「ACP 客户端」的「日志与诊断」):
//! prompts, replies, tool titles, command texts and working directories, permission titles, option IDs,
//! the plan, stderr and the login environment must not reach the log. The in-crate canary test of the
//! same name covers the history window; this one needs the fake agent process, which only an
//! integration test can find. All data is synthetic.

mod common;

use std::time::Duration;

use common::*;
use lectern_logging::testing;
use lectern_services::conversation::{ConversationCommand, Link, OptionId};

const CANARY_PROMPT: &str = "canary-pr0mpt-91be42";
const CANARY_TOKEN: &str = "sk-ant-api03-Zq7XbR2mK9vT4wLp8NcY3hJd6FsA1gUe5oIkQ0rVxW-Bn2Ct7Mz4Ly9Pa";
const CANARY_VAR: &str = "CANARY_VAR_NAME_6b17e9";
const CANARY_VALUE: &str = "canary-value-0c4aa8";
/// In `tests/fixtures/acp/loud.jsonl`.
const AGENT_CANARIES: [&str; 8] = [
    "CANARY-STDERR-TEXT",
    "CANARY-REPLY-TEXT",
    "CANARY-TOOL-TITLE",
    "CANARY-COMMAND-TEXT",
    "CANARY-WORKDIR",
    "CANARY-PERMISSION-TITLE",
    "CANARY-OPTION-ID",
    "CANARY-PLAN-TEXT",
];

/// The names of the methods the log may carry (`wire::Method::name`).
const METHODS: [&str; 8] = [
    "initialize",
    "session/resume",
    "session/load",
    "session/prompt",
    "session/cancel",
    "session/update",
    "session/request_permission",
    "other",
];

#[test]
fn logs_hold_no_conversation_contents() {
    let log = testing::capture();
    let h = Harness::with_vars("loud.jsonl", vec![(CANARY_VAR, CANARY_VALUE)]);
    let session = h.session(1);
    h.watch(&session);

    // A turn with a request, answered with an option the agent named.
    h.prompt(&session, &format!("{CANARY_PROMPT} {CANARY_TOKEN}"));
    let waiting = h.wait(&session, "a request", |s| s.permissions.len() == 1);
    h.command(
        &session,
        ConversationCommand::Answer {
            request: waiting.permissions[0].id,
            option: OptionId("CANARY-OPTION-ID".into()),
        },
    );
    h.wait_link(&session, Link::Idle);

    // A second turn on the same connection, stopped while the request waits; then the clock closes it.
    h.prompt(&session, CANARY_PROMPT);
    h.wait(&session, "a request", |s| s.permissions.len() == 1);
    h.command(&session, ConversationCommand::Cancel);
    h.wait_link(&session, Link::Idle);
    h.clock.advance(Duration::from_secs(11 * 60));
    h.wait_link(&session, Link::None);
    wait_for("the exit record", || {
        log.lines()
            .iter()
            .any(|l| l.contains("\"msg\":\"acp process exited\""))
            .then_some(())
    });

    // A second connection, closed with the plugin.
    h.prompt(&session, CANARY_PROMPT);
    h.wait(&session, "a request", |s| s.permissions.len() == 1);
    h.kernel.deactivate();
    wait_for("both exit records", || {
        (log.lines()
            .iter()
            .filter(|l| l.contains("\"msg\":\"acp process exited\""))
            .count()
            == 2)
            .then_some(())
    });

    for canary in AGENT_CANARIES.iter().copied().chain([
        CANARY_PROMPT,
        CANARY_TOKEN,
        CANARY_VAR,
        CANARY_VALUE,
    ]) {
        log.assert_absent(canary);
    }

    // What is logged: the process, counts, kinds and timings, in the session's scope.
    let lines = log.lines();
    let find = |msg: &str| -> Vec<&String> {
        let needle = format!("\"msg\":\"{msg}\"");
        lines.iter().filter(|l| l.contains(&needle)).collect()
    };
    let started = find("acp process started");
    assert_eq!(started.len(), 2);
    for line in &started {
        for field in [
            "\"program\":\"claude-agent-acp\"",
            "\"argc\":0",
            "\"cwd\":",
            "\"pid\":",
            "\"session\":",
        ] {
            assert!(line.contains(field), "{field} in {line}");
        }
    }
    let exited = find("acp process exited");
    assert_eq!(exited.len(), 2);
    for line in &exited {
        for field in [
            "\"pid\":",
            "\"code\":",
            "\"stderr_bytes\":",
            "\"runtime_ms\":",
            "\"session\":",
        ] {
            assert!(line.contains(field), "{field} in {line}");
        }
    }
    assert!(
        exited.iter().any(|l| !l.contains("\"stderr_bytes\":0")),
        "the stderr of the first process was counted"
    );
    let closing = find("acp connection closing");
    assert!(closing.iter().any(|l| l.contains("\"reason\":\"idle\"")));

    // JSON-RPC: only method names from the fixed table, with a request number and byte counts.
    let sent = find("acp message sent");
    assert!(sent.len() >= 8, "{} sent messages", sent.len());
    let responses = find("acp response received");
    assert!(!responses.is_empty());
    for line in sent
        .iter()
        .chain(&responses)
        .chain(&find("acp message received"))
    {
        let method = line
            .split("\"method\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .unwrap_or_else(|| panic!("a method in {line}"));
        assert!(METHODS.contains(&method), "{method} in {line}");
        assert!(line.contains("\"bytes\":"), "{line}");
    }
    assert!(
        sent.iter()
            .any(|l| l.contains("\"method\":\"session/prompt\"") && l.contains("\"req\":")),
        "a prompt carries its request number"
    );
    assert!(
        responses
            .iter()
            .all(|l| l.contains("\"took_us\":") && l.contains("\"ok\":") && l.contains("\"req\":"))
    );
    assert!(!find("acp turn finished").is_empty());
}
