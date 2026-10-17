//! `agents/*` (docs/design/agents.md「性能」). Sessions are the synthetic
//! fixtures of `lectern_agents::testing`, repeated to 50 MB.

use std::cell::Cell;
use std::fs::OpenOptions;
use std::hint::black_box;
use std::io::Write as _;
use std::path::Path;

use lectern_agents::AgentRoots;
use lectern_agents::claude::{ClaudeCode, SOURCE_ID};
use lectern_agents::codex::{self, Codex};
use lectern_agents::testing::{
    append_chunk, compress_zst, parse_file, source, synthetic_long_session, synthetic_projects,
    synthetic_rollout, synthetic_session,
};
use lectern_bench::{Bench, BenchError, Ctx, Measured, Value};
use lectern_cache::{Budgets, Cache, Config};
use lectern_services::session::{Agent, Cursor, SessionKey, SourceId, TurnItem};

/// The main session's ID in the fixtures; the file name makes
/// `ClaudeCode::session_file` recognise a main session.
const MAIN: &str = "11111111-1111-4111-8111-111111111111";
/// A Codex rollout file name, from which `Codex::session_file` takes the
/// thread ID.
const ROLLOUT: &str = "rollout-2026-10-02T09-00-00-aaaaaaaa-0000-4000-8000-000000000001.jsonl.zst";

fn session_len(ctx: &Ctx) -> usize {
    ctx.scale(50 << 20, 256 << 10)
}

/// Whole-file parse (records and turn index).
pub const PARSE_THROUGHPUT: Bench = Bench {
    id: "agents/parse_throughput",
    run: parse_throughput,
};

fn parse_throughput(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let data = synthetic_session(session_len(ctx));
    let bytes = data.len() as u64;
    let path = ctx.scratch().join(format!("{MAIN}.jsonl"));
    std::fs::write(&path, data)?;
    parse_file(Agent::ClaudeCode, &path).map_err(|e| BenchError::debug("parse", e))?;
    let time = ctx.time(|| parse_file(Agent::ClaudeCode, black_box(&path)));
    Ok(vec![Measured::new(
        "agents/parse_throughput",
        Value::Throughput { bytes, time },
    )])
}

/// Append about 4 KB of whole lines to a 50 MB session, then `read` from the
/// previous cursor to the end.
pub const APPEND_4K: Bench = Bench {
    id: "agents/append_4k",
    run: append_4k,
};

fn append(path: &Path, chunk: &[u8]) -> std::io::Result<()> {
    OpenOptions::new().append(true).open(path)?.write_all(chunk)
}

fn append_4k(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let path = ctx.scratch().join(format!("{MAIN}.jsonl"));
    std::fs::write(&path, synthetic_session(session_len(ctx)))?;
    let adapter = ClaudeCode::new(SourceId(SOURCE_ID.into()));
    let file = adapter
        .session_file(&path)
        .ok_or_else(|| BenchError::new("the session file is not recognised"))?;
    let mut cursor: Cursor = adapter
        .read(&file, &Cursor::START, u64::MAX)
        .map_err(|e| BenchError::debug("read", e))?
        .batch
        .cursor;
    let chunk = append_chunk();
    // Checked outside the clock: every read reaches the end without a reset.
    let failure: Cell<Option<String>> = Cell::new(None);
    let time = ctx.time_with_setup(
        || append(&path, chunk),
        |appended| {
            if let Err(error) = appended {
                failure.set(Some(format!("append: {error}")));
            }
            let parsed = adapter.read(&file, black_box(&cursor), u64::MAX);
            match &parsed {
                Ok(p) if p.batch.at_end && !p.batch.reset => cursor = p.batch.cursor.clone(),
                Ok(_) => failure.set(Some("read did not reach the end, or reset".into())),
                Err(e) => failure.set(Some(format!("read: {e:?}"))),
            }
            parsed
        },
    );
    if let Some(message) = failure.take() {
        return Err(BenchError::new(message));
    }
    Ok(vec![Measured::new("agents/append_4k", Value::Time(time))])
}

/// Writes a `.jsonl.zst` of the synthetic rollout; returns its path and
/// the decompressed length.
fn rollout(ctx: &Ctx) -> Result<(std::path::PathBuf, u64), BenchError> {
    let data = synthetic_rollout(session_len(ctx));
    let path = ctx.scratch().join(ROLLOUT);
    std::fs::write(&path, compress_zst(&data))?;
    Ok((path, data.len() as u64))
}

/// Decompress and parse a whole `.jsonl.zst` from the start, without a
/// decoded copy; throughput in decompressed bytes.
pub const ZST_THROUGHPUT: Bench = Bench {
    id: "agents/zst_throughput",
    run: zst_throughput,
};

fn zst_throughput(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (path, bytes) = rollout(ctx)?;
    parse_file(Agent::Codex, &path).map_err(|e| BenchError::debug("parse", e))?;
    let time = ctx.time(|| parse_file(Agent::Codex, black_box(&path)));
    Ok(vec![Measured::new(
        "agents/zst_throughput",
        Value::Throughput { bytes, time },
    )])
}

/// One `load` once the decoded copy exists (built by the first `turns`),
/// over the references of the last 200 turns.
pub const ZST_LOAD_DECODED: Bench = Bench {
    id: "agents/zst_load_decoded",
    run: zst_load_decoded,
};

fn zst_load_decoded(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (path, _) = rollout(ctx)?;
    let adapter = Codex::new(
        SourceId(codex::SOURCE_ID.into()),
        ctx.scratch().join("decoded"),
    );
    let file = adapter
        .session_file(&path)
        .ok_or_else(|| BenchError::new("the rollout file is not recognised"))?;
    let index = adapter
        .turns(&file)
        .map_err(|e| BenchError::debug("turns", e))?;
    let refs: Vec<_> = index
        .turns
        .iter()
        .rev()
        .take(200)
        .flat_map(|t| t.items.iter())
        .map(|item| match item {
            TurnItem::Tool(call) => call.input,
            TurnItem::AgentText(r) | TurnItem::Injected(r) => *r,
        })
        .collect();
    if refs.is_empty() {
        return Err(BenchError::new("the rollout has no content references"));
    }
    for r in &refs {
        adapter
            .load(&file, r)
            .map_err(|e| BenchError::debug("load", e))?;
    }
    let mut next = 0;
    let time = ctx.time(|| {
        let r = &refs[next % refs.len()];
        next += 1;
        adapter.load(&file, black_box(r))
    });
    Ok(vec![Measured::new(
        "agents/zst_load_decoded",
        Value::Time(time),
    )])
}

/// Roots with one Claude Code config directory under the scratch directory.
fn claude_roots(ctx: &Ctx, name: &str) -> AgentRoots {
    AgentRoots {
        claude: vec![ctx.scratch().join(name)],
        codex: Vec::new(),
    }
}

/// The first discovery of 1000 Claude Code session files (50 project
/// directories): enumerate, read each file's head and tail, build and sort
/// the list. No placement (`Projects` belongs to `project`) and no watch.
pub const DISCOVER_1000: Bench = Bench {
    id: "agents/discover_1000",
    run: discover_1000,
};

fn discover_1000(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let count = ctx.scale(1000, 20);
    let roots = claude_roots(ctx, "discover");
    synthetic_projects(&roots.claude[0].join("projects"), count)?;
    let decoded = ctx.scratch().join("decoded");
    let found = source(Agent::ClaudeCode, &roots, None, None, &decoded)
        .ok_or_else(|| BenchError::new("no Claude Code source"))?
        .sessions()
        .sessions
        .len();
    if found != count {
        return Err(BenchError::new(format!(
            "discovered {found} of {count} sessions"
        )));
    }
    let time = ctx.time(|| source(Agent::ClaudeCode, black_box(&roots), None, None, &decoded));
    Ok(vec![Measured::new(
        "agents/discover_1000",
        Value::Time(time),
    )])
}

/// Opening a 50 MB session (`synthetic_long_session`: as many tool calls
/// per megabyte as the design's reference session) whose turn index is in
/// the cache but not in memory (after a restart), with about 4 KB appended
/// since: `turns` reads
/// the cached index, checks its cursor and parses the appended part. The
/// updated index is written back afterwards (a background task in the
/// plugin), outside the clock.
pub const TURNS_CACHED_50MB: Bench = Bench {
    id: "agents/turns_cached_50mb",
    run: turns_cached_50mb,
};

fn turns_cached_50mb(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let roots = claude_roots(ctx, "turns");
    let dir = roots.claude[0].join("projects/-Users-dev-fixture-repo");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{MAIN}.jsonl"));
    std::fs::write(&path, synthetic_long_session(session_len(ctx)))?;
    let decoded = ctx.scratch().join("decoded");
    let cache = Cache::new(Config {
        root: ctx.scratch().join("cache"),
        budgets: Budgets::default(),
        ui_thread: None,
    });
    let key = SessionKey {
        source: SourceId(SOURCE_ID.into()),
        id: MAIN.into(),
    };
    let open = || {
        source(Agent::ClaudeCode, &roots, Some(&cache), None, &decoded)
            .ok_or_else(|| BenchError::new("no Claude Code source"))
    };
    // The first open parses the whole file and fills the cache.
    let built = open()?
        .turns(&key)
        .map_err(|e| BenchError::debug("turns", e))?;
    let chunk = append_chunk();
    let failure: Cell<Option<String>> = Cell::new(None);
    let time = ctx.time_with_setup(
        || {
            // Outside the clock: append, then a fresh source with nothing in memory.
            if let Err(error) = append(&path, chunk) {
                failure.set(Some(format!("append: {error}")));
            }
            open()
        },
        |opened| {
            let opened = opened.ok();
            let index = opened.as_ref().map(|s| s.turns(black_box(&key)));
            match &index {
                Some(Ok(index)) if index.turns.len() > built.turns.len() => {}
                Some(Ok(_)) => failure.set(Some("the appended turns are missing".into())),
                Some(Err(e)) => failure.set(Some(format!("turns: {e:?}"))),
                None => failure.set(Some("no source".into())),
            }
            // Dropped after the clock stops: the source writes the updated index then, where the
            // plugin writes it in a background task.
            (opened, index)
        },
    );
    if let Some(message) = failure.take() {
        return Err(BenchError::new(message));
    }
    Ok(vec![Measured::new(
        "agents/turns_cached_50mb",
        Value::Time(time),
    )])
}
