//! 测试与 bench 用：不经过插件，直接解析一个文件（docs/design/agents.md「本 crate 的公开项」）。

use std::path::Path;
use std::sync::Arc;

use lectern_cache::Cache;
use lectern_services::project::Projects;
use lectern_services::session::{
    Agent, Positioned, Record, SessionError, SessionSource, SourceId, TurnIndex,
};

use crate::claude::{self, ClaudeCode, Owner, SessionFile};
use crate::codex::{self, Codex};
use crate::filter::SessionFilter;
use crate::plugin::ListLineage;
use crate::roots::AgentRoots;
use crate::source::place::ProjectsRef;
use crate::source::{Engine, Setup};

/// 不经过插件的会话来源（bench 与集成测试用）：返回之前在调用线程上同步完成第一次发现，不监听目录，
/// 不发事件；`complete` 也在调用线程上同步补齐。插件在后台任务里把轮次索引写进 `cache`，这里在 drop
/// 时写。`projects` 为 None 时不归位（`place` 都是 `Pending`），
/// `cache` 为 None 时不读写持久缓存。`Agent::Other` 返回 None。
pub fn source(
    agent: Agent,
    roots: &AgentRoots,
    cache: Option<&Cache>,
    projects: Option<Arc<dyn Projects>>,
    decoded_dir: &Path,
) -> Option<Arc<dyn SessionSource>> {
    source_with_calls(agent, roots, cache, projects, decoded_dir).map(|(source, _)| source)
}

/// 适配器读会话正文的调用次数：`read`、`turns_after`、`load`（派生 Codex 子 agent 关联时读 `spawn_agent`
/// 输出的那一次也算 `load`）。发现只读文件的开头和末尾，不计入。
#[derive(Clone, Debug)]
pub struct AdapterCalls(Arc<crate::source::Calls>);

impl AdapterCalls {
    pub fn read(&self) -> usize {
        self.0.read.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn turns_after(&self) -> usize {
        self.0
            .turns_after
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn load(&self) -> usize {
        self.0.load.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// [`source`]，另交出这个来源的 [`AdapterCalls`]（`agents/discover_codex_1000` 检查第一次发现没有读父会话）。
pub fn source_with_calls(
    agent: Agent,
    roots: &AgentRoots,
    cache: Option<&Cache>,
    projects: Option<Arc<dyn Projects>>,
    decoded_dir: &Path,
) -> Option<(Arc<dyn SessionSource>, AdapterCalls)> {
    let setup = |roots: Vec<std::path::PathBuf>, adapter_version| Setup {
        roots,
        executor: None,
        cache: cache.map(Cache::app),
        watcher: None,
        projects: match projects.clone() {
            Some(p) => ProjectsRef::Direct(p),
            None => ProjectsRef::None,
        },
        filter: SessionFilter::new(),
        adapter_version,
    };
    match agent {
        Agent::ClaudeCode => {
            let engine = Engine::new(
                |_| ClaudeCode::new(SourceId(claude::SOURCE_ID.into())),
                setup(roots.claude_sessions(), claude::ADAPTER_VERSION),
            );
            let calls = AdapterCalls(engine.calls.clone());
            engine.start(None);
            let engine: Arc<dyn SessionSource> = engine;
            Some((engine, calls))
        }
        Agent::Codex => {
            let decoded = decoded_dir.to_path_buf();
            let engine = Engine::new(
                move |me| {
                    Codex::new(SourceId(codex::SOURCE_ID.into()), decoded)
                        .with_lineage(Arc::new(ListLineage(me.clone())))
                },
                setup(roots.codex_sessions(), codex::ADAPTER_VERSION),
            );
            let calls = AdapterCalls(engine.calls.clone());
            engine.start(None);
            let engine: Arc<dyn SessionSource> = engine;
            Some((engine, calls))
        }
        Agent::Other(_) => None,
    }
}

/// 解析整个文件，交出全部记录和轮次索引。
///
/// Claude Code 文件按路径认出主会话或子 agent（[`ClaudeCode::session_file`]），认不出时按主会话、
/// 会话 ID 取文件名（去掉扩展名）。Codex 文件按文件名认出线程 ID（[`Codex::session_file`]），认不出时
/// 会话 ID 取文件名；`.jsonl.zst` 从头流式解码一遍，不建解压副本；没有来源会话可找（`legacy` 分叉的
/// 继承前缀按边界未定处理）。
pub fn parse_file(
    agent: Agent,
    path: &Path,
) -> Result<(Vec<Positioned<Record>>, TurnIndex), SessionError> {
    match agent {
        Agent::ClaudeCode => {
            let adapter = ClaudeCode::new(SourceId(claude::SOURCE_ID.into()));
            let file = adapter.session_file(path).unwrap_or_else(|| {
                let id: std::sync::Arc<str> = path
                    .file_stem()
                    .map(|s| s.to_string_lossy())
                    .unwrap_or_default()
                    .into();
                SessionFile {
                    key: lectern_services::session::SessionKey {
                        source: adapter.source().clone(),
                        id: id.clone(),
                    },
                    path: path.into(),
                    owner: Owner::Main { session_id: id },
                }
            });
            let (parsed, index) = adapter.parse_all(&file)?;
            Ok((parsed.batch.records, index))
        }
        Agent::Codex => {
            let adapter = Codex::without_decoded_copies(SourceId(codex::SOURCE_ID.into()));
            let file = adapter.session_file(path).unwrap_or_else(|| {
                let name = path
                    .file_name()
                    .map(|s| s.to_string_lossy())
                    .unwrap_or_default();
                let compressed = name.ends_with(".zst");
                let id = name
                    .trim_end_matches(".zst")
                    .trim_end_matches(".jsonl")
                    .to_owned();
                codex::SessionFile {
                    key: lectern_services::session::SessionKey {
                        source: adapter.source().clone(),
                        id: id.into(),
                    },
                    path: path.into(),
                    compressed,
                    archived: false,
                }
            });
            let (parsed, index) = adapter.parse_all(&file)?;
            Ok((parsed.batch.records, index))
        }
        Agent::Other(_) => Err(SessionError::Unrecognized),
    }
}

/// 合成的 Claude Code 2.1.302 会话（`tests/fixtures/claude-code/2.1.302/`）：主会话、两个子 agent、
/// 另一个会话，依次拼接。
const CLAUDE_SAMPLE: [&[u8]; 4] = [
    include_bytes!(
        "../tests/fixtures/claude-code/2.1.302/projects/-Users-dev-fixture-repo/11111111-1111-4111-8111-111111111111.jsonl"
    ),
    include_bytes!(
        "../tests/fixtures/claude-code/2.1.302/projects/-Users-dev-fixture-repo/11111111-1111-4111-8111-111111111111/subagents/agent-a0fa0001.jsonl"
    ),
    include_bytes!(
        "../tests/fixtures/claude-code/2.1.302/projects/-Users-dev-fixture-repo/11111111-1111-4111-8111-111111111111/subagents/workflows/wf-fixture/agent-a0fa0002.jsonl"
    ),
    include_bytes!(
        "../tests/fixtures/claude-code/2.1.302/projects/-Users-dev-fixture-repo/22222222-2222-4222-8222-222222222222.jsonl"
    ),
];

/// 合成的 Codex 0.140.0 rollout（`tests/fixtures/codex/`）里的两个主会话（不含需要来源会话的分叉）。
const CODEX_SAMPLE: [&[u8]; 2] = [
    include_bytes!(
        "../tests/fixtures/codex/0.140.0-legacy/sessions/2026/10/02/rollout-2026-10-02T09-00-00-aaaaaaaa-0000-4000-8000-000000000001.jsonl"
    ),
    include_bytes!(
        "../tests/fixtures/codex/0.140.0-paginated/sessions/2026/10/05/rollout-2026-10-05T09-00-00-bbbbbbbb-0000-4000-8000-000000000001.jsonl"
    ),
];

/// 把 `sample` 依次拼接、整体重复，直到不少于 `len` 字节（总是整份样本的倍数，每一行都完整）。
fn repeat(sample: &[&[u8]], len: usize) -> Vec<u8> {
    let one: usize = sample.iter().map(|s| s.len()).sum();
    let mut data = Vec::with_capacity(len + one);
    while data.len() < len {
        for part in sample {
            data.extend_from_slice(part);
        }
    }
    data
}

/// 一个 Claude Code 会话文件的内容，不少于 `len` 字节：fixtures 的全部会话文件重复拼接。
/// 供 bench 的 `agents/parse_throughput`、`agents/append_4k`（docs/design/bench.md「迁移」）。
pub fn synthetic_session(len: usize) -> Vec<u8> {
    repeat(&CLAUDE_SAMPLE, len)
}

/// 一个形状接近真实长会话的 Claude Code 会话，不少于 `len` 字节：fixtures 的主会话（11 次工具调用）之后跟
/// 约 330 KB 的已知忽略记录（`file-history-snapshot`，代替真实会话里的大段工具结果），重复拼接。工具调用的
/// 密度与设计里的参照相同（56 MB 约 1600 次，「性能」），所以轮次索引的大小接近真实会话的。供 bench 的
/// `agents/turns_cached_50mb`。
pub fn synthetic_long_session(len: usize) -> Vec<u8> {
    let pad = format!(
        "{{\"type\":\"file-history-snapshot\",\"messageId\":\"pad\",\"snapshot\":{{\"padding\":\"{}\"}},\"isSnapshotUpdate\":false}}\n",
        "x".repeat(33_000)
    );
    let padding = pad.repeat(10);
    repeat(&[CLAUDE_SAMPLE[0], padding.as_bytes()], len)
}

/// 约 4 KB 的完整行：[`synthetic_session`] 开头到第一个不早于 4096 字节的行尾，追加到会话末尾后
/// 仍是合法的会话。
pub fn append_chunk() -> &'static [u8] {
    let first = CLAUDE_SAMPLE[0];
    let end = first
        .iter()
        .enumerate()
        .find(|(i, b)| **b == b'\n' && *i + 1 >= 4096)
        .map_or(first.len(), |(i, _)| i + 1);
    &first[..end]
}

/// 一个 Codex rollout 的内容，不少于 `len` 字节：fixtures 的两个主会话重复拼接。供 bench 的
/// `agents/zst_throughput`、`agents/zst_load_decoded`。
pub fn synthetic_rollout(len: usize) -> Vec<u8> {
    repeat(&CODEX_SAMPLE, len)
}

/// 用 `ruzstd` 的最快档压缩成一个 zstd 帧，即 `.jsonl.zst` 文件的内容。
pub fn compress_zst(data: &[u8]) -> Vec<u8> {
    ruzstd::encoding::compress_to_vec(data, ruzstd::encoding::CompressionLevel::Fastest)
}

/// 在 `sessions`（Codex 主目录下的 `sessions/`）里写 `parents` 个 Codex 父会话，每个在一轮里用 `spawn_agent`
/// 派出 `children` 个子会话；子会话的 `session_meta` 用 `source.subagent.thread_spawn.parent_thread_id` 指回父线程。
/// 记录形状与 `tests/fixtures/codex/0.140.0-legacy/` 相同，`spawn_agent` 的输出是 multi_agent v1 的
/// `{"agent_id": <子线程 ID>, "nickname": null}`（docs/design/agents.md「外部格式」）。每 100 个文件一个日期目录，
/// 时间戳都在 2026-10 月初（不是活跃会话）。返回写下的全部会话 ID：每个父会话之后跟着它的子会话。供 bench 的
/// `agents/discover_codex_1000`。
pub fn synthetic_rollouts(
    sessions: &Path,
    parents: usize,
    children: usize,
) -> std::io::Result<Vec<String>> {
    const CWD: &str = "/Users/dev/fixture/repo";
    let mut ids = Vec::with_capacity(parents * (children + 1));
    let mut files = 0usize;
    let mut write = |id: &str, body: &str| -> std::io::Result<()> {
        let day = 1 + (files / 100) % 28;
        files += 1;
        let dir = sessions.join(format!("2026/10/{day:02}"));
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            dir.join(format!("rollout-2026-10-{day:02}T09-00-00-{id}.jsonl")),
            body,
        )
    };
    for p in 0..parents {
        let parent = format!("{p:08x}-0000-4000-8000-000000000000");
        let ts = |n: usize| format!("2026-10-01T09:{:02}:{:02}.000Z", n / 60 % 60, n % 60);
        let mut body = String::new();
        let mut line = 0usize;
        let mut push = |body: &mut String, text: String| {
            body.push_str(&format!("{{\"timestamp\":\"{}\",{text}}}\n", ts(line)));
            line += 1;
        };
        push(
            &mut body,
            format!(
                r#""type":"session_meta","payload":{{"id":"{parent}","session_id":"{parent}","timestamp":"{}","cwd":"{CWD}","originator":"codex_cli_rs","cli_version":"0.140.0","source":"cli","model_provider":"fixture-provider","history_mode":"legacy","git":{{"branch":"main"}}}}"#,
                ts(0)
            ),
        );
        push(
            &mut body,
            format!(
                r#""type":"turn_context","payload":{{"turn_id":"turn-{p}","cwd":"{CWD}","model":"fixture-model-x"}}"#
            ),
        );
        push(
            &mut body,
            format!(
                r#""type":"event_msg","payload":{{"type":"task_started","turn_id":"turn-{p}"}}"#
            ),
        );
        push(
            &mut body,
            r#""type":"event_msg","payload":{"type":"user_message","message":"Placeholder prompt: split the work.","images":[]}"#.to_owned(),
        );
        let mut kids = Vec::with_capacity(children);
        for c in 0..children {
            let child = format!("{p:08x}-{:04x}-4000-8000-000000000000", c + 1);
            push(
                &mut body,
                format!(
                    r#""type":"response_item","payload":{{"type":"function_call","name":"spawn_agent","namespace":"multi_agent_v1","arguments":"{{\"message\":\"Placeholder subagent task {c}.\"}}","call_id":"call_{p}_{c}"}}"#
                ),
            );
            push(
                &mut body,
                format!(
                    r#""type":"response_item","payload":{{"type":"function_call_output","call_id":"call_{p}_{c}","output":"{{\"agent_id\":\"{child}\",\"nickname\":null}}"}}"#
                ),
            );
            kids.push(child);
        }
        push(
            &mut body,
            format!(
                r#""type":"event_msg","payload":{{"type":"task_complete","turn_id":"turn-{p}","last_agent_message":"Placeholder report."}}"#
            ),
        );
        write(&parent, &body)?;
        ids.push(parent.clone());
        for (c, child) in kids.into_iter().enumerate() {
            let t = |s: usize| format!("2026-10-01T10:{:02}:{:02}.000Z", c % 60, s);
            let body = [
                format!(
                    r#"{{"timestamp":"{}","type":"session_meta","payload":{{"id":"{child}","session_id":"{child}","timestamp":"{}","cwd":"{CWD}","originator":"codex_cli_rs","cli_version":"0.140.0","source":{{"subagent":{{"thread_spawn":{{"parent_thread_id":"{parent}","depth":1}}}}}},"model_provider":"fixture-provider","history_mode":"legacy","git":{{"branch":"main"}}}}}}"#,
                    t(0),
                    t(0)
                ),
                format!(
                    r#"{{"timestamp":"{}","type":"turn_context","payload":{{"turn_id":"turn-{p}-{c}","cwd":"{CWD}","model":"fixture-model-x"}}}}"#,
                    t(1)
                ),
                format!(
                    r#"{{"timestamp":"{}","type":"event_msg","payload":{{"type":"task_started","turn_id":"turn-{p}-{c}"}}}}"#,
                    t(2)
                ),
                format!(
                    r#"{{"timestamp":"{}","type":"event_msg","payload":{{"type":"user_message","message":"Placeholder subagent task {c}.","images":[]}}}}"#,
                    t(3)
                ),
                format!(
                    r#"{{"timestamp":"{}","type":"event_msg","payload":{{"type":"agent_message","message":"Placeholder subagent report."}}}}"#,
                    t(4)
                ),
                format!(
                    r#"{{"timestamp":"{}","type":"event_msg","payload":{{"type":"task_complete","turn_id":"turn-{p}-{c}","last_agent_message":"Placeholder subagent report."}}}}"#,
                    t(5)
                ),
            ]
            .join("\n")
                + "\n";
            write(&child, &body)?;
            ids.push(child);
        }
    }
    Ok(ids)
}

/// 在 `projects`（Claude Code 配置目录下的 `projects/`）里写 `sessions` 个会话文件，每 20 个一个项目目录：
/// 每个文件是 fixtures 的主会话，`sessionId` 换成文件名里的新 ID。返回写下的会话 ID。供 bench 的
/// `agents/discover_1000`。
pub fn synthetic_projects(projects: &Path, sessions: usize) -> std::io::Result<Vec<String>> {
    const MAIN: &str = "11111111-1111-4111-8111-111111111111";
    let sample = std::str::from_utf8(CLAUDE_SAMPLE[0]).expect("the fixture is UTF-8");
    let mut ids = Vec::with_capacity(sessions);
    for n in 0..sessions {
        let id = format!("{n:08x}-1111-4111-8111-111111111111");
        let dir = projects.join(format!("-Users-dev-fixture-repo-{}", n / 20));
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(format!("{id}.jsonl")), sample.replace(MAIN, &id))?;
        ids.push(id);
    }
    Ok(ids)
}
