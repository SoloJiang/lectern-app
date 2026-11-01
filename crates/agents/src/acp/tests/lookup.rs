//! 程序查找（`lookup_*`）。

use lectern_services::conversation::{AcpProgram, ConversationEvent, Conversations, Unavailable};

use super::support::*;

#[test]
fn lookup_finds_the_program_on_the_path_once() {
    let dir = tempfile::tempdir().unwrap();
    install(dir.path(), "claude-agent-acp");
    let rig = Rig::new(dir.path());
    let session = rig.claude_session();
    let _first = rig.engine.watch(&session);
    assert_eq!(rig.engine.lookup_state(AcpProgram::ClaudeCode), Some(true));
    // The result is kept for the run: removing the file changes nothing.
    std::fs::remove_file(dir.path().join("claude-agent-acp")).unwrap();
    let _second = rig.engine.watch(&session);
    assert_eq!(rig.engine.lookup_state(AcpProgram::ClaudeCode), Some(true));
    assert_eq!(rig.engine.snapshot(&session).unwrap().can_prompt, Ok(()));
    // Only the programs sessions need are looked up.
    assert_eq!(rig.engine.lookup_state(AcpProgram::Codex), None);
}

#[test]
fn lookup_retries_a_missing_program_on_the_next_watch() {
    let dir = tempfile::tempdir().unwrap();
    let rig = Rig::new(dir.path());
    let session = rig.claude_session();
    let first = rig.engine.watch(&session);
    assert_eq!(rig.engine.lookup_state(AcpProgram::ClaudeCode), Some(false));
    assert_eq!(
        rig.engine.snapshot(&session).unwrap().can_prompt,
        Err(Unavailable::NotInstalled(AcpProgram::ClaudeCode))
    );
    let generation = rig.engine.snapshot(&session).unwrap().generation;
    // The user installs it. Nothing changes until the next `watch`.
    install(dir.path(), "claude-agent-acp");
    rig.engine.on_projects_changed();
    assert_eq!(rig.engine.lookup_state(AcpProgram::ClaudeCode), Some(false));
    assert_eq!(
        rig.engine.snapshot(&session).unwrap().generation,
        generation
    );
    let _second = rig.engine.watch(&session);
    assert_eq!(rig.engine.lookup_state(AcpProgram::ClaudeCode), Some(true));
    let snapshot = rig.engine.snapshot(&session).unwrap();
    assert_eq!(snapshot.can_prompt, Ok(()));
    assert!(snapshot.generation > generation);
    drop(first);
}

#[test]
fn lookup_announces_the_change_with_the_new_generation() {
    let dir = tempfile::tempdir().unwrap();
    let rig = Rig::new(dir.path());
    let session = rig.claude_session();
    let _interest = rig.engine.watch(&session);
    let snapshot = rig.engine.snapshot(&session).unwrap();
    let last = rig.take_events().pop().expect("an event");
    assert_eq!(
        last,
        ConversationEvent::Changed {
            session,
            generation: snapshot.generation
        }
    );
}

#[test]
fn lookup_ignores_a_path_entry_that_is_relative() {
    let dir = tempfile::tempdir().unwrap();
    install(dir.path(), "claude-agent-acp");
    // `find_program` skips relative entries, so the program is not found by a relative PATH.
    let rig = Rig::new(std::path::Path::new("relative/bin"));
    let session = rig.claude_session();
    let _interest = rig.engine.watch(&session);
    assert_eq!(rig.engine.lookup_state(AcpProgram::ClaudeCode), Some(false));
}
