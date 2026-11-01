//! Permission requests against the fake agent: they wait for the user, are answered with the option
//! chosen, `cancelled` on `Cancel` and when the connection closes (docs/design/agents.md「ACP 客户端」的
//! 「运行、关闭、权限」).

mod common;

use std::time::Duration;

use common::*;
use lectern_services::conversation::{
    CallKind, CallState, ConversationCommand, EndReason, EntryBody, Link, OptionId, OptionKind,
    PermissionDetail, PermissionId, PermissionRequest,
};
use lectern_services::session::SessionKey;

/// Runs `script` until its first request is waiting.
fn waiting(script: &str) -> (Harness, SessionKey, PermissionRequest) {
    let h = Harness::new(script);
    let session = h.session(1);
    h.watch(&session);
    h.prompt(&session, "run it");
    let snapshot = h.wait(&session, "a request", |s| s.permissions.len() == 1);
    let request = snapshot.permissions[0].clone();
    (h, session, request)
}

fn answer(h: &Harness, session: &SessionKey, request: &PermissionRequest, option: &str) {
    h.command(
        session,
        ConversationCommand::Answer {
            request: request.id,
            option: OptionId(option.into()),
        },
    );
}

#[test]
fn acp_a_command_request_shows_the_command_and_waits_without_an_answer() {
    let (h, session, request) = waiting("permission.jsonl");
    let snapshot = h.snapshot(&session);
    assert_eq!(
        snapshot.link,
        Link::Working,
        "a request means a running turn"
    );
    assert_eq!(request.kind, CallKind::Execute);
    assert_eq!(&*request.title, "Run make test");
    let PermissionDetail::Command { text, workdir } = &request.detail else {
        panic!("a command: {:?}", request.detail);
    };
    assert_eq!(&**text, "make test");
    assert_eq!(
        workdir.as_deref().and_then(|p| p.to_str()),
        Some("/work/project/sub")
    );
    // Options: one per kind in the order of the buttons; the second "allow once" is dropped.
    let options: Vec<(&str, OptionKind)> =
        request.options.iter().map(|o| (&*o.id.0, o.kind)).collect();
    assert_eq!(
        options,
        [
            ("allow", OptionKind::AllowOnce),
            ("always", OptionKind::AllowAlways),
            ("deny", OptionKind::RejectOnce),
            ("never", OptionKind::RejectAlways)
        ]
    );
    assert!(matches!(
        &snapshot.entries.last().unwrap().body,
        EntryBody::Tool(tool) if tool.state == CallState::Pending
    ));

    // Nothing answers it: not time, not a closed inspector.
    h.unwatch_all();
    assert!(
        h.conversations.snapshot(&session).is_some(),
        "a connection keeps its state without a watcher"
    );
    h.clock.advance(Duration::from_secs(11 * 60));
    std::thread::sleep(Duration::from_millis(100));
    let later = h.snapshot(&session);
    assert_eq!(later.permissions.len(), 1);
    assert_eq!(later.link, Link::Working);
    assert!(alive(h.records()[0].pid), "a waiting request is not idle");
    let record = &h.records()[0];
    assert!(record.response("perm-1").is_none(), "no automatic answer");
}

#[test]
fn acp_an_answer_is_sent_with_the_chosen_option_and_the_turn_goes_on() {
    let (h, session, request) = waiting("permission.jsonl");
    answer(&h, &session, &request, "always");
    // The card is gone before `command` returned.
    assert!(h.snapshot(&session).permissions.is_empty());
    let done = h.wait(&session, "the turn to end", |s| s.link == Link::Idle);
    let response = h.records()[0].response("perm-1").unwrap().clone();
    assert_eq!(
        response["result"]["outcome"],
        serde_json::json!({"outcome": "selected", "optionId": "always"})
    );
    let texts: Vec<String> = done
        .entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::AgentText { text, .. } => Some(text.text.to_string()),
            _ => None,
        })
        .collect();
    assert!(texts.contains(&"Done.".to_owned()), "{texts:?}");
    assert!(done.entries.iter().any(|e| matches!(
        &e.body,
        EntryBody::Tool(tool) if tool.state == CallState::Completed
    )));
}

#[test]
fn acp_a_rejected_request_marks_the_call_denied() {
    let (h, session, request) = waiting("permission_deny.jsonl");
    answer(&h, &session, &request, "deny");
    let done = h.wait(&session, "the turn to end", |s| s.link == Link::Idle);
    let tool = done
        .entries
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::Tool(tool) => Some(tool),
            _ => None,
        })
        .expect("the tool call");
    assert_eq!(tool.state, CallState::Denied);
    assert_eq!(
        h.records()[0].response("perm-1").unwrap()["result"]["outcome"]["optionId"],
        "deny"
    );
}

#[test]
fn acp_answers_that_do_not_fit_are_ignored() {
    let (h, session, request) = waiting("permission.jsonl");
    let before = h.snapshot(&session);
    // A request that is not waiting, an option the request did not offer.
    h.command(
        &session,
        ConversationCommand::Answer {
            request: PermissionId(request.id.0 + 100),
            option: OptionId("allow".into()),
        },
    );
    answer(&h, &session, &request, "allow-2");
    answer(&h, &session, &request, "not-an-option");
    assert_eq!(h.snapshot(&session), before);
    // Disconnect does not apply to a running turn either.
    h.command(&session, ConversationCommand::Disconnect);
    assert_eq!(h.snapshot(&session), before);
    assert!(alive(h.records()[0].pid));
}

#[test]
fn acp_cancel_answers_the_waiting_request_cancelled_and_ends_the_turn() {
    let (h, session, _request) = waiting("permission_cancel.jsonl");
    h.command(&session, ConversationCommand::Cancel);
    // Before `command` returned: the card is gone and the link says why the turn is still open.
    let stopping = h.snapshot(&session);
    assert_eq!(stopping.link, Link::Stopping);
    assert!(stopping.permissions.is_empty());

    let done = h.wait(&session, "the turn to end", |s| s.link == Link::Idle);
    let record = &h.records()[0];
    assert_eq!(
        record.response("perm-1").unwrap()["result"]["outcome"],
        serde_json::json!({"outcome": "cancelled"})
    );
    let cancels = record.calls("session/cancel");
    assert_eq!(cancels.len(), 1);
    assert_eq!(cancels[0]["params"]["sessionId"], &*session.id);
    assert!(
        cancels[0].get("id").is_none(),
        "session/cancel is a notification"
    );
    assert!(matches!(
        &done.entries.last().unwrap().body,
        EntryBody::Ended(EndReason::Cancelled)
    ));
    // A second Cancel with nothing running does nothing.
    let before = h.snapshot(&session);
    h.command(&session, ConversationCommand::Cancel);
    assert_eq!(h.snapshot(&session), before);
}

#[test]
fn acp_closing_the_connection_answers_waiting_requests_cancelled() {
    // The adapter ignores SIGTERM, so it is still there to read what Lectern writes before it closes.
    let (h, session, _request) = waiting("permission_stubborn.jsonl");
    let pid = h.records()[0].pid;
    h.close_project();
    h.wait(&session, "the connection to close", |s| {
        s.link == Link::None && s.permissions.is_empty()
    });
    let response = wait_for("the cancelled answer", || {
        h.records()[0].response("perm-1").cloned()
    });
    assert_eq!(
        response["result"]["outcome"],
        serde_json::json!({"outcome": "cancelled"})
    );
    wait_for("the process to be killed", || {
        h.clock.advance(Duration::from_secs(2));
        (!alive(pid)).then_some(())
    });
}

#[test]
fn acp_requests_it_cannot_show_are_answered_cancelled_and_edits_have_no_preview_yet() {
    let (h, session, request) = waiting("permission_odd.jsonl");
    // Only the edit request is waiting; the one without a usable option and the one for another session
    // were answered `cancelled` without bothering the user.
    assert_eq!(request.kind, CallKind::Edit);
    assert_eq!(&*request.title, "Edit a file");
    assert_eq!(
        request.detail,
        PermissionDetail::None,
        "the diff preview is the next step"
    );
    let record = &h.records()[0];
    for id in ["perm-1", "perm-2"] {
        assert_eq!(
            record.response(id).unwrap()["result"]["outcome"],
            serde_json::json!({"outcome": "cancelled"}),
            "{id}"
        );
    }
    answer(&h, &session, &request, "ok");
    h.wait(&session, "the turn to end", |s| s.link == Link::Idle);
    assert_eq!(
        h.records()[0].response("perm-3").unwrap()["result"]["outcome"]["optionId"],
        "ok"
    );
    assert!(h.conversations.waiting().is_empty());
}

#[test]
fn acp_a_request_still_open_when_the_turn_ends_is_answered_cancelled() {
    let h = Harness::new("permission_left_open.jsonl");
    let session = h.session(1);
    h.watch(&session);
    // The first turn ends with a result, the second with an error; the adapter's request is open both
    // times, and it is answered either way.
    for (turn, request, ends_with_error) in [("first", "perm-1", false), ("second", "perm-2", true)]
    {
        h.prompt(&session, turn);
        let snapshot = h.wait(&session, "the turn to end", |s| {
            s.entries
                .iter()
                .any(|e| matches!(&e.body, EntryBody::Prompt { text, .. } if &*text.text == turn))
                && matches!(s.link, Link::Idle | Link::Failed(_))
        });
        assert!(snapshot.permissions.is_empty());
        assert_eq!(
            matches!(snapshot.link, Link::Failed(_)),
            ends_with_error,
            "{turn}"
        );
        let response = wait_for("the answer", || h.records()[0].response(request).cloned());
        assert_eq!(
            response["result"]["outcome"],
            serde_json::json!({"outcome": "cancelled"}),
            "{request}"
        );
    }
    assert_eq!(h.records().len(), 1, "the connection stays");
}
