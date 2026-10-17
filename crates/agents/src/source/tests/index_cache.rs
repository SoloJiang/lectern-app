//! 轮次索引缓存（`index_cache_*`）：命中后只解析追加部分、版本变化后不命中、`read` 顺带建索引、内存 LRU。

use std::io::{Seek, SeekFrom, Write as _};
use std::path::PathBuf;
use std::sync::Arc;

use lectern_cache::Cache;
use lectern_services::session::{
    Cursor, Format, SessionKey, SourceId, Timestamp, Turn, TurnId, TurnIndex,
};

use super::support::*;
use crate::claude;
use crate::source::cached::{CachedIndex, Indexes, MEMORY_BUDGET, estimate, index_key};

const PROMPTS: [&str; 3] = ["first prompt", "second prompt", "third prompt"];

fn write_session(h: &Harness) -> (SessionKey, PathBuf) {
    let id = uuid(7);
    let dir = h.claude_root().join("-work");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{id}.jsonl"));
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

#[test]
fn index_cache_hit_parses_only_the_appended_part() {
    let mut h = Harness::new();
    let (session, path) = write_session(&h);
    h.start();
    let first = h.source(&claude_source()).turns(&session).unwrap();
    assert_eq!(previews(&first), PROMPTS);
    h.stop();

    // Rewrite the second prompt in place, same length, far from the last 256 bytes the cursor
    // checks: a full parse would see it, a cache hit does not.
    let text = std::fs::read_to_string(&path).unwrap();
    let at = text.find("second prompt").unwrap();
    let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.seek(SeekFrom::Start(at as u64)).unwrap();
    f.write_all(b"SECOND PROMPT").unwrap();
    // And append a fourth turn.
    let id = session.id.to_string();
    f.seek(SeekFrom::End(0)).unwrap();
    f.write_all(claude_turn(&id, &h.base.join("work"), "fourth prompt", 40, 3).as_bytes())
        .unwrap();
    drop(f);

    h.start();
    let index = h.source(&claude_source()).turns(&session).unwrap();
    assert_eq!(
        previews(&index),
        [
            "first prompt",
            "second prompt",
            "third prompt",
            "fourth prompt"
        ],
        "the cached turns plus the appended one"
    );
    assert_eq!(
        index.cursor.offset(),
        std::fs::metadata(&path).unwrap().len()
    );
    // The memory copy is used from now on: the same `Arc`.
    let again = h.source(&claude_source()).turns(&session).unwrap();
    assert!(Arc::ptr_eq(&index, &again));
}

#[test]
fn index_cache_misses_after_an_adapter_version_change() {
    let mut h = Harness::new();
    let (session, _) = write_session(&h);
    h.start();
    let real = h.source(&claude_source()).turns(&session).unwrap();
    h.stop();

    let stale = |cursor: Cursor| {
        let mut turns = real.turns.to_vec();
        turns.push(Turn {
            id: TurnId("stale".into()),
            ordinal: 99,
            started_at: Timestamp(0),
            ended_at: None,
            prompt: None,
            prompt_preview: "stale".into(),
            items: Arc::new([]),
        });
        TurnIndex {
            session: session.clone(),
            cursor,
            turns: turns.into(),
            format: Format::Known,
        }
    };
    {
        let cache = cache(&h);
        let app = cache.app();
        // Under the key of another adapter version.
        let _ = app.put(
            &index_key(&session, claude::ADAPTER_VERSION + 1),
            &CachedIndex(stale(real.cursor.clone())),
        );
        // Under the current key, with a cursor written by another adapter version.
        let other = Cursor::new(
            SourceId(claude::SOURCE_ID.into()),
            claude::ADAPTER_VERSION + 1,
            real.cursor.offset(),
            real.cursor.state().to_vec(),
        );
        let _ = app.put(
            &index_key(&session, claude::ADAPTER_VERSION),
            &CachedIndex(stale(other)),
        );
    }
    h.start();
    let index = h.source(&claude_source()).turns(&session).unwrap();
    assert_eq!(*index, *real, "rebuilt from the file");
}

#[test]
fn index_cache_read_builds_the_index_on_the_way() {
    let mut h = Harness::new();
    let (session, _) = write_session(&h);
    h.start();
    let source = h.source(&claude_source());
    let mut cursor = Cursor::START;
    loop {
        let batch = source.read(&session, &cursor, 300).unwrap();
        cursor = batch.cursor;
        if batch.at_end {
            break;
        }
    }
    let engine = h.claude_engine();
    let (held, _) = engine
        .memory_indexes()
        .get(&session)
        .expect("built by read");
    assert_eq!(held.cursor, cursor);
    // `turns` hits memory and does not parse again.
    let index = source.turns(&session).unwrap();
    assert!(Arc::ptr_eq(&index, &held));
    let adapter = claude::ClaudeCode::new(claude_source());
    let file = adapter
        .session_file(&h.claude_root().join(format!("-work/{}.jsonl", session.id)))
        .unwrap();
    assert_eq!(*index, adapter.turns(&file).unwrap());
    drop(source);
    h.settle();

    // Written to the cache at the end of the reads, by a background task.
    let cached: Option<CachedIndex> = cache(&h)
        .app()
        .get(&index_key(&session, claude::ADAPTER_VERSION));
    assert_eq!(cached.unwrap().0, *index);
}

#[test]
fn index_cache_memory_is_a_byte_lru() {
    let big = |n: u32| {
        let turns: Vec<Turn> = (0..2000)
            .map(|i| Turn {
                id: TurnId(format!("turn-{i}").into()),
                ordinal: i + 1,
                started_at: Timestamp(0),
                ended_at: None,
                prompt: None,
                prompt_preview: "x".repeat(120).into(),
                items: Arc::new([]),
            })
            .collect();
        Arc::new(TurnIndex {
            session: key(&claude_source(), &uuid(n)),
            cursor: Cursor::START,
            turns: turns.into(),
            format: Format::Known,
        })
    };
    let one = estimate(&big(0));
    assert!(one > 100_000 && one < MEMORY_BUDGET / 4);
    let mut lru = Indexes::default();
    let n = (MEMORY_BUDGET / one) as u32 + 4;
    for i in 0..n {
        lru.put(big(i), None);
        // Keep the first one in use.
        lru.get(&key(&claude_source(), &uuid(0)));
    }
    assert!(lru.bytes() <= MEMORY_BUDGET);
    assert!(
        lru.contains(&key(&claude_source(), &uuid(0))),
        "recently used"
    );
    assert!(lru.contains(&key(&claude_source(), &uuid(n - 1))), "newest");
    assert!(
        !lru.contains(&key(&claude_source(), &uuid(1))),
        "oldest unused"
    );
}
