//! Claude Code 适配器的测试（docs/design/agents.md「测试」，验收第 2 步）。
//!
//! fixtures 是合成数据（`tests/fixtures/claude-code/2.1.302/SOURCE.md`）；超长行、坏行等在临时目录里生成。

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use lectern_services::session::{Cursor, Positioned, Record, SessionKey, SourceId};

use super::{ClaudeCode, Owner, Parsed, SOURCE_ID, SessionFile};

mod fixtures;
mod reading;
mod records;
mod summary;

pub(super) const MAIN: &str = "11111111-1111-4111-8111-111111111111";
pub(super) const FORK: &str = "22222222-2222-4222-8222-222222222222";
pub(super) const SUB1: &str = "11111111-1111-4111-8111-111111111111/subagents/agent-a0fa0001.jsonl";
pub(super) const SUB2: &str =
    "11111111-1111-4111-8111-111111111111/subagents/workflows/wf-fixture/agent-a0fa0002.jsonl";

/// fixtures 的版本目录。
pub(crate) fn version_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-code/2.1.302")
}

/// fixtures 里的项目目录。
pub(crate) fn project_dir() -> PathBuf {
    version_dir().join("projects/-Users-dev-fixture-repo")
}

/// fixtures 里的会话文件：名字、相对项目目录的路径。
pub(crate) fn fixture_files() -> [(&'static str, String); 4] {
    [
        ("main", format!("{MAIN}.jsonl")),
        ("subagent_worktree", SUB1.to_owned()),
        ("subagent_nested", SUB2.to_owned()),
        ("fork", format!("{FORK}.jsonl")),
    ]
}

pub(super) fn adapter() -> ClaudeCode {
    ClaudeCode::new(SourceId(SOURCE_ID.into()))
}

pub(super) fn fixture(rel: &str) -> SessionFile {
    adapter()
        .session_file(&project_dir().join(rel))
        .expect("a session file")
}

/// 一个用完即删的临时目录。
pub(crate) struct TempDir(PathBuf);

impl TempDir {
    pub(crate) fn new() -> TempDir {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lectern-agents-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 临时目录里的一个会话文件，归属与 `owner` 相同。
pub(crate) fn temp_file(dir: &TempDir, owner: Owner) -> SessionFile {
    let id: Arc<str> = match &owner {
        Owner::Main { session_id } => session_id.clone(),
        Owner::Subagent {
            parent_session_id,
            agent_id,
        } => format!("{parent_session_id}/agent-{agent_id}").into(),
    };
    SessionFile {
        key: SessionKey {
            source: SourceId(SOURCE_ID.into()),
            id,
        },
        path: dir.path().join("session.jsonl").into(),
        owner,
    }
}

pub(super) fn main_owner() -> Owner {
    Owner::Main {
        session_id: MAIN.into(),
    }
}

/// 在临时目录里写一个由这些行组成的主会话文件。
pub(super) fn write_lines(dir: &TempDir, lines: &[String]) -> SessionFile {
    let file = temp_file(dir, main_owner());
    let mut text = String::new();
    for l in lines {
        text.push_str(l);
        text.push('\n');
    }
    std::fs::write(&file.path, text).unwrap();
    file
}

/// 合成的一行记录：`type` 与额外字段，带主会话的公共字段。
pub(super) fn record(kind: &str, sec: u32, extra: &str) -> String {
    format!(
        r#"{{"type":"{kind}","sessionId":"{MAIN}","cwd":"/Users/dev/fixture/repo","gitBranch":"main","version":"2.1.302","uuid":"bbbbbbbb-0000-4000-8000-{sec:012}","timestamp":"2026-10-02T10:{:02}:{:02}.000Z"{extra}}}"#,
        sec / 60,
        sec % 60
    )
}

pub(super) fn prompt(sec: u32, text: &str) -> String {
    record(
        "user",
        sec,
        &format!(r#","message":{{"role":"user","content":"{text}"}}"#),
    )
}

pub(super) fn assistant(sec: u32, msg: &str, blocks: &str, stop: &str) -> String {
    record(
        "assistant",
        sec,
        &format!(
            r#","requestId":"req_{msg}","message":{{"id":"msg_{msg}","model":"fixture-model-a","role":"assistant","content":[{blocks}],"stop_reason":{stop},"usage":{{"input_tokens":1,"output_tokens":2}}}}"#
        ),
    )
}

/// 从 `from` 起反复 `read` 到 `at_end`。
pub(super) fn read_to_end(
    adapter: &ClaudeCode,
    file: &SessionFile,
    from: &Cursor,
    max_bytes: u64,
) -> (Vec<Positioned<Record>>, Cursor, Vec<Parsed>) {
    let mut cursor = from.clone();
    let mut records = Vec::new();
    let mut batches = Vec::new();
    loop {
        let parsed = adapter.read(file, &cursor, max_bytes).unwrap();
        // 从头回放的一批游标可以比 `from` 小；其余没到末尾的批都让游标前进。
        if !parsed.batch.at_end && !parsed.batch.reset {
            assert!(
                parsed.batch.cursor.offset() > cursor.offset(),
                "a batch that is not at the end moves the cursor"
            );
        }
        records.extend(parsed.batch.records.iter().cloned());
        cursor = parsed.batch.cursor.clone();
        let at_end = parsed.batch.at_end;
        batches.push(parsed);
        if at_end {
            return (records, cursor, batches);
        }
    }
}

/// 每条记录一行，供快照阅读。
pub(super) fn render(records: &[Positioned<Record>]) -> String {
    let mut out = String::new();
    for r in records {
        let inherited = if r.inherited { " inherited" } else { "" };
        writeln!(out, "{}.{}{inherited} {:?}", r.pos, r.seq, r.value).unwrap();
    }
    out
}
