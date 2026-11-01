//! Live items give way to the history (docs/design/agents.md「ACP 客户端」的「实时项换成历史项」): once
//! a turn is in the log and its `session/prompt` has returned, the live items of the turn are replaced by
//! the history items in one snapshot, and the turn never shows twice. The log is the fake source's turn
//! index, which the tests change by hand and announce with `SessionsChanged`.

mod common;

use std::time::{SystemTime, UNIX_EPOCH};

use common::*;
use lectern_services::conversation::{ConversationSnapshot, EntryBody};
use lectern_services::session::{SessionKey, SessionsChanged};

fn now_ms() -> i64 {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    i64::try_from(elapsed.as_millis()).unwrap()
}

/// The source reports the session changed.
fn announce(h: &Harness, session: &SessionKey) {
    h.kernel
        .inner()
        .expect("kernel")
        .emitter()
        .emit(SessionsChanged {
            source: session.source.clone(),
            generation: 2,
            changed: vec![session.clone()].into(),
            removed: std::sync::Arc::from([]),
        });
}

fn keys(snapshot: &ConversationSnapshot) -> Vec<String> {
    snapshot
        .entries
        .iter()
        .map(|e| e.key.0.to_string())
        .collect()
}

fn live_keys(snapshot: &ConversationSnapshot) -> usize {
    snapshot
        .entries
        .iter()
        .filter(|e| e.key.0.starts_with("l:"))
        .count()
}

/// The log after the user's turn: the history turn and one more, which began after the prompt was sent.
fn log_with_turn(h: &Harness, session: &SessionKey, prompt: &str) {
    let first = h.claude.turn(
        1,
        "history prompt",
        vec![h.claude.agent_text("history reply")],
    );
    let second = h.claude.turn_at(
        2,
        now_ms() + 1000,
        prompt,
        vec![h.claude.agent_text("Hello world")],
    );
    h.claude.set_turns(session, vec![first, second]);
}

#[test]
fn acp_a_finished_turn_in_the_log_replaces_its_live_items_in_one_snapshot() {
    let h = Harness::new("basic.jsonl");
    let session = h.session(1);
    h.watch(&session);
    let live = h.turn(&session, "hello");
    assert!(live_keys(&live) >= 2, "{:?}", keys(&live));
    assert_eq!(keys(&live)[..2], ["h:1:0", "h:1:1"]);

    log_with_turn(&h, &session, "hello");
    announce(&h, &session);
    let swapped = h.wait(&session, "the history to replace the live items", |s| {
        live_keys(s) == 0
    });
    assert_eq!(
        keys(&swapped),
        ["h:1:0", "h:1:1", "h:2:0", "h:2:1"],
        "the turn shows once, as history"
    );
    assert!(matches!(
        &swapped.entries[2].body,
        EntryBody::Prompt { text, .. } if &*text.text == "hello"
    ));
    assert!(matches!(
        &swapped.entries[3].body,
        EntryBody::AgentText { text, streaming: false } if &*text.text == "Hello world"
    ));
    assert!(
        swapped.generation > live.generation,
        "a new generation carries the swap"
    );
    // The next turn on the same connection starts from room, not from what the first one used.
    let next = h.turn(&session, "again");
    assert_eq!(
        keys(&next),
        ["h:1:0", "h:1:1", "h:2:0", "h:2:1", "l:1:0", "l:1:1"],
        "the swapped turn stays history; the new one is live from its own start"
    );
    assert_eq!(h.records().len(), 1, "the connection was reused");
}

#[test]
fn acp_a_turn_that_is_in_the_log_but_not_finished_shows_only_its_live_items() {
    let h = Harness::new("hold.jsonl");
    let session = h.session(1);
    h.watch(&session);
    h.prompt(&session, "hello");
    // Text updates reach the snapshot at most every 50 ms of the (manual) clock.
    wait_for("the reply to start", || {
        h.clock.advance(std::time::Duration::from_millis(60));
        h.snapshot(&session)
            .entries
            .iter()
            .any(|e| {
                matches!(&e.body, EntryBody::AgentText { text, .. } if &*text.text == "working")
            })
            .then_some(())
    });

    // The adapter has written the turn so far; the turn is still running.
    let calls = h.claude.turns_calls();
    log_with_turn(&h, &session, "hello");
    // Announcements that arrive before a read starts merge into that read, so keep announcing until a
    // second read has started: by then the first one has installed its window, which holds the turn (the
    // snapshot has nothing new to show for it, so there is nothing else to wait for).
    wait_for("the history to be read twice", || {
        announce(&h, &session);
        (h.claude.turns_calls() >= calls + 2).then_some(())
    });
    let running = h.snapshot(&session);
    let shown = keys(&running);
    assert!(
        !shown.iter().any(|k| k.starts_with("h:2:")),
        "the turn shows once, live: {shown:?}"
    );
    assert!(live_keys(&running) >= 2, "{shown:?}");

    // The connection goes away without the prompt returning; the turn is history only.
    h.close_project();
    let done = h.wait(&session, "the history to take over", |s| {
        live_keys(s) == 0 && s.entries.iter().any(|e| e.key.0.starts_with("h:2:"))
    });
    assert_eq!(keys(&done), ["h:1:0", "h:1:1", "h:2:0", "h:2:1"]);
}

#[test]
fn acp_a_turn_that_is_not_in_the_log_keeps_its_live_items() {
    let h = Harness::new("basic.jsonl");
    let session = h.session(1);
    h.watch(&session);
    h.turn(&session, "hello");

    // The log has a turn with another prompt, and an older one with the same words as ours.
    let first = h.claude.turn_at(
        1,
        1000,
        "hello",
        vec![h.claude.agent_text("an older answer")],
    );
    let second = h.claude.turn_at(
        2,
        now_ms() + 1000,
        "something else",
        vec![h.claude.agent_text("x")],
    );
    h.claude.set_turns(&session, vec![first, second]);
    announce(&h, &session);
    let snapshot = h.wait(&session, "the new window", |s| {
        s.entries.iter().any(|e| &*e.key.0 == "h:2:0")
    });
    assert!(live_keys(&snapshot) >= 2, "{:?}", keys(&snapshot));
    assert_eq!(keys(&snapshot)[..4], ["h:1:0", "h:1:1", "h:2:0", "h:2:1"]);
}
