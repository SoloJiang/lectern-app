//! 不进内存 LRU 的轮次索引读取（`turns_unretained_*`，review 设计前置条件 G）：LRU 的字节数和条目不变，结果与
//! `turns` 相同，索引写进 `cache`、之后的 `turns` 从那里接着解析，不派生子 agent 的关联。

use std::io::{Seek, SeekFrom, Write as _};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use lectern_cache::Cache;
use lectern_services::session::{SessionKey, TurnIndex};

use super::fixtures;
use super::support::*;
use crate::claude::{self, ClaudeCode};
use crate::codex::Codex;
use crate::source::cached::{CachedIndex, index_key};

const PROMPTS: [&str; 3] = ["first prompt", "second prompt", "third prompt"];
const P1: &str = "bbbbbbbb-0000-4000-8000-000000000001";
const P2: &str = "bbbbbbbb-0000-4000-8000-000000000002";

fn claude_path(h: &Harness, id: &str) -> PathBuf {
    h.claude_root().join(format!("-work/{id}.jsonl"))
}

/// 一个三轮的 Claude Code 会话。
fn write_session(h: &Harness, n: u32) -> (SessionKey, PathBuf) {
    let id = uuid(n);
    let path = claude_path(h, &id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        claude_session(&id, &h.base.join("work"), &PROMPTS, 0),
    )
    .unwrap();
    (key(&claude_source(), &id), path)
}

fn previews(index: &TurnIndex) -> Vec<String> {
    index
        .turns
        .iter()
        .map(|t| t.prompt_preview.to_string())
        .collect()
}

fn cache(h: &Harness) -> Cache {
    Cache::new(lectern_cache::Config {
        root: h.cache_dir(),
        budgets: lectern_cache::Budgets::default(),
        ui_thread: None,
    })
}

fn cached(h: &Harness, session: &SessionKey, version: u32) -> Option<TurnIndex> {
    let hit: Option<CachedIndex> = cache(h).app().get(&index_key(session, version));
    hit.map(|c| c.0)
}

/// Codex 来源的适配器调用次数：`read`、`turns_after`、`load`。
fn codex_calls(h: &Harness) -> (usize, usize, usize) {
    let calls = &h.codex_engine().calls;
    (
        calls.read.load(Ordering::Relaxed),
        calls.turns_after.load(Ordering::Relaxed),
        calls.load.load(Ordering::Relaxed),
    )
}

fn paginated(h: &Harness, thread: &str, time: &str) {
    let name = format!("rollout-2026-10-05T{time}-{thread}.jsonl");
    h.copy(
        &fixtures()
            .join("codex/0.140.0-paginated/sessions/2026/10/05")
            .join(&name),
        &h.codex_root().join("2026/10/05").join(&name),
    );
}

/// 有 `paginated` 的父会话 P1 与它的子会话 P2，发现完成。
fn codex_harness() -> Harness {
    let mut h = Harness::new();
    paginated(&h, P1, "09-00-00");
    paginated(&h, P2, "09-30-00");
    h.start();
    h
}

/// 子会话 P2 在当前列表里的 `Parent::tool_call`。
fn p2_tool_call(h: &Harness) -> Option<String> {
    let list = h.list(&codex_source());
    let summary = list.get(&key(&codex_source(), P2)).expect("listed");
    summary
        .parent
        .as_ref()
        .expect("a subagent")
        .tool_call
        .as_ref()
        .map(|t| t.0.to_string())
}

#[test]
fn turns_unretained_leaves_the_memory_lru_as_it_was() {
    let mut h = Harness::new();
    let (a, a_path) = write_session(&h, 7);
    let (b, _) = write_session(&h, 8);
    h.start();
    let source = h.source(&claude_source());
    let engine = h.claude_engine();
    let held = source.turns(&a).unwrap();
    h.settle();
    let bytes = engine.memory_indexes().bytes();
    assert!(bytes > 0);

    // Not in memory: read, not kept.
    let index = source.turns_unretained(&b).unwrap();
    assert_eq!(previews(&index), PROMPTS);
    drop(index);
    h.settle();
    {
        let lru = engine.memory_indexes();
        assert_eq!(lru.bytes(), bytes, "nothing added");
        assert!(!lru.contains(&b));
        assert!(lru.contains(&a), "nothing evicted");
    }

    // In memory on the current file: the same `Arc`.
    let hit = source.turns_unretained(&a).unwrap();
    assert!(Arc::ptr_eq(&hit, &held));
    assert_eq!(engine.memory_indexes().bytes(), bytes);

    // In memory, but the file has grown since: parsed on from the memory copy, which stays as it was.
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&a_path)
        .unwrap();
    f.write_all(claude_turn(&a.id, &h.base.join("work"), "fourth prompt", 40, 3).as_bytes())
        .unwrap();
    drop(f);
    let grown = source.turns_unretained(&a).unwrap();
    assert_eq!(previews(&grown).len(), 4);
    h.settle();
    let lru = engine.memory_indexes();
    assert_eq!(lru.bytes(), bytes, "not replaced");
    let (still, _) = lru.peek(&a).unwrap();
    assert!(Arc::ptr_eq(&still, &held));
    assert!(!lru.contains(&b));
}

#[test]
fn turns_unretained_equals_turns() {
    // Claude Code.
    let mut h = Harness::new();
    let (session, path) = write_session(&h, 7);
    h.start();
    let source = h.source(&claude_source());
    let unretained = source.turns_unretained(&session).unwrap();
    let adapter = ClaudeCode::new(claude_source());
    let file = adapter.session_file(&path).unwrap();
    assert_eq!(*unretained, adapter.turns(&file).unwrap());
    let turns = source.turns(&session).unwrap();
    assert_eq!(*unretained, *turns);
    drop(source);

    // Codex, against a `turns` in another run that never called `turns_unretained`.
    let h = codex_harness();
    let parent = key(&codex_source(), P1);
    let unretained = h.source(&codex_source()).turns_unretained(&parent).unwrap();
    let other = codex_harness();
    let turns = other.source(&codex_source()).turns(&parent).unwrap();
    assert_eq!(*unretained, *turns);
    let adapter = Codex::without_decoded_copies(codex_source());
    let file = h.codex_engine().file_by_id(P1).unwrap();
    assert_eq!(*unretained, adapter.turns(&file).unwrap());
}

#[test]
fn turns_unretained_writes_the_index_for_a_later_turns() {
    let mut h = Harness::new();
    let (session, path) = write_session(&h, 7);
    h.start();
    let source = h.source(&claude_source());
    let engine = h.claude_engine();
    let index = source.turns_unretained(&session).unwrap();
    // Written before the call returned, not by a queued task.
    assert_eq!(
        cached(&h, &session, claude::ADAPTER_VERSION).as_ref(),
        Some(&*index)
    );
    drop(index);
    assert!(!engine.memory_indexes().contains(&session));

    // Rewrite the second prompt in place, far from the bytes the cursor checks: a full parse would
    // see it, a `turns` that parses on from the cached index does not.
    let text = std::fs::read_to_string(&path).unwrap();
    let at = text.find("second prompt").unwrap();
    let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.seek(SeekFrom::Start(at as u64)).unwrap();
    f.write_all(b"SECOND PROMPT").unwrap();
    drop(f);

    let turns = source.turns(&session).unwrap();
    assert_eq!(previews(&turns), PROMPTS, "from the cache");
    assert!(engine.memory_indexes().contains(&session), "turns keeps it");
}

#[test]
fn turns_unretained_does_not_derive_spawn_links() {
    // Reference: a `turns` of the parent, alone.
    let reference = codex_harness();
    let before = codex_calls(&reference);
    reference
        .source(&codex_source())
        .turns(&key(&codex_source(), P1))
        .unwrap();
    reference.settle();
    let after = codex_calls(&reference);
    let derive_loads = after.2 - before.2;
    assert!(derive_loads > 0, "the parent has spawn_agent calls");
    assert_eq!(p2_tool_call(&reference).as_deref(), Some("call_p1_0003"));

    let h = codex_harness();
    let parent = key(&codex_source(), P1);
    let engine = h.codex_engine();
    h.take_seen(&codex_source());
    let links = engine.links_of(&parent);
    let (read, turns_after, load) = codex_calls(&h);

    h.source(&codex_source()).turns_unretained(&parent).unwrap();
    h.settle();
    assert_eq!(
        codex_calls(&h),
        (read, turns_after + 1, load),
        "one parse, no load for a derivation"
    );
    assert_eq!(engine.links_of(&parent), links, "links unchanged");
    assert_eq!(p2_tool_call(&h), None);
    assert!(h.take_seen(&codex_source()).is_empty(), "nothing published");
    assert!(!engine.memory_indexes().contains(&parent));

    // A later `turns` parses on from the cached index and derives once, as on its own.
    h.source(&codex_source()).turns(&parent).unwrap();
    h.settle();
    assert_eq!(
        codex_calls(&h),
        (read, turns_after + 2, load + derive_loads)
    );
    assert_eq!(p2_tool_call(&h).as_deref(), Some("call_p1_0003"));
    let seen = h.take_seen(&codex_source());
    assert_eq!(seen.len(), 1);
    assert_eq!(&*seen[0].event.changed, [key(&codex_source(), P2)]);
}
