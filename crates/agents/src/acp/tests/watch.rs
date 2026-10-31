//! 关注、快照和命令（`watch_*`）。

use std::path::Path;

use lectern_services::conversation::{
    ConversationCommand, ConversationEvent, Conversations, HistoryError, Link, Load, OptionId,
    PermissionId, PromptOrigin,
};
use lectern_services::session::Agent;

use super::support::*;

fn rig() -> Rig {
    Rig::new(Path::new("/nonexistent"))
}

#[test]
fn watch_shares_one_snapshot_and_releases_it_with_the_last_interest() {
    let rig = rig();
    let session = rig.claude_session();
    assert!(rig.engine.snapshot(&session).is_none());
    let first = rig.engine.watch(&session);
    let second = rig.engine.watch(&session);
    let snapshot = rig.engine.snapshot(&session).unwrap();
    assert_eq!(snapshot.agent, Agent::ClaudeCode);
    assert_eq!(snapshot.link, Link::None);
    assert_eq!(snapshot.load, Load::Ready);
    assert_eq!(rig.engine.watched(), 1);
    drop(first);
    assert!(rig.engine.snapshot(&session).is_some());
    drop(second);
    assert!(rig.engine.snapshot(&session).is_none());
    assert_eq!(rig.engine.watched(), 0);
    // Watching again starts from scratch.
    let _again = rig.engine.watch(&session);
    assert_eq!(rig.engine.snapshot(&session).unwrap().load, Load::Ready);
}

#[test]
fn watch_events_carry_the_snapshots_generation() {
    let rig = rig();
    let session = rig.claude_session();
    let _interest = rig.engine.watch(&session);
    let events = rig.take_events();
    assert!(!events.is_empty());
    let mut previous = 0;
    for event in &events {
        let ConversationEvent::Changed {
            session: changed,
            generation,
        } = event
        else {
            panic!("only Changed events: {event:?}");
        };
        assert_eq!(changed, &session);
        assert!(*generation > previous, "generations only grow");
        previous = *generation;
    }
    assert_eq!(previous, rig.engine.snapshot(&session).unwrap().generation);
}

#[test]
fn watch_rereads_after_a_sessions_change_and_stays_quiet_when_nothing_changed() {
    let rig = rig();
    let session = rig.claude_session();
    let _interest = rig.engine.watch(&session);
    rig.take_events();
    let before = rig.engine.snapshot(&session).unwrap();

    // Nothing in the log changed: the same content, no new generation, no event.
    rig.engine
        .on_sessions_changed(&changed(&claude_source(), &session));
    let same = rig.engine.snapshot(&session).unwrap();
    assert_eq!(same.generation, before.generation);
    assert!(rig.take_events().is_empty());

    // A new turn arrives.
    let mut turns: Vec<_> = (1..=2)
        .map(|n| rig.claude.turn(n, &format!("prompt {n}"), vec![]))
        .collect();
    turns.push(rig.claude.turn(3, "third", vec![]));
    rig.claude.set_turns(&session, turns);
    rig.engine
        .on_sessions_changed(&changed(&claude_source(), &session));
    let after = rig.engine.snapshot(&session).unwrap();
    assert_eq!(after.generation, before.generation + 1);
    assert_eq!(after.entries.len(), 3);
    assert_eq!(
        rig.take_events(),
        [ConversationEvent::Changed {
            session: session.clone(),
            generation: after.generation
        }]
    );

    // A session nobody watches is not read.
    let other = key(&claude_source(), "c1a0de00-2222-4222-8222-000000000002");
    let calls = rig.claude.turns_calls();
    rig.engine
        .on_sessions_changed(&changed(&claude_source(), &other));
    assert_eq!(rig.claude.turns_calls(), calls);
}

#[test]
fn watch_load_earlier_reads_twenty_more_turns_up_to_two_hundred() {
    let rig = rig();
    let session = rig.claude_session();
    let turns = (1..=250).map(|n| rig.claude.turn(n, "p", vec![])).collect();
    rig.claude.set_turns(&session, turns);
    let _interest = rig.engine.watch(&session);
    assert_eq!(rig.engine.snapshot(&session).unwrap().earlier, 230);
    rig.engine
        .command(&session, ConversationCommand::LoadEarlier);
    let snapshot = rig.engine.snapshot(&session).unwrap();
    assert_eq!((snapshot.entries.len(), snapshot.earlier), (40, 210));
    for _ in 0..20 {
        rig.engine
            .command(&session, ConversationCommand::LoadEarlier);
    }
    let snapshot = rig.engine.snapshot(&session).unwrap();
    assert_eq!((snapshot.entries.len(), snapshot.earlier), (200, 50));
    let calls = rig.claude.turns_calls();
    rig.engine
        .command(&session, ConversationCommand::LoadEarlier);
    assert_eq!(rig.claude.turns_calls(), calls, "the cap is reached");
}

#[test]
fn watch_load_earlier_does_nothing_without_earlier_turns() {
    let rig = rig();
    let session = rig.claude_session();
    let _interest = rig.engine.watch(&session);
    let calls = rig.claude.turns_calls();
    rig.engine
        .command(&session, ConversationCommand::LoadEarlier);
    assert_eq!(rig.claude.turns_calls(), calls);
}

#[test]
fn watch_ignores_commands_that_need_a_connection() {
    let rig = rig();
    let session = rig.claude_session();
    let _interest = rig.engine.watch(&session);
    let before = rig.engine.snapshot(&session).unwrap();
    rig.take_events();
    for command in [
        ConversationCommand::Prompt {
            text: "hello".into(),
            origin: PromptOrigin::User,
        },
        ConversationCommand::Cancel,
        ConversationCommand::Answer {
            request: PermissionId(1),
            option: OptionId("allow".into()),
        },
        ConversationCommand::Disconnect,
        ConversationCommand::Dismiss,
    ] {
        rig.engine.command(&session, command);
    }
    assert_eq!(rig.engine.snapshot(&session).unwrap(), before);
    assert!(rig.take_events().is_empty());
    assert!(rig.engine.waiting().is_empty());
}

#[test]
fn watch_a_session_whose_source_is_gone_is_not_found() {
    let rig = rig();
    let gone = key(
        &lectern_services::session::SourceId("session-gone".into()),
        CLAUDE_SESSION,
    );
    let _interest = rig.engine.watch(&gone);
    let snapshot = rig.engine.snapshot(&gone).unwrap();
    assert_eq!(snapshot.load, Load::Failed(HistoryError::NotFound));
    assert_eq!(snapshot.agent, Agent::Other("session-gone".into()));
    assert!(snapshot.entries.is_empty());
}

#[test]
fn watch_a_stopped_engine_hands_out_inert_interests() {
    let rig = rig();
    let session = rig.claude_session();
    let interest = rig.engine.watch(&session);
    rig.engine.stop();
    assert!(rig.engine.snapshot(&session).is_none());
    let _later = rig.engine.watch(&session);
    assert!(rig.engine.snapshot(&session).is_none());
    drop(interest);
}
