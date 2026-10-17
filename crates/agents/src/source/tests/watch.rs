//! 监听（`watch_*`）与路由（`source_*`）：新增、追加、删除、目录事件、重新扫描、根目录后出现。

use std::io::Write as _;
use std::sync::atomic::Ordering;

use lectern_fs_watch::Flags;
use lectern_services::session::{Cursor, ToolCallId};

use super::support::*;
use super::{copy_tree, fixtures};

/// `claude/projects/-work/<id>.jsonl`，cwd 是 `<base>/work`。
fn write_claude(h: &Harness, id: &str, prompts: &[&str]) -> std::path::PathBuf {
    let dir = h.claude_root().join("-work");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{id}.jsonl"));
    std::fs::write(&path, claude_session(id, &h.base.join("work"), prompts, 0)).unwrap();
    path
}

fn append(path: &std::path::Path, text: &str) {
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    f.write_all(text.as_bytes()).unwrap();
}

#[test]
fn watch_new_session_appears() {
    let mut h = Harness::new();
    std::fs::create_dir_all(h.base.join("work")).unwrap();
    h.start();
    h.take_seen(&claude_source());
    let id = uuid(1);
    write_claude(&h, &id, &["first prompt"]);
    h.emit(&h.claude_root(), &[(&format!("-work/{id}.jsonl"), F)]);
    let seen = h.take_seen(&claude_source());
    assert_eq!(seen.len(), 1);
    assert_eq!(&*seen[0].event.changed, [key(&claude_source(), &id)]);
    assert_eq!(seen[0].event.generation, 2);
    let summary = seen[0].list.get(&key(&claude_source(), &id)).unwrap();
    assert_eq!(summary.title.as_deref(), Some("first prompt"));
}

#[test]
fn watch_append_updates_the_summary() {
    let mut h = Harness::new();
    let id = uuid(1);
    let path = write_claude(&h, &id, &["first prompt"]);
    h.start();
    let before = h.list(&claude_source()).sessions[0].clone();
    assert!(!before.open_turn);
    h.take_seen(&claude_source());

    // A new prompt that has no answer yet.
    let turn = claude_turn(&id, &h.base.join("work"), "second prompt", 30, 1);
    append(&path, turn.lines().next().unwrap());
    append(&path, "\n");
    h.emit(&h.claude_root(), &[(&format!("-work/{id}.jsonl"), F)]);
    let seen = h.take_seen(&claude_source());
    let after = seen[0].list.sessions[0].clone();
    assert!(after.open_turn);
    assert!(after.last_activity > before.last_activity);
    assert_eq!(after.title, before.title, "the title is the first prompt");
    assert_eq!(after.started_at, before.started_at);
}

#[test]
fn watch_append_follows_the_turn_index_of_an_open_session() {
    let mut h = Harness::new();
    let id = uuid(1);
    let session = key(&claude_source(), &id);
    let path = write_claude(&h, &id, &["first prompt"]);
    h.start();
    let index = h.source(&claude_source()).turns(&session).unwrap();
    assert_eq!(index.turns.len(), 1);
    let engine = h.claude_engine();
    let summaries = engine.full_summaries.load(Ordering::Relaxed);

    append(
        &path,
        &claude_turn(&id, &h.base.join("work"), "second prompt", 30, 1),
    );
    h.emit(&h.claude_root(), &[(&format!("-work/{id}.jsonl"), F)]);
    // The refresh parsed the appended part into the index in memory, without a new summary
    // (no head, no tail scan): the index cursor is at the end before anyone called `turns`.
    assert_eq!(engine.full_summaries.load(Ordering::Relaxed), summaries);
    let len = std::fs::metadata(&path).unwrap().len();
    let (held, _) = engine.memory_indexes().get(&session).unwrap();
    assert_eq!(held.cursor.offset(), len);
    assert_eq!(held.turns.len(), 2);
    let summary = h.list(&claude_source()).sessions[0].clone();
    assert!(!summary.open_turn);
    assert_eq!(summary.last_activity.0 % 60_000, 31_000);
}

#[test]
fn watch_deleted_session_is_removed() {
    let mut h = Harness::new();
    let id = uuid(1);
    let path = write_claude(&h, &id, &["first prompt"]);
    h.start();
    h.take_seen(&claude_source());
    std::fs::remove_file(&path).unwrap();
    h.emit(
        &h.claude_root(),
        &[(&format!("-work/{id}.jsonl"), F | Flags::REMOVED)],
    );
    let seen = h.take_seen(&claude_source());
    assert_eq!(&*seen[0].event.removed, [key(&claude_source(), &id)]);
    assert!(seen[0].list.sessions.is_empty());
    assert_eq!(
        h.source(&claude_source())
            .read(&key(&claude_source(), &id), &Cursor::START, 1),
        Err(lectern_services::session::SessionError::NotFound)
    );
}

#[test]
fn watch_removed_directory_removes_the_sessions_under_it() {
    let mut h = Harness::new();
    write_claude(&h, &uuid(1), &["one"]);
    write_claude(&h, &uuid(2), &["two"]);
    h.start();
    assert_eq!(h.list(&claude_source()).sessions.len(), 2);
    h.take_seen(&claude_source());
    std::fs::remove_dir_all(h.claude_root().join("-work")).unwrap();
    // The backend only reports the directory.
    h.emit(&h.claude_root(), &[("-work", D | Flags::REMOVED)]);
    let seen = h.take_seen(&claude_source());
    assert_eq!(seen[0].event.removed.len(), 2);
    assert!(h.list(&claude_source()).sessions.is_empty());
}

#[test]
fn watch_rescan_finds_what_no_event_reported() {
    let mut h = Harness::new();
    h.start();
    h.take_seen(&claude_source());
    write_claude(&h, &uuid(1), &["one"]);
    // Events were dropped: the backend asks for a rescan of the whole tree.
    h.emit(
        &h.claude_root(),
        &[("", D | Flags::RESCAN | Flags::DROPPED)],
    );
    assert_eq!(h.list(&claude_source()).sessions.len(), 1);
}

#[test]
fn watch_root_created_after_start_is_scanned() {
    let mut h = Harness::new();
    std::fs::remove_dir_all(h.codex_root()).unwrap();
    h.start();
    assert!(h.list(&codex_source()).sessions.is_empty());
    h.take_seen(&codex_source());
    // fs_watch watches `<CODEX_HOME>` for the root to appear.
    assert!(h.run().backend.is_armed(&h.base.join("codex")));

    copy_tree(
        &fixtures().join("codex/0.140.0-paginated/sessions"),
        &h.codex_root(),
    );
    h.emit(&h.base.join("codex"), &[("sessions", D | Flags::CREATED)]);
    let seen = h.take_seen(&codex_source());
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].event.changed.len(), 2);
    assert!(h.run().backend.is_armed(&h.codex_root()));
}

#[test]
fn watch_meta_json_links_the_subagent_to_its_tool_call() {
    let mut h = Harness::new();
    let fixture = fixtures().join(
        "claude-code/2.1.302/projects/-Users-dev-fixture-repo/11111111-1111-4111-8111-111111111111",
    );
    let dir = h
        .claude_root()
        .join("-Users-dev-fixture-repo/11111111-1111-4111-8111-111111111111/subagents");
    h.copy(
        &fixture.join("subagents/agent-a0fa0001.jsonl"),
        &dir.join("agent-a0fa0001.jsonl"),
    );
    h.start();
    let sub = key(
        &claude_source(),
        "11111111-1111-4111-8111-111111111111/agent-a0fa0001",
    );
    let parent = h.list(&claude_source()).get(&sub).unwrap().parent.clone();
    assert_eq!(parent.unwrap().tool_call, None, "no meta.json yet");

    h.copy(
        &fixture.join("subagents/agent-a0fa0001.meta.json"),
        &dir.join("agent-a0fa0001.meta.json"),
    );
    h.emit(
        &h.claude_root(),
        &[(
            "-Users-dev-fixture-repo/11111111-1111-4111-8111-111111111111/subagents/agent-a0fa0001.meta.json",
            F,
        )],
    );
    let parent = h.list(&claude_source()).get(&sub).unwrap().parent.clone();
    assert!(matches!(parent.unwrap().tool_call, Some(ToolCallId(_))));
}

#[test]
fn source_same_session_id_in_two_sources_gives_two_keys() {
    let mut h = Harness::new();
    let shared = "aaaaaaaa-0000-4000-8000-000000000001";
    write_claude(&h, shared, &["claude prompt"]);
    h.copy(
        &fixtures().join(format!(
            "codex/0.140.0-legacy/sessions/2026/10/02/rollout-2026-10-02T09-00-00-{shared}.jsonl"
        )),
        &h.codex_root().join(format!(
            "2026/10/02/rollout-2026-10-02T09-00-00-{shared}.jsonl"
        )),
    );
    h.start();
    let claude = h.list(&claude_source());
    let codex = h.list(&codex_source());
    assert_eq!(claude.sessions[0].key.id, codex.sessions[0].key.id);
    assert_ne!(claude.sessions[0].key, codex.sessions[0].key);
    for source in [claude_source(), codex_source()] {
        for seen in h.take_seen(&source) {
            assert_eq!(seen.event.source, source);
            assert!(seen.event.changed.iter().all(|k| k.source == source));
            assert!(seen.list.sessions.iter().all(|s| s.key.source == source));
        }
    }
}
