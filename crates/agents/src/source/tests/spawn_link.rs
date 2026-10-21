//! Codex 子 agent 关联派出它的调用（`spawn_link_*`，docs/design/agents.md「Codex 子 agent 与派出它的调用」，
//! 验收第 5 步）。全部是合成数据：fixtures 里的 `paginated` 父会话与子 rollout，其余在临时目录里生成。

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use lectern_cache::Cache;
use lectern_services::session::{SessionKey, ToolCallId, TurnItem};

use super::support::*;
use super::{copy_tree, fixtures};
use crate::codex::{self, Codex};
use crate::source::cached::{CachedSpawns, spawns_key};

const L1: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const P1: &str = "bbbbbbbb-0000-4000-8000-000000000001";
const P2: &str = "bbbbbbbb-0000-4000-8000-000000000002";
const MINUTE: i64 = 60_000;

// --- 合成的 rollout ----------------------------------------------------------------

fn ts(sec: u32) -> String {
    format!("2026-10-09T09:{:02}:{:02}.000Z", sec / 60 % 60, sec % 60)
}

/// `ts(sec)` 的 UTC 毫秒。
fn at(sec: u32) -> i64 {
    crate::time::parse(&ts(sec)).unwrap().0
}

fn line(kind: &str, sec: u32, payload: &str) -> String {
    format!(
        r#"{{"timestamp":"{}","type":"{kind}","payload":{payload}}}"#,
        ts(sec)
    )
}

/// `session_meta`；`source` 是 JSON（`"cli"`、[`subagent_of`] 等）。
fn meta(thread: &str, source: &str) -> String {
    line(
        "session_meta",
        0,
        &format!(
            r#"{{"id":"{thread}","timestamp":"{}","cwd":"/Users/dev/fixture/repo","cli_version":"0.140.0","source":{source},"history_mode":"legacy"}}"#,
            ts(0)
        ),
    )
}

fn subagent_of(parent: &str) -> String {
    format!(r#"{{"subagent":{{"thread_spawn":{{"parent_thread_id":"{parent}","depth":1}}}}}}"#)
}

fn turn_start(turn: &str, sec: u32, prompt: &str) -> Vec<String> {
    vec![
        line(
            "turn_context",
            sec,
            &format!(r#"{{"turn_id":"{turn}","cwd":"/Users/dev/fixture/repo","model":"m"}}"#),
        ),
        line(
            "event_msg",
            sec,
            &format!(r#"{{"type":"task_started","turn_id":"{turn}"}}"#),
        ),
        line(
            "event_msg",
            sec,
            &format!(r#"{{"type":"user_message","message":"{prompt}","images":[]}}"#),
        ),
    ]
}

fn turn_end(turn: &str, sec: u32) -> String {
    line(
        "event_msg",
        sec,
        &format!(r#"{{"type":"task_complete","turn_id":"{turn}","last_agent_message":"done"}}"#),
    )
}

fn call(name: &str, call: &str, sec: u32) -> String {
    line(
        "response_item",
        sec,
        &format!(
            r#"{{"type":"function_call","name":"{name}","arguments":"{{\"message\":\"task\"}}","call_id":"{call}"}}"#
        ),
    )
}

/// `function_call_output`，`output` 是原样的输出文本。
fn output(call: &str, sec: u32, output: &str) -> String {
    let output = serde_json::to_string(output).unwrap();
    line(
        "response_item",
        sec,
        &format!(r#"{{"type":"function_call_output","call_id":"{call}","output":{output}}}"#),
    )
}

/// 一次 `spawn_agent` 调用和它的输出。
fn spawn(id: &str, sec: u32, out: &str) -> Vec<String> {
    vec![call("spawn_agent", id, sec), output(id, sec + 1, out)]
}

/// multi_agent v1 的输出。
fn agent_out(child: &str) -> String {
    format!(r#"{{"agent_id":"{child}","nickname":null}}"#)
}

fn rollout_path(h: &Harness, thread: &str) -> PathBuf {
    h.codex_root().join(format!(
        "2026/10/09/rollout-2026-10-09T09-00-00-{thread}.jsonl"
    ))
}

fn text(lines: &[String]) -> String {
    let mut out = String::new();
    for l in lines {
        out.push_str(l);
        out.push('\n');
    }
    out
}

fn write(h: &Harness, thread: &str, lines: &[String]) -> PathBuf {
    let path = rollout_path(h, thread);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text(lines)).unwrap();
    path
}

fn append(path: &Path, lines: &[String]) {
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    f.write_all(text(lines).as_bytes()).unwrap();
}

/// 一个子 rollout：`session_meta` 的来源是 `source`，自己的一轮。
fn child_with(h: &Harness, thread: &str, source: &str) -> PathBuf {
    let mut lines = vec![meta(thread, source)];
    lines.extend(turn_start(&format!("turn-{thread}"), 1, "child task"));
    lines.push(turn_end(&format!("turn-{thread}"), 2));
    write(h, thread, &lines)
}

fn child(h: &Harness, thread: &str, parent: &str) -> PathBuf {
    child_with(h, thread, &subagent_of(parent))
}

/// 一个父会话：一轮里依次派出 `children`（调用 ID `call_<n>`）；`ended` 时这一轮已结束。
fn parent(h: &Harness, thread: &str, children: &[&str], ended: bool) -> PathBuf {
    let mut lines = vec![meta(thread, r#""cli""#)];
    lines.extend(turn_start("t1", 1, "parent prompt"));
    for (n, c) in children.iter().enumerate() {
        lines.extend(spawn(&format!("call_{n}"), 2 + 2 * n as u32, &agent_out(c)));
    }
    if ended {
        lines.push(turn_end("t1", 30));
    }
    write(h, thread, &lines)
}

fn emit_paths(h: &Harness, paths: &[&Path]) {
    let rels: Vec<String> = paths.iter().map(|p| h.rel_to(&h.codex_root(), p)).collect();
    let events: Vec<(&str, lectern_fs_watch::Flags)> =
        rels.iter().map(|r| (r.as_str(), F)).collect();
    h.emit(&h.codex_root(), &events);
}

fn codex_key(id: &str) -> SessionKey {
    key(&codex_source(), id)
}

/// 子会话在当前列表里的 `Parent::tool_call`。
fn tool_call(h: &Harness, child: &str) -> Option<String> {
    let list = h.list(&codex_source());
    let summary = list.get(&codex_key(child)).expect("the child is listed");
    summary
        .parent
        .as_ref()
        .expect("a subagent")
        .tool_call
        .as_ref()
        .map(|t| t.0.to_string())
}

fn turns(h: &Harness, thread: &str) {
    h.source(&codex_source()).turns(&codex_key(thread)).unwrap();
    h.settle();
}

fn calls(h: &Harness) -> (usize, usize, usize) {
    let calls = &h.codex_engine().calls;
    (
        calls.read.load(Ordering::Relaxed),
        calls.turns_after.load(Ordering::Relaxed),
        calls.load.load(Ordering::Relaxed),
    )
}

fn paginated(name: &str) -> PathBuf {
    fixtures()
        .join("codex/0.140.0-paginated/sessions/2026/10/05")
        .join(name)
}

fn p1_name() -> String {
    format!("rollout-2026-10-05T09-00-00-{P1}.jsonl")
}

fn p2_name() -> String {
    format!("rollout-2026-10-05T09-30-00-{P2}.jsonl")
}

fn in_root(h: &Harness, name: &str) -> PathBuf {
    h.codex_root().join("2026/10/05").join(name)
}

fn cache(h: &Harness) -> Cache {
    Cache::new(lectern_cache::Config {
        root: h.cache_dir(),
        budgets: lectern_cache::Budgets::default(),
        ui_thread: None,
    })
}

// --- 测试 -----------------------------------------------------------------------

#[test]
fn spawn_link_spawned_thread_takes_a_canonical_agent_id_from_the_output() {
    let dir = tempfile::tempdir().unwrap();
    let parent = uuid(1);
    let child = uuid(2);
    let mut lines = vec![meta(&parent, r#""cli""#)];
    lines.extend(turn_start("t1", 1, "prompt"));
    lines.extend(spawn("c_v1", 2, &agent_out(&child)));
    lines.extend(spawn("c_bare", 4, &format!(r#"{{"agent_id":"{child}"}}"#)));
    lines.extend(spawn("c_short", 6, r#"{"agent_id":"1234"}"#));
    lines.extend(spawn(
        "c_braced",
        8,
        &format!(r#"{{"agent_id":"{{{child}}}"}}"#),
    ));
    lines.extend(spawn(
        "c_v2",
        10,
        r#"{"task_name":"helper","nickname":null}"#,
    ));
    lines.extend(spawn(
        "c_text",
        12,
        "collab spawn failed: agent limit reached",
    ));
    lines.extend(spawn("c_array", 14, &format!(r#"["{child}"]"#)));
    lines.extend(spawn("c_number", 16, r#"{"agent_id":7}"#));
    // Not a `spawn_agent` call, with an output of the same shape.
    lines.push(call("exec_command", "c_exec", 18));
    lines.push(output("c_exec", 19, &agent_out(&child)));
    // No output yet.
    lines.push(call("spawn_agent", "c_open", 20));
    lines.push(turn_end("t1", 21));
    let path = dir
        .path()
        .join(format!("rollout-2026-10-09T09-00-00-{parent}.jsonl"));
    std::fs::write(&path, text(&lines)).unwrap();

    let adapter = Codex::without_decoded_copies(codex_source());
    let file = adapter.session_file(&path).unwrap();
    let index = adapter.turns(&file).unwrap();
    let found: HashMap<String, Option<String>> = index
        .turns
        .iter()
        .flat_map(|t| t.items.iter())
        .filter_map(|item| match item {
            TurnItem::Tool(call) => Some((
                call.id.0.to_string(),
                adapter
                    .spawned_thread(&file, call)
                    .unwrap()
                    .map(|c| c.to_string()),
            )),
            _ => None,
        })
        .collect();
    let mut expected: HashMap<String, Option<String>> = [
        "c_short", "c_braced", "c_v2", "c_text", "c_array", "c_number", "c_exec", "c_open",
    ]
    .into_iter()
    .map(|c| (c.to_owned(), None))
    .collect();
    expected.insert("c_v1".into(), Some(child.clone()));
    expected.insert("c_bare".into(), Some(child.clone()));
    assert_eq!(found, expected);
}

#[test]
fn spawn_link_fixtures_link_the_subagent_once_its_parent_is_indexed() {
    let mut h = Harness::new();
    for version in ["0.140.0-legacy", "0.140.0-paginated"] {
        copy_tree(
            &fixtures().join(format!("codex/{version}/sessions")),
            &h.codex_root(),
        );
    }
    h.start();
    assert_eq!(
        tool_call(&h, P2),
        None,
        "discovery does not read the parent"
    );
    h.take_seen(&codex_source());

    turns(&h, P1);
    assert_eq!(tool_call(&h, P2).as_deref(), Some("call_p1_0003"));
    let seen = h.take_seen(&codex_source());
    assert_eq!(seen.len(), 1);
    assert_eq!(&*seen[0].event.changed, [codex_key(P2)]);

    // The legacy main session spawned a thread that is not on this machine: the list stays.
    turns(&h, L1);
    assert!(h.take_seen(&codex_source()).is_empty());
    let links = h.codex_engine().links_of(&codex_key(L1)).unwrap().unwrap();
    assert_eq!(
        &*links.spawns,
        [(
            ToolCallId("call_l1_0010".into()),
            Some("cccccccc-0000-4000-8000-000000000001".into())
        )]
    );
}

/// The list after a discovery of both fixtures and a `turns` of the parent.
fn reference() -> String {
    let mut h = Harness::new();
    h.copy(&paginated(&p1_name()), &in_root(&h, &p1_name()));
    h.copy(&paginated(&p2_name()), &in_root(&h, &p2_name()));
    h.start();
    turns(&h, P1);
    let list = h.list(&codex_source());
    assert_eq!(tool_call(&h, P2).as_deref(), Some("call_p1_0003"));
    h.render(&list)
}

#[test]
fn spawn_link_order_of_arrival_does_not_matter() {
    let expected = reference();

    // The child is written before its parent.
    let mut h = Harness::new();
    h.copy(&paginated(&p2_name()), &in_root(&h, &p2_name()));
    h.start();
    let parent = in_root(&h, &p1_name());
    h.copy(&paginated(&p1_name()), &parent);
    emit_paths(&h, &[&parent]);
    turns(&h, P1);
    assert_eq!(h.render(&h.list(&codex_source())), expected, "child first");

    // The parent is written (and indexed) before its child.
    let mut h = Harness::new();
    h.copy(&paginated(&p1_name()), &in_root(&h, &p1_name()));
    h.start();
    turns(&h, P1);
    let child = in_root(&h, &p2_name());
    h.copy(&paginated(&p2_name()), &child);
    emit_paths(&h, &[&child]);
    assert_eq!(h.render(&h.list(&codex_source())), expected, "parent first");

    // The parent's output is appended after the child appeared; the parent's index is in memory.
    let mut h = Harness::new();
    let full = std::fs::read_to_string(paginated(&p1_name())).unwrap();
    let lines: Vec<String> = full.lines().map(str::to_owned).collect();
    let cut = lines
        .iter()
        .position(|l| l.contains("function_call_output") && l.contains("call_p1_0003"))
        .unwrap();
    let parent = in_root(&h, &p1_name());
    std::fs::create_dir_all(parent.parent().unwrap()).unwrap();
    std::fs::write(&parent, text(&lines[..cut])).unwrap();
    h.start();
    turns(&h, P1);
    let child = in_root(&h, &p2_name());
    h.copy(&paginated(&p2_name()), &child);
    emit_paths(&h, &[&child]);
    assert_eq!(tool_call(&h, P2), None, "the output is not written yet");
    h.take_seen(&codex_source());
    append(&parent, &lines[cut..]);
    emit_paths(&h, &[&parent]);
    assert_eq!(h.render(&h.list(&codex_source())), expected, "output last");
    let seen = h.take_seen(&codex_source());
    assert!(seen.last().unwrap().event.changed.contains(&codex_key(P2)));
}

#[test]
fn spawn_link_active_parents_are_indexed_in_the_background_one_at_a_time() {
    let mut h = Harness::new();
    let (a, a1, a2) = (uuid(0x10), uuid(0x11), uuid(0x12));
    let (b, b1) = (uuid(0x20), uuid(0x21));
    let (c, c1) = (uuid(0x30), uuid(0x31));
    // A and C have an open turn; B ended a day before "now".
    let a_path = parent(&h, &a, &[&a1], false);
    parent(&h, &b, &[&b1], true);
    parent(&h, &c, &[&c1], false);
    for (kid, of) in [(&a1, &a), (&b1, &b), (&c1, &c)] {
        child(&h, kid, of);
    }
    h.start_at(at(0) + 24 * 60 * MINUTE);
    assert_eq!(tool_call(&h, &a1).as_deref(), Some("call_0"));
    assert_eq!(tool_call(&h, &c1).as_deref(), Some("call_0"));
    assert_eq!(tool_call(&h, &b1), None, "an inactive parent is not read");
    let engine = h.codex_engine();
    assert_eq!(
        calls(&h),
        (0, 2, 2),
        "one index build and one load for A and C"
    );
    assert_eq!(engine.max_builds_in_flight(), 1);

    // Later spawns of A are derived by `follow` from the index in memory.
    append(&a_path, &spawn("call_1", 10, &agent_out(&a2)));
    let a2_path = child(&h, &a2, &a);
    emit_paths(&h, &[&a_path, &a2_path]);
    assert_eq!(tool_call(&h, &a2).as_deref(), Some("call_1"));
    assert_eq!(calls(&h), (0, 3, 3), "the follow of A, no new build");
}

#[test]
fn spawn_link_a_compressed_parent_is_never_indexed_in_the_background() {
    let mut h = Harness::new();
    let (p, kid) = (uuid(1), uuid(2));
    // An open turn and a modification time of "now": active, if it were not a cold file.
    let jsonl = parent(&h, &p, &[&kid], false);
    compress(&jsonl);
    std::fs::remove_file(&jsonl).unwrap();
    child(&h, &kid, &p);
    h.start();
    let engine = h.codex_engine();
    let summary = h.list(&codex_source()).get(&codex_key(&p)).unwrap().clone();
    assert!(summary.open_turn, "completed from the archive");
    assert_eq!(tool_call(&h, &kid), None);
    assert_eq!(calls(&h), (0, 0, 0), "no turns_after, no load");
    assert_eq!(engine.adapter.decodes(), 0, "no decoded copy");
    assert_eq!(engine.max_builds_in_flight(), 0);
}

#[test]
fn spawn_link_a_parent_is_active_for_ten_minutes_after_its_last_activity() {
    for (after, linked) in [(9 * MINUTE, true), (11 * MINUTE, false)] {
        let mut h = Harness::new();
        let (p, kid) = (uuid(1), uuid(2));
        parent(&h, &p, &[&kid], true);
        child(&h, &kid, &p);
        h.start_at(at(30) + after);
        assert_eq!(tool_call(&h, &kid).is_some(), linked, "{after} ms after");
        assert_eq!(calls(&h).1, usize::from(linked));
    }
}

#[test]
fn spawn_link_an_active_parent_is_queued_at_most_once_in_ten_minutes() {
    let mut h = Harness::new();
    let (p, kid) = (uuid(1), uuid(2));
    // A multi_agent v2 output: no thread ID, so the child never gets linked.
    let mut lines = vec![meta(&p, r#""cli""#)];
    lines.extend(turn_start("t1", 1, "parent prompt"));
    lines.extend(spawn(
        "call_0",
        2,
        r#"{"task_name":"helper","nickname":null}"#,
    ));
    let path = write(&h, &p, &lines);
    child(&h, &kid, &p);
    let now = at(0) + 60 * MINUTE;
    h.start_at(now);
    assert_eq!(calls(&h).1, 1);
    assert_eq!(tool_call(&h, &kid), None);

    // The index leaves memory; appends only redo the summary, and the parent is not queued again
    // within ten minutes.
    let engine = h.codex_engine();
    engine.memory_indexes().remove(&codex_key(&p));
    append(
        &path,
        &[line(
            "event_msg",
            5,
            r#"{"type":"agent_message","message":"x"}"#,
        )],
    );
    emit_paths(&h, &[&path]);
    assert_eq!(calls(&h).1, 1);

    h.set_codex_clock(now + 11 * MINUTE);
    append(
        &path,
        &[line(
            "event_msg",
            6,
            r#"{"type":"agent_message","message":"y"}"#,
        )],
    );
    emit_paths(&h, &[&path]);
    assert_eq!(calls(&h).1, 2, "queued again after ten minutes");
}

#[test]
fn spawn_link_restart_takes_the_links_from_agents_spawns() {
    let mut h = Harness::new();
    let parent = in_root(&h, &p1_name());
    h.copy(&paginated(&p1_name()), &parent);
    h.copy(&paginated(&p2_name()), &in_root(&h, &p2_name()));
    h.start();
    turns(&h, P1);
    assert!(tool_call(&h, P2).is_some());
    h.stop();

    h.start();
    let seen = h.take_seen(&codex_source());
    assert_eq!(seen.len(), 1, "linked in the first publication");
    let child = seen[0].list.get(&codex_key(P2)).unwrap();
    assert_eq!(
        child.parent.as_ref().unwrap().tool_call,
        Some(ToolCallId("call_p1_0003".into()))
    );
    assert_eq!(calls(&h), (0, 0, 0), "no read, turns_after or load");
    h.stop();

    // An entry past the end of the parent file is dropped.
    let len = std::fs::metadata(&parent).unwrap().len();
    let entry = |offset| CachedSpawns {
        offset,
        spawns: vec![("call_p1_0003".into(), Some(P2.into()))],
    };
    {
        let cache = cache(&h);
        let _ = cache.app().put(
            &spawns_key(&codex_key(P1), codex::ADAPTER_VERSION),
            &entry(len + 1),
        );
    }
    h.start();
    assert_eq!(tool_call(&h, P2), None, "offset past the end");
    h.stop();

    // An entry of another adapter version is not a hit.
    {
        let cache = cache(&h);
        let app = cache.app();
        let _ = app.remove::<CachedSpawns>(&spawns_key(&codex_key(P1), codex::ADAPTER_VERSION));
        let _ = app.put(
            &spawns_key(&codex_key(P1), codex::ADAPTER_VERSION + 1),
            &entry(len),
        );
    }
    h.start();
    assert_eq!(tool_call(&h, P2), None, "another adapter version");
    assert_eq!(calls(&h), (0, 0, 0));
}

#[test]
fn spawn_link_survives_archiving_and_follows_the_parent_away_and_back() {
    let mut h = Harness::new();
    let archive = h.base.join("codex/archived_sessions");
    std::fs::create_dir_all(&archive).unwrap();
    let parent = in_root(&h, &p1_name());
    h.copy(&paginated(&p1_name()), &parent);
    let child = in_root(&h, &p2_name());
    h.copy(&paginated(&p2_name()), &child);
    h.start();
    turns(&h, P1);
    assert!(tool_call(&h, P2).is_some());
    let engine = h.codex_engine();

    // Codex compresses the parent and moves it to the archive.
    let zst = compress(&parent);
    std::fs::remove_file(&parent).unwrap();
    let archived = archive.join(zst.file_name().unwrap());
    std::fs::rename(&zst, &archived).unwrap();
    let name = archived.file_name().unwrap().to_str().unwrap().to_owned();
    let backend = &h.run().backend;
    backend.emit(
        &h.codex_root(),
        &[
            (&h.rel_to(&h.codex_root(), &parent), F),
            (&h.rel_to(&h.codex_root(), &zst), F),
        ],
    );
    backend.emit(&archive, &[(&name, F)]);
    h.advance();
    let summary = h.list(&codex_source()).get(&codex_key(P1)).unwrap().clone();
    assert!(summary.archived);
    assert_eq!(tool_call(&h, P2).as_deref(), Some("call_p1_0003"));
    assert_eq!(
        engine.adapter.decodes(),
        0,
        "the archive is not decompressed"
    );

    // Deleted: no tool call, the parent key stays.
    let bytes = std::fs::read(&archived).unwrap();
    std::fs::remove_file(&archived).unwrap();
    h.emit(&archive, &[(&name, F)]);
    assert!(h.list(&codex_source()).get(&codex_key(P1)).is_none());
    assert_eq!(tool_call(&h, P2), None);
    let list = h.list(&codex_source());
    let summary = list.get(&codex_key(P2)).unwrap();
    assert_eq!(summary.parent.as_ref().unwrap().session, codex_key(P1));

    // Restored: linked again, from `agents.spawns`.
    std::fs::write(&archived, bytes).unwrap();
    h.emit(&archive, &[(&name, F)]);
    assert_eq!(tool_call(&h, P2).as_deref(), Some("call_p1_0003"));
    assert_eq!(engine.adapter.decodes(), 0);

    // A hidden parent counts as gone.
    h.filter.hide(codex_key(P1));
    emit_paths(&h, &[&child]);
    assert!(h.list(&codex_source()).get(&codex_key(P1)).is_none());
    assert_eq!(tool_call(&h, P2), None);
}

#[test]
fn spawn_link_rewritten_parent_is_derived_again() {
    let mut h = Harness::new();
    let (p, k1, k2) = (uuid(1), uuid(2), uuid(3));
    let path = parent(&h, &p, &[&k1], true);
    child(&h, &k1, &p);
    child(&h, &k2, &p);
    h.start();
    turns(&h, &p);
    assert_eq!(tool_call(&h, &k1).as_deref(), Some("call_0"));
    assert_eq!(tool_call(&h, &k2), None);

    // Replaced by another file whose `call_0` spawned the other child.
    let mut lines = vec![meta(&p, r#""cli""#)];
    lines.extend(turn_start(
        "t9",
        1,
        "a replaced parent prompt, longer than before",
    ));
    lines.extend(spawn("call_0", 2, &agent_out(&k2)));
    lines.push(turn_end("t9", 9));
    let tmp = h.base.join("replacement.jsonl");
    std::fs::write(&tmp, text(&lines)).unwrap();
    std::fs::rename(&tmp, &path).unwrap();
    emit_paths(&h, &[&path]);
    turns(&h, &p);
    assert_eq!(tool_call(&h, &k1), None, "the old link does not stay");
    assert_eq!(tool_call(&h, &k2).as_deref(), Some("call_0"));
}

#[test]
fn spawn_link_a_late_derivation_from_an_older_index_is_dropped() {
    let mut h = Harness::new();
    let (p, k1, k2) = (uuid(1), uuid(2), uuid(3));
    let path = parent(&h, &p, &[&k1], false);
    child(&h, &k1, &p);
    h.start();
    let engine = h.codex_engine();
    let file = engine.file_by_id(&p).unwrap();
    let adapter = Codex::without_decoded_copies(codex_source());
    let older = adapter.turns(&file).unwrap();
    append(&path, &spawn("call_1", 10, &agent_out(&k2)));
    let newer = adapter.turns(&file).unwrap();

    assert!(engine.derive_links(&file, &newer, 1_000, false));
    assert!(!engine.derive_links(&file, &older, 999, false));
    let links = engine.links_of(&codex_key(&p)).unwrap().unwrap();
    assert_eq!(links.seq, 1_000);
    assert_eq!(links.spawns.len(), 2);
    assert_eq!(links.offset, std::fs::metadata(&path).unwrap().len());
}

#[test]
fn spawn_link_matches_the_declared_parent_and_the_first_call() {
    let mut h = Harness::new();
    let (x, y) = (uuid(1), uuid(2));
    let (k1, k2, k3) = (uuid(3), uuid(4), uuid(5));
    // X spawned K1 twice and K2 once; K2 says Y is its parent.
    let mut lines = vec![meta(&x, r#""cli""#)];
    lines.extend(turn_start("t1", 1, "parent prompt"));
    lines.extend(spawn("call_a", 2, &agent_out(&k1)));
    lines.extend(spawn("call_b", 4, &agent_out(&k1)));
    lines.extend(spawn("call_c", 6, &agent_out(&k2)));
    lines.push(turn_end("t1", 8));
    write(&h, &x, &lines);
    parent(&h, &y, &[], true);
    child(&h, &k1, &x);
    child(&h, &k2, &y);
    // A subagent without a parent thread.
    child_with(&h, &k3, r#"{"subagent":"review"}"#);
    h.start();
    turns(&h, &x);
    turns(&h, &y);
    assert_eq!(
        tool_call(&h, &k1).as_deref(),
        Some("call_a"),
        "the first call"
    );
    assert_eq!(tool_call(&h, &k2), None, "not the declared parent");
    assert_eq!(tool_call(&h, &k3), None);
    let list = h.list(&codex_source());
    let review = list.get(&codex_key(&k3)).unwrap();
    assert!(review.parent.as_ref().unwrap().session.id.is_empty());
}

#[test]
fn spawn_link_entries_round_trip_through_the_cache() {
    let h = Harness::new();
    let cache = cache(&h);
    let app = cache.app();
    let k = spawns_key(&codex_key(P1), codex::ADAPTER_VERSION);
    let entry = CachedSpawns {
        offset: 42,
        spawns: vec![
            (Arc::from("call_1"), Some(Arc::from(P2))),
            (Arc::from("call_2"), None),
        ],
    };
    assert_eq!(app.put(&k, &entry), lectern_cache::Written::Committed);
    assert_eq!(app.get::<CachedSpawns>(&k), Some(entry));
}
