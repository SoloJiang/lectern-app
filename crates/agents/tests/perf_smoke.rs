//! agents 的性能冒烟测试（docs/design/agents.md「性能」）。`bench` 的设计文档（#7）合并之前，
//! 预算基准先以被忽略的测试运行：
//!
//! ```sh
//! cargo test -p lectern-agents --features test-support --release --test perf_smoke -- --ignored --nocapture
//! ```
//!
//!
//! - `agents/parse_throughput`：fixtures 拼成约 50 MB，整文件解析 ≥ 200 MB/s。
//! - `agents/append_4k`：50 MB 的会话追加约 4 KB 之后，从上一次的游标 `read` 一次 < 1 ms。
//! - `agents/zst_throughput`：Codex fixtures 拼成约 50 MB 再压缩成 `.jsonl.zst`，解压加整文件解析
//!   ≥ 100 MB/s（按解压后的字节计）。
//! - `agents/zst_load_decoded`：同一个 `.jsonl.zst` 的解压副本建好之后，`load` 一次 < 1 ms。
//!
//! fixtures 是合成的 Claude Code 2.1.302 会话（`crates/agents/tests/fixtures/claude-code/2.1.302/`）
//! 和 Codex 0.140.0 rollout（`crates/agents/tests/fixtures/codex/`）。

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lectern_agents::claude::{ClaudeCode, SOURCE_ID};
use lectern_agents::codex::{self, Codex};
use lectern_agents::testing::parse_file;
use lectern_services::session::{Agent, Cursor, SourceId, TurnItem};

const TARGET: usize = 50 << 20;
const MAIN: &str = "11111111-1111-4111-8111-111111111111";

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/claude-code/2.1.302/projects/-Users-dev-fixture-repo")
}

/// fixtures 里的全部会话文件依次拼接，重复到约 `TARGET` 字节，写进一个按主会话命名的临时文件。
fn build(dir: &Path) -> (PathBuf, Vec<u8>) {
    let root = fixture_dir();
    let mut sample = Vec::new();
    for rel in [
        format!("{MAIN}.jsonl"),
        format!("{MAIN}/subagents/agent-a0fa0001.jsonl"),
        format!("{MAIN}/subagents/workflows/wf-fixture/agent-a0fa0002.jsonl"),
        "22222222-2222-4222-8222-222222222222.jsonl".to_owned(),
    ] {
        sample.extend(std::fs::read(root.join(rel)).expect("fixture"));
    }
    let mut data = Vec::with_capacity(TARGET + sample.len());
    while data.len() < TARGET {
        data.extend_from_slice(&sample);
    }
    let path = dir.join(format!("{MAIN}.jsonl"));
    std::fs::write(&path, &data).expect("write the bench session");
    (path, sample)
}

/// Codex 的 fixtures（主会话，不含需要来源会话的分叉）依次拼接，重复到约 `TARGET` 字节，压缩成
/// 一个 `.jsonl.zst`。返回压缩文件的路径和解压后的字节数。
fn build_codex(dir: &Path) -> (PathBuf, usize) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex");
    let mut sample = Vec::new();
    for rel in [
        "0.140.0-legacy/sessions/2026/10/02/rollout-2026-10-02T09-00-00-aaaaaaaa-0000-4000-8000-000000000001.jsonl",
        "0.140.0-paginated/sessions/2026/10/05/rollout-2026-10-05T09-00-00-bbbbbbbb-0000-4000-8000-000000000001.jsonl",
    ] {
        sample.extend(std::fs::read(root.join(rel)).expect("fixture"));
    }
    let mut data = Vec::with_capacity(TARGET + sample.len());
    while data.len() < TARGET {
        data.extend_from_slice(&sample);
    }
    let zst =
        ruzstd::encoding::compress_to_vec(&data[..], ruzstd::encoding::CompressionLevel::Fastest);
    let path =
        dir.join("rollout-2026-10-02T09-00-00-aaaaaaaa-0000-4000-8000-000000000001.jsonl.zst");
    std::fs::write(&path, zst).expect("write the bench archive");
    (path, data.len())
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

struct Report {
    failed: bool,
}

impl Report {
    fn line(&mut self, name: &str, measured: String, budget: &str, ok: bool) {
        println!(
            "{name:<26} {measured:>14}   budget {budget:<12} {}",
            if ok { "ok" } else { "OVER BUDGET" }
        );
        self.failed |= !ok;
    }
}

#[test]
#[ignore = "timing; run in release mode on demand"]
fn perf_smoke() {
    let dir = std::env::temp_dir().join(format!("lectern-perf-agents-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let (path, sample) = build(&dir);
    let len = std::fs::metadata(&path).expect("stat").len();
    let mut report = Report { failed: false };

    // agents/parse_throughput：整文件解析（记录加轮次索引），取 5 次的中位数。
    let _ = parse_file(Agent::ClaudeCode, &path).expect("parse");
    let mut runs = Vec::new();
    for _ in 0..5 {
        let start = Instant::now();
        let (records, index) = parse_file(Agent::ClaudeCode, &path).expect("parse");
        runs.push(start.elapsed());
        std::hint::black_box((records, index));
    }
    let mbps = len as f64 / (1 << 20) as f64 / median(runs).as_secs_f64();
    report.line(
        "agents/parse_throughput",
        format!("{mbps:.0} MB/s"),
        ">= 200 MB/s",
        mbps >= 200.0,
    );

    // agents/append_4k：每次追加约 4 KB 的完整行，再从上一次的游标读到末尾，取 200 次的中位数。
    let adapter = ClaudeCode::new(SourceId(SOURCE_ID.into()));
    let file = adapter.session_file(&path).expect("session file");
    let mut cursor = adapter
        .read(&file, &Cursor::START, u64::MAX)
        .expect("read")
        .batch
        .cursor;
    let chunk: Vec<u8> = {
        let mut end = 0;
        for (i, b) in sample.iter().enumerate() {
            if *b == b'\n' {
                end = i + 1;
                if end >= 4096 {
                    break;
                }
            }
        }
        sample[..end].to_vec()
    };
    let mut runs = Vec::new();
    for _ in 0..200 {
        let mut f = OpenOptions::new().append(true).open(&path).expect("open");
        f.write_all(&chunk).expect("append");
        drop(f);
        let start = Instant::now();
        let parsed = adapter.read(&file, &cursor, u64::MAX).expect("read");
        runs.push(start.elapsed());
        assert!(parsed.batch.at_end && !parsed.batch.reset);
        cursor = parsed.batch.cursor;
    }
    let append = median(runs);
    report.line(
        "agents/append_4k",
        format!(
            "{:.3} ms ({} B)",
            append.as_secs_f64() * 1000.0,
            chunk.len()
        ),
        "< 1 ms",
        append < Duration::from_millis(1),
    );

    // agents/zst_throughput：不建副本，从头流式解压并解析整个文件，取 5 次的中位数。
    let (zst, decoded_len) = build_codex(&dir);
    let _ = parse_file(Agent::Codex, &zst).expect("parse");
    let mut runs = Vec::new();
    for _ in 0..5 {
        let start = Instant::now();
        let (records, index) = parse_file(Agent::Codex, &zst).expect("parse");
        runs.push(start.elapsed());
        std::hint::black_box((records, index));
    }
    let mbps = decoded_len as f64 / (1 << 20) as f64 / median(runs).as_secs_f64();
    report.line(
        "agents/zst_throughput",
        format!("{mbps:.0} MB/s"),
        ">= 100 MB/s",
        mbps >= 100.0,
    );

    // agents/zst_load_decoded：解压副本建好之后（第一次 `turns`），对索引里的引用逐个 `load`，取中位数。
    let codex = Codex::new(SourceId(codex::SOURCE_ID.into()), dir.join("decoded"));
    let file = codex.session_file(&zst).expect("session file");
    let index = codex.turns(&file).expect("turns");
    let refs: Vec<_> = index
        .turns
        .iter()
        .rev()
        .take(200)
        .flat_map(|t| t.items.iter())
        .map(|i| match i {
            TurnItem::Tool(c) => c.input,
            TurnItem::AgentText(r) | TurnItem::Injected(r) => *r,
        })
        .collect();
    let mut runs = Vec::new();
    for r in &refs {
        let start = Instant::now();
        let content = codex.load(&file, r).expect("load");
        runs.push(start.elapsed());
        std::hint::black_box(content);
    }
    let load = median(runs);
    report.line(
        "agents/zst_load_decoded",
        format!(
            "{:.3} ms ({} loads)",
            load.as_secs_f64() * 1000.0,
            refs.len()
        ),
        "< 1 ms",
        load < Duration::from_millis(1),
    );

    let _ = std::fs::remove_dir_all(&dir);
    assert!(!report.failed, "over budget");
}
