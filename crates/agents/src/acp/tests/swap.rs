//! Live items give way to history items (`swap_*`, docs/design/agents.md「ACP 客户端」的「实时项换成
//! 历史项」), without a process: the test plays the connection by changing the live items directly.

use std::path::Path;

use lectern_services::conversation::{
    ConversationSnapshot, Conversations, EntryBody, PromptOrigin,
};

use super::support::*;
use crate::acp::wire::Stop;

fn keys(snapshot: &ConversationSnapshot) -> Vec<String> {
    snapshot
        .entries
        .iter()
        .map(|e| e.key.0.to_string())
        .collect()
}

#[test]
fn swap_replaces_a_returned_turn_in_the_same_generation_and_never_shows_a_turn_twice() {
    let rig = Rig::new(Path::new("/nonexistent"));
    let session = rig.claude_session();
    let _interest = rig.engine.watch(&session);
    assert_eq!(keys(&rig.engine.snapshot(&session).unwrap()).len(), 3);

    // The user sends a prompt; the reply is streaming.
    rig.engine.with_live(&session, |live| {
        live.begin_prompt("third prompt", PromptOrigin::User, 10_000);
        live.agent_text("a live reply");
    });
    let live = rig.engine.snapshot(&session).unwrap();
    assert_eq!(
        keys(&live),
        ["h:1:0", "h:2:0", "h:2:1", "l:0:0", "l:0:1"],
        "history first, then the live turn"
    );

    // The adapter has written the turn to the log while it is still running: one turn, shown live.
    let s = &rig.claude;
    let mut turns = vec![
        s.turn(1, "first prompt", vec![]),
        s.turn(2, "second prompt", vec![s.agent_text("a reply")]),
    ];
    turns.push(s.turn_at(
        3,
        10_500,
        "third prompt",
        vec![s.agent_text("the logged reply")],
    ));
    s.set_turns(&session, turns);
    rig.engine
        .on_sessions_changed(&changed(&claude_source(), &session));
    let running = rig.engine.snapshot(&session).unwrap();
    assert_eq!(
        keys(&running),
        ["h:1:0", "h:2:0", "h:2:1", "l:0:0", "l:0:1"],
        "the logged copy of a running turn is not shown"
    );

    // The prompt returns: in one new generation the live items are gone and the history shows the turn.
    rig.engine
        .with_live(&session, |live| live.finish(Stop::EndTurn));
    let done = rig.engine.snapshot(&session).unwrap();
    assert_eq!(done.generation, running.generation + 1);
    assert_eq!(
        keys(&done),
        ["h:1:0", "h:2:0", "h:2:1", "h:3:0", "h:3:1"],
        "history only"
    );
    assert!(matches!(
        &done.entries[4].body,
        EntryBody::AgentText { text, .. } if &*text.text == "the logged reply"
    ));
}

#[test]
fn swap_waits_for_the_log_when_the_prompt_returns_first() {
    let rig = Rig::new(Path::new("/nonexistent"));
    let session = rig.claude_session();
    let _interest = rig.engine.watch(&session);
    rig.engine.with_live(&session, |live| {
        live.begin_prompt("third prompt", PromptOrigin::User, 10_000);
        live.agent_text("a live reply");
        live.finish(Stop::EndTurn);
    });
    let before = rig.engine.snapshot(&session).unwrap();
    assert_eq!(keys(&before)[3..], ["l:0:0", "l:0:1"], "not in the log yet");

    let s = &rig.claude;
    let turns = vec![
        s.turn(1, "first prompt", vec![]),
        s.turn(2, "second prompt", vec![s.agent_text("a reply")]),
        s.turn_at(
            3,
            10_500,
            "third prompt",
            vec![s.agent_text("the logged reply")],
        ),
    ];
    s.set_turns(&session, turns);
    rig.engine
        .on_sessions_changed(&changed(&claude_source(), &session));
    let after = rig.engine.snapshot(&session).unwrap();
    assert_eq!(keys(&after), ["h:1:0", "h:2:0", "h:2:1", "h:3:0", "h:3:1"]);
    assert_eq!(after.generation, before.generation + 1);
}
