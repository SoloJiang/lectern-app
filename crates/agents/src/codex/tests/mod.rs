//! Codex 适配器的测试（docs/design/agents.md「测试」，验收第 3 步）。
//!
//! fixtures 是合成数据（`tests/fixtures/codex/*/SOURCE.md`）；`.jsonl.zst`、坏行、超长行等在临时目录里生成。

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lectern_services::session::{Cursor, Positioned, Record, SourceId};

use super::{Codex, Lineage, Parsed, SOURCE_ID, SessionFile};
pub(crate) use crate::claude::tests::TempDir;

mod fixtures;
mod reading;
mod records;
mod summary;
mod zst;

pub(super) const L1: &str = "aaaaaaaa-0000-4000-8000-000000000001";
pub(super) const L2: &str = "aaaaaaaa-0000-4000-8000-000000000002";
pub(super) const L3: &str = "aaaaaaaa-0000-4000-8000-000000000003";
pub(super) const P1: &str = "bbbbbbbb-0000-4000-8000-000000000001";
pub(super) const P2: &str = "bbbbbbbb-0000-4000-8000-000000000002";

pub(crate) fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex")
}

/// fixtures：名字、相对 `tests/fixtures/codex/` 的路径。
pub(crate) fn fixture_files() -> [(&'static str, String); 5] {
    [
        (
            "legacy_main",
            format!("0.140.0-legacy/sessions/2026/10/02/rollout-2026-10-02T09-00-00-{L1}.jsonl"),
        ),
        (
            "legacy_fork",
            format!("0.140.0-legacy/sessions/2026/10/03/rollout-2026-10-03T09-00-00-{L2}.jsonl"),
        ),
        (
            "legacy_fork_of_fork",
            format!("0.140.0-legacy/sessions/2026/10/04/rollout-2026-10-04T09-00-00-{L3}.jsonl"),
        ),
        (
            "paginated_main",
            format!("0.140.0-paginated/sessions/2026/10/05/rollout-2026-10-05T09-00-00-{P1}.jsonl"),
        ),
        (
            "paginated_subagent",
            format!("0.140.0-paginated/sessions/2026/10/05/rollout-2026-10-05T09-30-00-{P2}.jsonl"),
        ),
    ]
}

/// fixture 的相对路径。
pub(super) fn rel(name: &str) -> String {
    fixture_files()
        .into_iter()
        .find(|(n, _)| *n == name)
        .map(|(_, r)| r)
        .unwrap_or_else(|| panic!("no fixture {name}"))
}

/// 线程 ID 到文件的表，可以在测试中途增删。
#[derive(Default)]
pub(crate) struct MapLineage(Mutex<HashMap<String, SessionFile>>);

impl MapLineage {
    pub(crate) fn insert(&self, thread: &str, file: SessionFile) {
        self.0.lock().unwrap().insert(thread.to_owned(), file);
    }
}

impl Lineage for MapLineage {
    fn locate(&self, thread_id: &str) -> Option<SessionFile> {
        self.0.lock().unwrap().get(thread_id).cloned()
    }
}

/// 不建解压副本的适配器，能找到全部 fixtures 作为来源会话。
pub(super) fn adapter() -> Codex {
    let base = Codex::without_decoded_copies(SourceId(SOURCE_ID.into()));
    let lineage = MapLineage::default();
    for (_, r) in fixture_files() {
        let file = base.session_file(&fixtures_dir().join(r)).unwrap();
        lineage.insert(&file.key.id.clone(), file);
    }
    base.with_lineage(Arc::new(lineage))
}

pub(super) fn fixture(name: &str) -> SessionFile {
    adapter()
        .session_file(&fixtures_dir().join(rel(name)))
        .expect("a session file")
}

/// 临时目录里的一个 rollout 文件名（线程 ID 为 `thread`）。
pub(crate) fn rollout_name(thread: &str, compressed: bool) -> String {
    format!(
        "rollout-2026-10-09T09-00-00-{thread}.jsonl{}",
        if compressed { ".zst" } else { "" }
    )
}

/// 合成的一行：`type`、`payload`，时间戳按秒递增。
pub(super) fn line(kind: &str, sec: u32, payload: &str) -> String {
    format!(
        r#"{{"timestamp":"2026-10-09T09:{:02}:{:02}.000Z","type":"{kind}","payload":{payload}}}"#,
        sec / 60,
        sec % 60
    )
}

pub(super) fn meta_line(thread: &str, extra: &str) -> String {
    line(
        "session_meta",
        0,
        &format!(
            r#"{{"id":"{thread}","timestamp":"2026-10-09T09:00:00.000Z","cwd":"/Users/dev/fixture/repo","cli_version":"0.140.0","source":"cli","history_mode":"legacy"{extra}}}"#
        ),
    )
}

pub(super) fn event(sec: u32, payload: &str) -> String {
    line("event_msg", sec, payload)
}

pub(super) fn item(sec: u32, payload: &str) -> String {
    line("response_item", sec, payload)
}

/// 在临时目录里写一个由这些行组成的 rollout 文件。
pub(super) fn write_lines(dir: &TempDir, thread: &str, lines: &[String]) -> SessionFile {
    let path = dir.path().join(rollout_name(thread, false));
    let mut text = String::new();
    for l in lines {
        text.push_str(l);
        text.push('\n');
    }
    std::fs::write(&path, text).unwrap();
    adapter().session_file(&path).unwrap()
}

/// 从 `from` 起反复 `read` 到 `at_end`。
pub(super) fn read_to_end(
    adapter: &Codex,
    file: &SessionFile,
    from: &Cursor,
    max_bytes: u64,
) -> (Vec<Positioned<Record>>, Cursor, Vec<Parsed>) {
    let mut cursor = from.clone();
    let mut records = Vec::new();
    let mut batches = Vec::new();
    loop {
        let parsed = adapter.read(file, &cursor, max_bytes).unwrap();
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
