//! 压缩会话的补齐（`complete_*`）：冷缓存下只主动补齐最近的 8 个，之后 kernel 静止；其余在 `complete` 之后补齐。

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use lectern_services::session::SessionKey;

use super::support::*;
use crate::source::{AUTO_COMPLETE, MAX_COMPLETING};

/// 一个最小的 Codex rollout（一轮，已结束），压缩成 `.jsonl.zst`，修改时间为 `mtime`。
fn write_rollout(h: &Harness, n: u32, mtime: SystemTime) -> (SessionKey, PathBuf) {
    let id = uuid(n);
    let dir = h.codex_root().join("2026/10/09");
    std::fs::create_dir_all(&dir).unwrap();
    let cwd = "/Users/dev/fixture/repo";
    let lines = [
        format!(
            r#"{{"timestamp":"2026-10-09T09:00:00.000Z","type":"session_meta","payload":{{"id":"{id}","timestamp":"2026-10-09T09:00:00.000Z","cwd":"{cwd}","cli_version":"0.140.0","source":"cli","history_mode":"legacy"}}}}"#
        ),
        format!(
            r#"{{"timestamp":"2026-10-09T09:00:01.000Z","type":"turn_context","payload":{{"turn_id":"t-{n}","cwd":"{cwd}","model":"fixture-model-x"}}}}"#
        ),
        format!(
            r#"{{"timestamp":"2026-10-09T09:00:02.000Z","type":"event_msg","payload":{{"type":"task_started","turn_id":"t-{n}"}}}}"#
        ),
        format!(
            r#"{{"timestamp":"2026-10-09T09:00:03.000Z","type":"event_msg","payload":{{"type":"user_message","message":"prompt {n}","images":[]}}}}"#
        ),
        format!(
            r#"{{"timestamp":"2026-10-09T09:00:04.000Z","type":"event_msg","payload":{{"type":"task_complete","turn_id":"t-{n}"}}}}"#
        ),
    ];
    let path = dir.join(format!("rollout-2026-10-09T09-00-00-{id}.jsonl"));
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();
    let zst = compress(&path);
    std::fs::remove_file(&path).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&zst)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    (key(&codex_source(), &id), zst)
}

#[test]
fn complete_cold_start_completes_only_the_most_recent_eight() {
    assert_eq!((AUTO_COMPLETE, MAX_COMPLETING), (8, 2));
    let mut h = Harness::new();
    let epoch = SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000);
    let keys: Vec<SessionKey> = (0..100)
        .map(|n| write_rollout(&h, n, epoch + Duration::from_secs(u64::from(n))).0)
        .collect();
    h.start();

    let seen = h.take_seen(&codex_source());
    let first = &seen[0].list;
    assert_eq!(first.sessions.len(), 100);
    assert!(
        first.sessions.iter().all(|s| s.tail_pending),
        "the list is published before any full decode"
    );
    let list = h.list(&codex_source());
    let mut done: Vec<&SessionKey> = list
        .sessions
        .iter()
        .filter(|s| !s.tail_pending)
        .map(|s| &s.key)
        .collect();
    done.sort();
    let mut newest: Vec<&SessionKey> = keys[92..].iter().collect();
    newest.sort();
    assert_eq!(done, newest, "the eight most recently modified");
    // Nothing else is queued: the kernel is quiescent once these finished.
    assert_eq!(h.run().executor.run_until_idle(), 0);
    h.run().kernel.barrier();
    assert!(h.quiescent());

    // A view shows an old one: it is completed on request.
    h.source(&codex_source())
        .complete(&[keys[3].clone(), keys[99].clone()]);
    h.settle();
    let list = h.list(&codex_source());
    assert!(!list.get(&keys[3]).unwrap().tail_pending);
    assert_eq!(
        list.sessions.iter().filter(|s| !s.tail_pending).count(),
        9,
        "completed sessions are ignored"
    );
    assert!(h.quiescent());
}

#[test]
fn complete_runs_at_most_two_tasks_at_once() {
    let mut h = Harness::new();
    let epoch = SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000);
    for n in 0..4 {
        write_rollout(&h, n, epoch + Duration::from_secs(u64::from(n)));
    }
    h.start();
    let list = h.list(&codex_source());
    assert!(list.sessions.iter().all(|s| !s.tail_pending));
    let engine = h.codex_engine();
    assert_eq!(engine.completion_running(), 0);
    assert_eq!(
        engine
            .max_completing
            .load(std::sync::atomic::Ordering::Relaxed),
        MAX_COMPLETING,
        "four sessions, two tasks at a time"
    );
}
