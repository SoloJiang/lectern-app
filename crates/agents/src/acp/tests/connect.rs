//! Prompts and the other connection commands, in the cases that need no process (`connect_*`): the
//! rig has no executor, so a start that would need a reader thread fails with `Spawn`. What a real
//! process does is in `tests/acp_*.rs`, with the fake agent.

use lectern_services::conversation::{
    AcpProgram, ConversationCommand, Conversations, EntryBody, Link, LinkError, PromptOrigin,
    Unavailable,
};

use super::support::*;

fn prompt(text: &str) -> ConversationCommand {
    ConversationCommand::Prompt {
        text: text.into(),
        origin: PromptOrigin::User,
    }
}

#[test]
fn connect_prompt_waits_for_the_lookup_and_fails_with_spawn_when_the_program_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let rig = Rig::new(dir.path());
    let session = rig.claude_session();
    // The lookup is under way and has not answered.
    rig.engine.set_searching(AcpProgram::ClaudeCode);
    let _interest = rig.engine.watch(&session);
    assert_eq!(rig.engine.snapshot(&session).unwrap().can_prompt, Ok(()));

    rig.engine.command(&session, prompt("hello"));
    let waiting = rig.engine.snapshot(&session).unwrap();
    assert_eq!(waiting.link, Link::Connecting, "the prompt waits");
    assert_eq!(waiting.can_prompt, Err(Unavailable::Busy));
    let EntryBody::Prompt { text, origin } = &waiting.entries.last().unwrap().body else {
        panic!("the prompt is the last entry");
    };
    assert_eq!((&*text.text, origin), ("hello", &PromptOrigin::User));
    assert_eq!(&*waiting.entries.last().unwrap().key.0, "l:0:0");

    // The lookup finishes: nothing is installed.
    rig.engine.lookup(AcpProgram::ClaudeCode);
    let failed = rig.engine.snapshot(&session).unwrap();
    assert_eq!(failed.link, Link::Failed(LinkError::Spawn));
    assert_eq!(
        failed.entries.len(),
        waiting.entries.len(),
        "the prompt stays"
    );
    assert_eq!(
        failed.can_prompt,
        Err(Unavailable::NotInstalled(AcpProgram::ClaudeCode))
    );

    // `Dismiss` clears the failure; there is no connection to go back to.
    rig.engine.command(&session, ConversationCommand::Dismiss);
    assert_eq!(rig.engine.snapshot(&session).unwrap().link, Link::None);
}

#[test]
fn connect_a_found_program_that_cannot_be_started_here_fails_with_spawn() {
    let dir = tempfile::tempdir().unwrap();
    install(dir.path(), "claude-agent-acp");
    let rig = Rig::new(dir.path());
    let session = rig.claude_session();
    let _interest = rig.engine.watch(&session);
    rig.engine.command(&session, prompt("hello"));
    assert_eq!(
        rig.engine.snapshot(&session).unwrap().link,
        Link::Failed(LinkError::Spawn)
    );
    // `Disconnect` also clears a failure that has no connection.
    rig.engine
        .command(&session, ConversationCommand::Disconnect);
    assert_eq!(rig.engine.snapshot(&session).unwrap().link, Link::None);
    // A later prompt starts over.
    rig.engine.command(&session, prompt("again"));
    let snapshot = rig.engine.snapshot(&session).unwrap();
    assert_eq!(snapshot.link, Link::Failed(LinkError::Spawn));
    let keys: Vec<&str> = snapshot
        .entries
        .iter()
        .filter(|e| e.key.0.starts_with("l:"))
        .map(|e| &*e.key.0)
        .collect();
    assert_eq!(keys, ["l:0:0", "l:1:0"], "live keys only grow");
}

#[test]
fn connect_a_prompt_over_256_kib_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    install(dir.path(), "claude-agent-acp");
    let rig = Rig::new(dir.path());
    let session = rig.claude_session();
    let _interest = rig.engine.watch(&session);
    let before = rig.engine.snapshot(&session).unwrap();
    rig.engine
        .command(&session, prompt(&"x".repeat((256 << 10) + 1)));
    assert_eq!(rig.engine.snapshot(&session).unwrap(), before);
    // Exactly the limit is accepted.
    rig.engine.command(&session, prompt(&"x".repeat(256 << 10)));
    assert_ne!(rig.engine.snapshot(&session).unwrap(), before);
}

#[test]
fn connect_a_prompt_for_a_session_nobody_watches_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    install(dir.path(), "claude-agent-acp");
    let rig = Rig::new(dir.path());
    let session = rig.claude_session();
    rig.engine.command(&session, prompt("hello"));
    assert!(rig.engine.snapshot(&session).is_none());
}
