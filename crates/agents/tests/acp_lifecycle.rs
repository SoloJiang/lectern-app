//! When the ACP processes start and stop: idle exit, `Disconnect`, project close, the limit of three
//! connections, deactivation (docs/design/agents.md「ACP 客户端」的「运行、关闭、权限」).

mod common;

use std::time::Duration;

use common::*;
use lectern_services::conversation::{ConversationCommand, EntryBody, Link, Unavailable};

#[test]
fn acp_an_idle_connection_closes_after_ten_minutes() {
    let h = Harness::new("basic.jsonl");
    let session = h.session(1);
    h.watch(&session);
    h.turn(&session, "hello");
    let pid = h.records()[0].pid;

    h.clock.advance(Duration::from_secs(9 * 60 + 59));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(h.snapshot(&session).link, Link::Idle);
    assert!(alive(pid));

    h.clock.advance(Duration::from_secs(2));
    let closed = h.wait_link(&session, Link::None);
    wait_dead(pid);
    // What the connection showed is now history: the reread window replaces the live entries.
    assert!(
        closed.entries.iter().all(|e| !e.key.0.starts_with("l:")),
        "{:?}",
        closed.entries.iter().map(|e| &e.key.0).collect::<Vec<_>>()
    );
    assert!(closed.plan.is_none() && closed.context.is_none());

    // The next prompt starts a new process.
    h.turn(&session, "back again");
    assert_eq!(h.records().len(), 2);
}

#[test]
fn acp_activity_restarts_the_idle_time() {
    let h = Harness::new("basic.jsonl");
    let session = h.session(1);
    h.watch(&session);
    h.turn(&session, "one");
    h.clock.advance(Duration::from_secs(8 * 60));
    // A second turn nearly ten minutes after the first starts the clock again.
    h.turn(&session, "two");
    h.clock.advance(Duration::from_secs(8 * 60));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        h.snapshot(&session).link,
        Link::Idle,
        "16 minutes after the first turn, 8 after the second"
    );
    h.clock.advance(Duration::from_secs(3 * 60));
    h.wait_link(&session, Link::None);
}

#[test]
fn acp_disconnect_closes_an_idle_connection_and_not_a_running_turn() {
    let h = Harness::new("hold.jsonl");
    let session = h.session(1);
    h.watch(&session);
    h.prompt(&session, "start");
    h.wait_link(&session, Link::Working);
    let pid = h.started_pid();
    h.command(&session, ConversationCommand::Disconnect);
    assert_eq!(h.snapshot(&session).link, Link::Working, "ignored mid-turn");
    assert!(alive(pid));

    let h = Harness::new("basic.jsonl");
    let session = h.session(1);
    h.watch(&session);
    h.turn(&session, "hello");
    let pid = h.records()[0].pid;
    h.command(&session, ConversationCommand::Disconnect);
    assert_eq!(
        h.snapshot(&session).link,
        Link::None,
        "changed before the command returned"
    );
    wait_dead(pid);
}

#[test]
fn acp_closing_the_project_closes_its_connections() {
    let h = Harness::new("basic.jsonl");
    let session = h.session(1);
    h.watch(&session);
    h.turn(&session, "hello");
    let pid = h.records()[0].pid;
    h.close_project();
    let closed = h.wait_link(&session, Link::None);
    assert_eq!(closed.can_prompt, Err(Unavailable::OutsideProject));
    wait_dead(pid);
}

#[test]
fn acp_a_connection_keeps_its_state_without_a_watcher_until_it_closes() {
    let h = Harness::new("basic.jsonl");
    let session = h.session(1);
    h.watch(&session);
    h.turn(&session, "hello");
    h.unwatch_all();
    assert!(h.conversations.snapshot(&session).is_some());
    h.clock.advance(Duration::from_secs(11 * 60));
    wait_for("the state to go", || {
        h.conversations.snapshot(&session).is_none().then_some(())
    });
}

#[test]
fn acp_the_fourth_connection_is_refused_while_the_three_are_busy() {
    let h = Harness::new("hold.jsonl");
    let sessions: Vec<_> = (1..=4).map(|n| h.session(n)).collect();
    for session in &sessions {
        h.watch(session);
    }
    for session in &sessions[..3] {
        h.prompt(session, "start");
        h.wait_link(session, Link::Working);
    }
    let fourth = &sessions[3];
    let refused = h.wait(fourth, "the limit", |s| {
        s.can_prompt == Err(Unavailable::TooMany)
    });
    h.prompt(fourth, "start");
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(h.snapshot(fourth).generation, refused.generation);
    assert_eq!(h.records().len(), 3, "no fourth process");
}

#[test]
fn acp_the_fourth_connection_closes_the_idle_one_unused_longest() {
    let h = Harness::new("basic.jsonl");
    let sessions: Vec<_> = (1..=4).map(|n| h.session(n)).collect();
    for session in &sessions {
        h.watch(session);
    }
    for session in &sessions[..3] {
        h.turn(session, "hello");
        h.clock.advance(Duration::from_secs(1));
    }
    let first_pid = h.record_of(&sessions[0]).pid;
    let others: Vec<u32> = sessions[1..3].iter().map(|s| h.record_of(s).pid).collect();
    // Three idle connections: a fourth session can still prompt.
    assert_eq!(h.snapshot(&sessions[3]).can_prompt, Ok(()));

    h.prompt(&sessions[3], "make room");
    assert_eq!(
        h.snapshot(&sessions[0]).link,
        Link::None,
        "the oldest idle connection is closed at once"
    );
    h.wait(&sessions[3], "the fourth turn", |s| {
        s.link == Link::Idle && s.entries.iter().any(|e| e.key.0.starts_with("l:"))
    });
    wait_dead(first_pid);
    for pid in others {
        assert!(alive(pid), "the others stay");
    }
    assert_eq!(h.records().len(), 4);
}

#[test]
fn acp_sigkill_follows_sigterm_after_two_seconds() {
    let h = Harness::new("stubborn.jsonl");
    let session = h.session(1);
    h.watch(&session);
    h.turn(&session, "hello");
    let pid = h.records()[0].pid;
    wait_for("the helper child", || {
        (!h.children().is_empty()).then_some(())
    });
    let child = h.children()[0];

    h.command(&session, ConversationCommand::Disconnect);
    std::thread::sleep(Duration::from_millis(200));
    assert!(alive(pid), "it ignores SIGTERM");
    // The timer is set when the close runs; keep moving the clock until it has been reached.
    wait_for("SIGKILL", || {
        h.clock.advance(Duration::from_secs(2));
        (!alive(pid)).then_some(())
    });
    wait_dead(child);
}

#[test]
fn acp_deactivating_the_plugin_terminates_the_whole_process_group() {
    let h = Harness::new("stubborn.jsonl");
    let session = h.session(1);
    h.watch(&session);
    h.turn(&session, "hello");
    let pid = h.records()[0].pid;
    wait_for("the helper child", || {
        (!h.children().is_empty()).then_some(())
    });
    let child = h.children()[0];
    assert!(alive(pid) && alive(child));

    // The adapter ignores SIGTERM: the wait for it is bounded, then the group is killed.
    let started = std::time::Instant::now();
    h.kernel.deactivate();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "deactivation does not wait for a stubborn agent: {:?}",
        started.elapsed()
    );
    wait_dead(pid);
    wait_dead(child);
}

#[test]
fn acp_deactivating_the_plugin_ends_a_polite_agent_and_forgets_the_sessions() {
    let h = Harness::new("basic.jsonl");
    let session = h.session(1);
    h.watch(&session);
    h.turn(&session, "hello");
    let pid = h.records()[0].pid;
    h.kernel.deactivate();
    wait_dead(pid);
    assert!(h.conversations.snapshot(&session).is_none());
    // A late prompt does nothing.
    h.prompt(&session, "anyone there");
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(h.records().len(), 1);
}

#[test]
fn acp_a_process_that_dies_while_idle_is_reported_and_the_next_prompt_restarts_it() {
    let h = Harness::new("basic.jsonl");
    let session = h.session(1);
    h.watch(&session);
    h.turn(&session, "hello");
    let pid = h.records()[0].pid;
    // Something outside Lectern kills the adapter.
    let target = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .unwrap();
    rustix::process::kill_process(target, rustix::process::Signal::KILL).unwrap();
    let failed = h.wait(&session, "the failure", |s| {
        matches!(s.link, Link::Failed(_))
    });
    assert!(matches!(
        failed.link,
        Link::Failed(lectern_services::conversation::LinkError::Exited { code: None })
    ));
    h.turn(&session, "after the crash");
    assert_eq!(h.records().len(), 2);
    assert!(matches!(
        &h.snapshot(&session).entries.last().unwrap().body,
        EntryBody::AgentText { text, .. } if &*text.text == "Hello world"
    ));
}
