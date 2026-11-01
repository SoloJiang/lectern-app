//! The ACP client against the fake agent: starting the process, the handshake, a turn, the live
//! entries, and what is refused or fails (docs/design/agents.md「ACP 客户端」的「测试」).

mod common;

use std::time::Duration;

use common::*;
use lectern_services::conversation::{
    CallKind, CallState, ContextUsage, ConversationCommand, EntryBody, Link, LinkError, StepState,
    Unavailable,
};
use lectern_services::project::{Location, PackageMatch};
use lectern_services::session::Agent;

fn live_keys(snapshot: &lectern_services::conversation::ConversationSnapshot) -> Vec<String> {
    snapshot
        .entries
        .iter()
        .map(|e| e.key.0.to_string())
        .filter(|key| key.starts_with("l:"))
        .collect()
}

#[test]
fn acp_nothing_starts_until_a_prompt_is_sent() {
    let h = Harness::new("basic.jsonl");
    let session = h.session(1);
    h.watch(&session);
    h.clock.advance(Duration::from_secs(3600));
    std::thread::sleep(Duration::from_millis(100));
    assert!(h.records().is_empty(), "watching starts no agent");
    assert_eq!(h.snapshot(&session).link, Link::None);
    assert_eq!(h.snapshot(&session).can_prompt, Ok(()));
}

#[test]
fn acp_starts_the_adapter_without_arguments_and_declares_no_client_capabilities() {
    let h = Harness::new("basic.jsonl");
    let session = h.session(1);
    h.watch(&session);
    h.turn(&session, "do the thing");
    let records = h.records();
    assert_eq!(records.len(), 1);
    let record = &records[0];

    // The program, its arguments, directory and environment.
    assert!(
        record.args().is_empty(),
        "no arguments: {:?}",
        record.args()
    );
    assert_eq!(record.cwd(), h.project.to_str());
    assert!(record.has_env("PATH"));
    assert!(record.has_env("HOME"));
    assert!(
        !record.has_env("LECTERN_TOKEN"),
        "LECTERN_ variables stay out of the agent"
    );
    assert_eq!(record.var("PWD"), h.project.to_str());

    // The handshake.
    assert_eq!(
        record.methods(),
        ["initialize", "session/resume", "session/prompt"]
    );
    let initialize = &record.messages[0];
    assert_eq!(initialize["params"]["protocolVersion"], 1);
    assert_eq!(
        initialize["params"]["clientCapabilities"],
        serde_json::json!({}),
        "no fs, no terminal"
    );
    assert!(!record.lines[0].contains("\"fs\"") && !record.lines[0].contains("terminal"));
    let resume = &record.messages[1];
    assert_eq!(resume["params"]["sessionId"], &*session.id);
    assert_eq!(resume["params"]["cwd"], h.project.to_str().unwrap());
    assert_eq!(resume["params"]["mcpServers"], serde_json::json!([]));
    let prompt = &record.messages[2];
    assert_eq!(prompt["params"]["sessionId"], &*session.id);
    assert_eq!(
        prompt["params"]["prompt"],
        serde_json::json!([{"type": "text", "text": "do the thing"}])
    );
}

#[test]
fn acp_a_turn_becomes_live_entries_a_plan_and_the_context_usage() {
    let h = Harness::new("basic.jsonl");
    let session = h.session(1);
    h.watch(&session);
    let snapshot = h.turn(&session, "do the thing");

    assert_eq!(snapshot.link, Link::Idle);
    assert_eq!(snapshot.can_prompt, Ok(()));
    assert_eq!(
        snapshot
            .entries
            .iter()
            .map(|e| &*e.key.0)
            .collect::<Vec<_>>(),
        [
            "h:1:0", "h:1:1", "l:0:0", "l:0:1", "l:0:2", "l:0:3", "l:0:4"
        ],
        "history first, then this run's prompt and its reply"
    );
    let bodies: Vec<&EntryBody> = snapshot.entries[2..].iter().map(|e| &e.body).collect();
    assert!(matches!(bodies[0], EntryBody::Prompt { text, .. } if &*text.text == "do the thing"));
    assert!(
        matches!(bodies[1], EntryBody::AgentText { text, streaming: false } if &*text.text == "Reading ")
    );
    assert!(matches!(
        bodies[2],
        EntryBody::Tool(tool) if tool.kind == CallKind::Read
            && tool.state == CallState::Completed
            && &*tool.summary == "main.rs"
    ));
    assert!(matches!(
        bodies[3],
        EntryBody::Tool(tool) if tool.kind == CallKind::Execute
            && tool.state == CallState::Completed
            && &*tool.summary == "cargo test"
    ));
    assert!(
        matches!(bodies[4], EntryBody::AgentText { text, streaming: false } if &*text.text == "Hello world"),
        "chunks join into one passage"
    );
    let plan = snapshot.plan.as_ref().expect("a plan");
    assert_eq!(plan.len(), 2);
    assert_eq!(
        (&*plan[0].text, plan[0].state),
        ("Look at the code", StepState::Completed)
    );
    assert_eq!(plan[1].state, StepState::InProgress);
    assert_eq!(
        snapshot.context,
        Some(ContextUsage {
            used: 1200,
            size: 200_000
        })
    );

    // A second prompt goes to the same process.
    let second = h.turn(&session, "and again");
    assert_eq!(h.records().len(), 1, "the connection is reused");
    assert_eq!(h.records()[0].calls("session/prompt").len(), 2);
    assert_eq!(
        live_keys(&second),
        [
            "l:0:0", "l:0:1", "l:0:2", "l:0:3", "l:0:4", "l:1:0", "l:1:1"
        ]
    );
    assert!(matches!(
        &second.entries.last().unwrap().body,
        EntryBody::AgentText { text, .. } if &*text.text == "second answer"
    ));
}

#[test]
fn acp_updates_reach_the_snapshot_at_most_every_50_ms() {
    let h = Harness::new("hold.jsonl");
    let session = h.session(1);
    h.watch(&session);
    h.prompt(&session, "start");
    let working = h.wait_link(&session, Link::Working);
    // The agent sends its first chunk right after the prompt. The clock does not move, so the chunk
    // waits for the next swap.
    let record = h.record_of(&session);
    wait_for("the prompt to arrive", || {
        (!record_calls(&h, record.pid).is_empty()).then_some(())
    });
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(h.snapshot(&session).generation, working.generation);
    // 50 ms later it is in.
    let snapshot = wait_for("the chunk", || {
        h.clock.advance(Duration::from_millis(50));
        let snapshot = h.snapshot(&session);
        (snapshot.generation > working.generation).then_some(snapshot)
    });
    assert!(matches!(
        &snapshot.entries.last().unwrap().body,
        EntryBody::AgentText { text, streaming: true } if &*text.text == "working"
    ));
    assert_eq!(snapshot.link, Link::Working);
}

fn record_calls(h: &Harness, pid: u32) -> Vec<String> {
    h.records()
        .into_iter()
        .find(|r| r.pid == pid)
        .map(|r| {
            r.calls("session/prompt")
                .iter()
                .map(|m| m.to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn acp_session_load_drops_the_replay() {
    let h = Harness::new("load.jsonl");
    let session = h.session(1);
    h.watch(&session);
    let snapshot = h.turn(&session, "go on");
    let record = &h.records()[0];
    assert_eq!(
        record.methods(),
        ["initialize", "session/load", "session/prompt"],
        "an adapter without resume is asked to load"
    );
    // The old prompt and reply the adapter replayed are neither entries nor context.
    assert_eq!(live_keys(&snapshot), ["l:0:0", "l:0:1"]);
    assert!(matches!(
        &snapshot.entries.last().unwrap().body,
        EntryBody::AgentText { text, .. } if &*text.text == "fresh answer"
    ));
    assert_eq!(snapshot.context, None);
}

#[test]
fn acp_codex_sessions_use_the_codex_program() {
    let h = Harness::new("basic.jsonl");
    let session = h.codex_session(1);
    h.watch(&session);
    h.turn(&session, "hello");
    let record = &h.records()[0];
    assert!(record.lines[0].contains("initialize"));
    assert_eq!(
        h.snapshot(&session).agent,
        Agent::Codex,
        "the agent is the source's"
    );
}

#[test]
fn acp_requests_for_capabilities_it_did_not_declare_get_method_not_found() {
    let h = Harness::new("client_requests.jsonl");
    let session = h.session(1);
    h.watch(&session);
    let snapshot = h.turn(&session, "go");
    assert!(matches!(
        &snapshot.entries.last().unwrap().body,
        EntryBody::AgentText { text, .. } if &*text.text == "no client capabilities used"
    ));
    let record = &h.records()[0];
    for id in ["fs-1", "term-1"] {
        let response = record
            .response(id)
            .unwrap_or_else(|| panic!("{id} answered"));
        assert_eq!(response["error"]["code"], -32601, "{id}");
    }
}

#[test]
fn acp_refuses_sessions_it_must_not_resume_and_starts_nothing() {
    let h = Harness::new("basic.jsonl");

    // A sub-agent session, a source command that is missing, an ID that is not a canonical UUID, a
    // session placed outside the open projects.
    let child = h.session(1);
    h.claude.set_spec(&child, |spec| spec.parent = true);
    let no_command = h.session(2);
    h.claude.set_spec(&no_command, |spec| spec.resume = None);
    let outside = h.session(3);
    h.claude.set_spec(&outside, |spec| {
        spec.place = lectern_services::session::Place::Pending;
    });
    let not_a_uuid =
        lectern_agents::testing::acp::key(&lectern_agents::testing::acp::claude_source(), "--help");
    h.claude.add(
        &not_a_uuid,
        lectern_agents::testing::acp::Spec {
            cwd: h.project.clone(),
            ..lectern_agents::testing::acp::Spec::located(
                &not_a_uuid,
                h.project_key.clone(),
                &Agent::ClaudeCode,
            )
        },
    );

    let expected = [
        (&child, Unavailable::NotResumable),
        (&no_command, Unavailable::NotResumable),
        (&outside, Unavailable::OutsideProject),
        (&not_a_uuid, Unavailable::NotResumable),
    ];
    for (session, why) in expected {
        h.watch(session);
        let before = h.snapshot(session);
        assert_eq!(before.can_prompt, Err(why.clone()));
        h.prompt(session, "hello");
        let after = h.snapshot(session);
        assert_eq!(after.link, Link::None, "{why:?}");
        assert_eq!(after.generation, before.generation, "{why:?}");
    }
    std::thread::sleep(Duration::from_millis(200));
    assert!(h.records().is_empty(), "no process for any of them");
}

#[test]
fn acp_checks_the_working_directory_again_when_it_starts() {
    let h = Harness::new("basic.jsonl");
    // The directory is gone.
    let missing = h.session(1);
    h.claude
        .set_spec(&missing, |spec| spec.cwd = h.root.join("gone"));
    h.watch(&missing);
    h.prompt(&missing, "hello");
    h.wait_link(&missing, Link::Failed(LinkError::Spawn));

    // The directory is not in the project the session is placed in.
    let elsewhere = h.session(2);
    h.projects.set_location(Some(Location {
        project: lectern_agents::testing::acp::folder("/somewhere/else"),
        worktree: None,
        rel: "".into(),
        package: PackageMatch::NotLoaded,
    }));
    h.watch(&elsewhere);
    h.prompt(&elsewhere, "hello");
    let failed = h.wait_link(&elsewhere, Link::Failed(LinkError::Spawn));
    assert!(
        matches!(
            &failed.entries.last().unwrap().body,
            EntryBody::Prompt { .. }
        ),
        "the prompt stays in the list"
    );
    std::thread::sleep(Duration::from_millis(100));
    assert!(h.records().is_empty(), "no process was started");

    // A failure is cleared by the next prompt.
    h.projects.set_location(Some(Location {
        project: h.project_key.clone(),
        worktree: None,
        rel: "".into(),
        package: PackageMatch::NotLoaded,
    }));
    h.turn(&elsewhere, "try again");
    assert_eq!(h.records().len(), 1);
}

/// A script that makes the connection fail, and how.
fn fails(script: &str, link: Link) -> (Harness, lectern_services::session::SessionKey) {
    let h = Harness::new(script);
    let session = h.session(1);
    h.watch(&session);
    h.prompt(&session, "hello");
    h.wait_link(&session, link);
    (h, session)
}

fn ends(h: &Harness) {
    let records = h.records();
    assert_eq!(records.len(), 1);
    wait_dead(records[0].pid);
}

#[test]
fn acp_a_protocol_version_other_than_one_makes_the_adapter_unusable() {
    let (h, session) = fails("version2.jsonl", Link::Failed(LinkError::Unsupported));
    ends(&h);
    assert_eq!(
        h.snapshot(&session).can_prompt,
        Err(Unavailable::Unsupported),
        "the last negotiated version is not 1"
    );
    // A prompt is refused without starting another process.
    h.prompt(&session, "again");
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(h.records().len(), 1);
}

#[test]
fn acp_an_adapter_that_cannot_resume_or_load_is_unsupported_for_this_session() {
    let (h, session) = fails("no_restore.jsonl", Link::Failed(LinkError::Unsupported));
    ends(&h);
    assert_eq!(h.snapshot(&session).can_prompt, Ok(()));
}

#[test]
fn acp_resume_errors_are_told_apart() {
    let (h, _) = fails("auth.jsonl", Link::Failed(LinkError::AuthRequired));
    ends(&h);
    let (h, _) = fails("load_error.jsonl", Link::Failed(LinkError::LoadFailed));
    ends(&h);
}

#[test]
fn acp_a_process_that_exits_while_connecting_is_reported_with_its_code() {
    let (h, _) = fails(
        "exit_on_initialize.jsonl",
        Link::Failed(LinkError::Exited { code: Some(3) }),
    );
    ends(&h);
}

#[test]
fn acp_a_line_that_is_not_json_closes_the_connection() {
    let (h, _) = fails("garbage.jsonl", Link::Failed(LinkError::Protocol));
    ends(&h);
}

#[test]
fn acp_a_line_over_16_mib_closes_the_connection() {
    let (h, session) = fails("oversize.jsonl", Link::Failed(LinkError::Protocol));
    ends(&h);
    // Nothing of the line is kept.
    assert!(
        h.snapshot(&session).entries.iter().all(
            |e| !matches!(&e.body, EntryBody::AgentText { text, .. } if text.text.contains('x'))
        )
    );
}

#[test]
fn acp_a_handshake_that_takes_over_30_seconds_fails() {
    for script in ["silent.jsonl", "silent_restore.jsonl"] {
        let h = Harness::new(script);
        let session = h.session(1);
        h.watch(&session);
        h.prompt(&session, "hello");
        let record = h.started_pid();
        // Not yet.
        h.clock.advance(Duration::from_secs(29));
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(h.snapshot(&session).link, Link::Connecting, "{script}");
        h.clock.advance(Duration::from_secs(2));
        h.wait_link(&session, Link::Failed(LinkError::Protocol));
        wait_dead(record);
    }
}

#[test]
fn acp_a_failed_prompt_keeps_the_connection_and_the_next_prompt_uses_it() {
    let h = Harness::new("prompt_error.jsonl");
    let session = h.session(1);
    h.watch(&session);
    h.prompt(&session, "first");
    let failed = h.wait(&session, "a failed prompt", |s| {
        matches!(s.link, Link::Failed(_))
    });
    // Only the first line of the agent's message, which is for the screen and not for the logs.
    assert_eq!(
        failed.link,
        Link::Failed(LinkError::PromptFailed {
            message: "The service said no".into()
        })
    );
    let pid = h.records()[0].pid;
    assert!(alive(pid), "the process stays");
    assert_eq!(
        failed.can_prompt,
        Ok(()),
        "a failure does not block a prompt"
    );

    h.prompt(&session, "second");
    let snapshot = h.wait(&session, "the second turn", |s| s.link == Link::Idle);
    assert_eq!(h.records().len(), 1, "the same process");
    assert!(matches!(
        &snapshot.entries.last().unwrap().body,
        EntryBody::AgentText { text, .. } if &*text.text == "recovered"
    ));
}

#[test]
fn acp_dismiss_goes_back_to_idle_while_the_connection_lives() {
    let h = Harness::new("prompt_auth.jsonl");
    let session = h.session(1);
    h.watch(&session);
    h.prompt(&session, "hello");
    h.wait_link(&session, Link::Failed(LinkError::AuthRequired));
    let pid = h.records()[0].pid;
    h.command(&session, ConversationCommand::Dismiss);
    assert_eq!(h.snapshot(&session).link, Link::Idle);
    assert!(alive(pid));
}
