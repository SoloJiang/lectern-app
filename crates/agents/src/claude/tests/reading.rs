//! 增量读取、改写与截断、读取进度、异常行（`incremental_*`、`reset_*`、`progress_*`、`format_*`）。

use std::fs::OpenOptions;
use std::io::Write as _;

use lectern_services::session::{Cursor, Format, Record, SessionError, UsageKey};

use super::*;
use crate::claude::ADAPTER_VERSION;
use crate::lines::MAX_LINE;

/// 确定性的伪随机数（xorshift64*）。
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) % n.max(1)
    }
}

fn append(path: &Path, bytes: &[u8]) {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    f.write_all(bytes).unwrap();
}

/// 游标经 `to_bytes` / `from_bytes` 和 serde 往返。
fn round_trip(cursor: &Cursor) -> Cursor {
    let back = Cursor::from_bytes(&cursor.to_bytes()).unwrap();
    let json = serde_json::to_string(&back).unwrap();
    serde_json::from_str(&json).unwrap()
}

#[test]
fn incremental_random_appends_match_a_single_read() {
    let adapter = adapter();
    for (name, rel) in fixture_files() {
        let fixture = fixture(&rel);
        let bytes = std::fs::read(&fixture.path).unwrap();
        let (expected, expected_cursor, _) =
            read_to_end(&adapter, &fixture, &Cursor::START, u64::MAX);
        let expected_index = adapter.turns(&fixture).unwrap();

        for seed in 1..=8u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let dir = TempDir::new();
            let file = temp_file(&dir, fixture.owner.clone());
            std::fs::write(&file.path, b"").unwrap();
            let mut cursor = Cursor::START;
            let mut records = Vec::new();
            let mut index = None;
            let mut written = 0;
            while written < bytes.len() {
                // 包括行中间的切分。
                let n = 1 + rng.below(bytes.len() as u64 / 3) as usize;
                let end = (written + n).min(bytes.len());
                append(&file.path, &bytes[written..end]);
                written = end;
                let max_bytes = [1, 97, 4096, u64::MAX][rng.below(4) as usize];
                let (batch, next, parsed) = read_to_end(&adapter, &file, &cursor, max_bytes);
                assert!(
                    parsed.iter().all(|p| !p.batch.reset),
                    "{name}: appending never resets"
                );
                records.extend(batch);
                cursor = round_trip(&next);
                index = Some(
                    adapter
                        .turns_after(&file, index.as_ref())
                        .unwrap_or_else(|e| {
                            // 还没有写到会话头之前认不出格式。
                            assert_eq!(e, SessionError::Unrecognized);
                            lectern_services::session::TurnIndex {
                                session: file.key.clone(),
                                cursor: Cursor::START,
                                turns: Arc::new([]),
                                format: Format::Known,
                            }
                        }),
                );
            }
            assert_eq!(records, expected, "{name} seed {seed}");
            assert_eq!(cursor, expected_cursor, "{name} seed {seed}");
            let mut index = index.unwrap();
            index.session = fixture.key.clone();
            assert_eq!(index, expected_index, "{name} seed {seed}");
        }
    }
}

#[test]
fn incremental_cursor_round_trip_continues_identically() {
    let adapter = adapter();
    let file = fixture(&format!("{MAIN}.jsonl"));
    let (all, _, _) = read_to_end(&adapter, &file, &Cursor::START, u64::MAX);
    let first = adapter.read(&file, &Cursor::START, 5000).unwrap().batch;
    assert!(!first.at_end);
    let (rest_direct, _, _) = read_to_end(&adapter, &file, &first.cursor, u64::MAX);
    let (rest_trip, _, _) = read_to_end(&adapter, &file, &round_trip(&first.cursor), u64::MAX);
    assert_eq!(rest_direct, rest_trip);
    let mut joined = first.records.clone();
    joined.extend(rest_direct);
    assert_eq!(joined, all);
    // 游标只停在换行之后。
    let bytes = std::fs::read(&file.path).unwrap();
    assert_eq!(bytes[first.cursor.offset() as usize - 1], b'\n');
}

/// 把一份 fixture 写进临时目录，读到末尾，返回文件和游标。
fn read_copy(dir: &TempDir, rel: &str) -> (SessionFile, Cursor) {
    let file = temp_file(dir, main_owner());
    std::fs::copy(project_dir().join(rel), &file.path).unwrap();
    let (_, cursor, _) = read_to_end(&adapter(), &file, &Cursor::START, u64::MAX);
    (file, cursor)
}

/// `from` 处读到末尾：第一批 `reset`，记录与从头读相同。
fn assert_replays(file: &SessionFile, from: &Cursor) {
    let adapter = adapter();
    let (fresh, fresh_cursor, _) = read_to_end(&adapter, file, &Cursor::START, u64::MAX);
    let (replayed, cursor, batches) = read_to_end(&adapter, file, from, 1000);
    assert!(batches[0].batch.reset, "the first batch resets");
    assert!(
        batches[1..].iter().all(|b| !b.batch.reset),
        "the replay continues without resetting again"
    );
    assert_eq!(replayed, fresh);
    assert_eq!(cursor, fresh_cursor);
}

#[test]
fn reset_after_truncation() {
    let dir = TempDir::new();
    let (file, cursor) = read_copy(&dir, &format!("{MAIN}.jsonl"));
    let len = std::fs::metadata(&file.path).unwrap().len();
    OpenOptions::new()
        .write(true)
        .open(&file.path)
        .unwrap()
        .set_len(len / 2)
        .unwrap();
    assert_replays(&file, &cursor);
}

#[test]
fn reset_after_replacement_by_another_session() {
    let usage_ids = |records: &[Positioned<Record>]| -> Vec<String> {
        records
            .iter()
            .filter_map(|r| match &r.value {
                Record::Usage(u) => match &u.key {
                    UsageKey::Claude { message_id, .. } => Some(message_id.to_string()),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    };
    // 换成更长的另一个会话（偏移仍在文件之内，前缀对不上）。
    let dir = TempDir::new();
    let (file, cursor) = read_copy(&dir, &format!("{FORK}.jsonl"));
    std::fs::copy(project_dir().join(format!("{MAIN}.jsonl")), &file.path).unwrap();
    assert_replays(&file, &cursor);
    let (replayed, _, _) = read_to_end(&adapter(), &file, &cursor, u64::MAX);
    assert!(
        !usage_ids(&replayed)
            .iter()
            .any(|id| id.starts_with("msg_fixture_f")),
        "records only in the old file are not replayed"
    );

    // 换成更短的（偏移超出文件）。
    let dir = TempDir::new();
    let (file, cursor) = read_copy(&dir, &format!("{MAIN}.jsonl"));
    std::fs::copy(project_dir().join(format!("{FORK}.jsonl")), &file.path).unwrap();
    assert_replays(&file, &cursor);
    let (replayed, _, _) = read_to_end(&adapter(), &file, &cursor, u64::MAX);
    assert!(!usage_ids(&replayed).contains(&"msg_fixture_0010".to_owned()));
}

#[test]
fn reset_after_rewrite_before_the_cursor() {
    let dir = TempDir::new();
    let (file, cursor) = read_copy(&dir, &format!("{MAIN}.jsonl"));
    let mut bytes = std::fs::read(&file.path).unwrap();
    // 改写游标之前 256 字节里的一个字节，长度不变。
    let at = bytes.len() - 100;
    bytes[at] = if bytes[at] == b'x' { b'y' } else { b'x' };
    std::fs::write(&file.path, &bytes).unwrap();
    assert_replays(&file, &cursor);
}

#[test]
fn reset_for_foreign_or_unreadable_cursors() {
    let dir = TempDir::new();
    let (file, cursor) = read_copy(&dir, &format!("{MAIN}.jsonl"));
    let state = cursor.state().to_vec();
    let foreign = [
        Cursor::new(
            SourceId("session-codex".into()),
            ADAPTER_VERSION,
            cursor.offset(),
            state.clone(),
        ),
        Cursor::new(
            cursor.source().unwrap().clone(),
            ADAPTER_VERSION + 1,
            cursor.offset(),
            state.clone(),
        ),
        Cursor::new(
            cursor.source().unwrap().clone(),
            ADAPTER_VERSION,
            cursor.offset(),
            vec![9, 9, 9],
        ),
        // 前缀副本完好、解析器状态坏了。
        Cursor::new(
            cursor.source().unwrap().clone(),
            ADAPTER_VERSION,
            cursor.offset(),
            state[..state.len() - 1].to_vec(),
        ),
        // 偏移不在行首。
        Cursor::new(
            cursor.source().unwrap().clone(),
            ADAPTER_VERSION,
            cursor.offset() - 3,
            state,
        ),
    ];
    for from in &foreign {
        assert_replays(&file, from);
    }
}

#[test]
fn progress_max_bytes_one_consumes_one_line_per_call() {
    let adapter = adapter();
    let file = fixture(&format!("{MAIN}.jsonl"));
    let bytes = std::fs::read(&file.path).unwrap();
    let ends: Vec<u64> = bytes
        .iter()
        .enumerate()
        .filter(|(_, b)| **b == b'\n')
        .map(|(i, _)| i as u64 + 1)
        .collect();
    let (records, _, batches) = read_to_end(&adapter, &file, &Cursor::START, 1);
    let offsets: Vec<u64> = batches.iter().map(|b| b.batch.cursor.offset()).collect();
    assert_eq!(offsets, ends, "one line per call");
    assert!(batches.last().unwrap().batch.at_end);
    assert!(batches[..batches.len() - 1].iter().all(|b| !b.batch.at_end));
    let (all, _, _) = read_to_end(&adapter, &file, &Cursor::START, u64::MAX);
    assert_eq!(records, all);
}

#[test]
fn progress_a_line_larger_than_max_bytes_is_returned_whole() {
    let dir = TempDir::new();
    let big = "x".repeat(1 << 20);
    let file = write_lines(
        &dir,
        &[
            prompt(1, "first"),
            assistant(
                2,
                "big",
                &format!(r#"{{"type":"text","text":"{big}"}}"#),
                r#""end_turn""#,
            ),
            prompt(3, "last"),
        ],
    );
    let adapter = adapter();
    let first = adapter.read(&file, &Cursor::START, 10).unwrap().batch;
    assert!(!first.at_end);
    let second = adapter.read(&file, &first.cursor, 10).unwrap().batch;
    assert!(!second.at_end);
    assert!(second.cursor.offset() - first.cursor.offset() > 1 << 20);
    assert!(
        second
            .records
            .iter()
            .any(|r| matches!(r.value, Record::Usage(_))),
        "the large line's records"
    );
    let third = adapter.read(&file, &second.cursor, 10).unwrap().batch;
    assert!(third.at_end);
    assert!(matches!(third.records[0].value, Record::TurnStarted { .. }));
}

#[test]
fn progress_oversized_lines_are_skipped_and_counted() {
    let dir = TempDir::new();
    let huge = "y".repeat(MAX_LINE + 10);
    let file = write_lines(
        &dir,
        &[
            prompt(1, "first"),
            prompt(2, &huge),
            assistant(
                3,
                "after",
                r#"{"type":"text","text":"after"}"#,
                r#""end_turn""#,
            ),
        ],
    );
    let adapter = adapter();
    let len = std::fs::metadata(&file.path).unwrap().len();
    let all = adapter.read(&file, &Cursor::START, u64::MAX).unwrap();
    assert_eq!(all.stats.oversized_lines, 1);
    assert!(all.batch.at_end);
    assert_eq!(all.batch.cursor.offset(), len);
    let starts = all
        .batch
        .records
        .iter()
        .filter(|r| matches!(r.value, Record::TurnStarted { .. }))
        .count();
    assert_eq!(starts, 1, "the oversized prompt is skipped");
    assert!(
        all.batch
            .records
            .iter()
            .any(|r| matches!(r.value, Record::Usage(_)))
    );

    // 分批时它也算消费了一行。
    let first = adapter.read(&file, &Cursor::START, 1).unwrap().batch;
    let second = adapter.read(&file, &first.cursor, 1).unwrap();
    assert!(second.batch.records.is_empty());
    assert_eq!(second.stats.oversized_lines, 1);
    assert!(!second.batch.at_end);
    assert!(second.batch.cursor.offset() > first.cursor.offset() + MAX_LINE as u64);
    let third = adapter.read(&file, &second.batch.cursor, 1).unwrap().batch;
    assert!(third.at_end);
    assert_eq!(third.cursor.offset(), len);
    // 越过超长行之后的游标照样能续读（前缀副本取自超长行的末尾）。
    let again = adapter
        .read(&file, &second.batch.cursor, u64::MAX)
        .unwrap()
        .batch;
    assert!(!again.reset);
    assert_eq!(again.records, third.records);
}

#[test]
fn progress_an_unfinished_oversized_line_is_not_skipped() {
    let dir = TempDir::new();
    let file = write_lines(&dir, &[prompt(1, "first")]);
    append(&file.path, "z".repeat(MAX_LINE + 10).as_bytes());
    let adapter = adapter();
    let parsed = adapter.read(&file, &Cursor::START, u64::MAX).unwrap();
    assert!(parsed.batch.at_end);
    assert_eq!(parsed.stats.oversized_lines, 0);
    assert_eq!(
        parsed.batch.cursor.offset(),
        prompt(1, "first").len() as u64 + 1
    );
}

#[test]
fn format_unknown_types_are_counted_and_known_ones_ignored() {
    let dir = TempDir::new();
    let file = write_lines(
        &dir,
        &[
            record("fixture-unknown", 0, ""),
            prompt(1, "hello"),
            record("fixture-unknown", 2, ""),
            record("fixture-unknown", 3, ""),
            record("system", 4, r#","subtype":"turn_duration""#),
            record("attachment", 5, r#","attachment":{}"#),
            r#"{"type":"summary","summary":"placeholder","leafUuid":"x"}"#.to_owned(),
        ],
    );
    let adapter = adapter();
    let parsed = adapter.read(&file, &Cursor::START, u64::MAX).unwrap();
    assert_eq!(parsed.stats.unknown_types, [("fixture-unknown".into(), 3)]);
    assert_eq!(parsed.stats.bad_lines, 0);
    assert_eq!(parsed.stats.format(), Format::Partial);
    assert_eq!(adapter.turns(&file).unwrap().format, Format::Partial);
    // 摘要只读到字段找齐为止（这里是第一个 prompt），只计入读过的行。
    let (summary, stats) = adapter.summarize(&file).unwrap();
    assert_eq!(summary.format, Format::Partial);
    assert_eq!(stats.unknown_types, [("fixture-unknown".into(), 1)]);
}

#[test]
fn format_bad_lines_are_skipped_and_counted() {
    let dir = TempDir::new();
    let file = write_lines(
        &dir,
        &[
            // 会话头之前没有 `sessionId` 的消息。
            r#"{"type":"user","cwd":"/Users/dev/fixture/repo","timestamp":"2026-10-02T10:00:00.000Z","message":{"content":"x"}}"#.to_owned(),
            prompt(1, "hello"),
            "{not json".to_owned(),
            r#"{"no_type":true}"#.to_owned(),
            record("user", 2, ""),
            record("assistant", 3, r#","message":{"id":"m","usage":{"input_tokens":"many"}}"#),
            r#"{"type":"assistant","sessionId":"s","cwd":"/","timestamp":"yesterday","message":{}}"#.to_owned(),
            String::new(),
            "   ".to_owned(),
            assistant(4, "ok", r#"{"type":"text","text":"fine"}"#, r#""end_turn""#),
        ],
    );
    let adapter = adapter();
    let parsed = adapter.read(&file, &Cursor::START, u64::MAX).unwrap();
    assert_eq!(parsed.stats.bad_lines, 6);
    assert!(parsed.batch.at_end);
    assert_eq!(
        parsed.batch.cursor.offset(),
        std::fs::metadata(&file.path).unwrap().len()
    );
    let kinds: Vec<&str> = parsed
        .batch
        .records
        .iter()
        .map(|r| match r.value {
            Record::Context { .. } => "context",
            Record::TurnStarted { .. } => "turn",
            Record::Usage(_) => "usage",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["context", "turn", "context", "usage"]);
}

#[test]
fn format_unfinished_last_line_is_read_once_complete() {
    let dir = TempDir::new();
    let file = write_lines(&dir, &[prompt(1, "hello")]);
    let line = assistant(2, "m", r#"{"type":"text","text":"hi"}"#, r#""end_turn""#);
    let (head, rest) = line.split_at(40);
    append(&file.path, head.as_bytes());
    let adapter = adapter();
    let first = adapter.read(&file, &Cursor::START, u64::MAX).unwrap().batch;
    assert!(first.at_end);
    assert_eq!(first.cursor.offset(), prompt(1, "hello").len() as u64 + 1);
    append(&file.path, rest.as_bytes());
    let still = adapter.read(&file, &first.cursor, u64::MAX).unwrap().batch;
    assert!(still.records.is_empty() && still.at_end && !still.reset);
    assert_eq!(still.cursor, first.cursor);
    append(&file.path, b"\n");
    let second = adapter.read(&file, &first.cursor, u64::MAX).unwrap().batch;
    assert!(!second.reset);
    assert!(
        second
            .records
            .iter()
            .any(|r| matches!(r.value, Record::Usage(_)))
    );
}

#[test]
fn format_unrecognized_files() {
    let adapter = adapter();
    // 开头 256 KB 里没有会话头。
    let dir = TempDir::new();
    let filler =
        r#"{"type":"file-history-snapshot","messageId":"x","snapshot":{"trackedFileBackups":{}}}"#;
    let mut lines: Vec<String> =
        std::iter::repeat_n(filler.to_owned(), 300 * 1024 / filler.len()).collect();
    lines.push(prompt(1, "too late"));
    let file = write_lines(&dir, &lines);
    let parsed = adapter.read(&file, &Cursor::START, u64::MAX).unwrap().batch;
    assert!(parsed.records.is_empty() && parsed.at_end && !parsed.reset);
    assert_eq!(parsed.cursor, Cursor::START);
    assert_eq!(adapter.turns(&file), Err(SessionError::Unrecognized));
    // 读不到 cwd：不列出。
    assert_eq!(
        adapter.summarize(&file).unwrap_err(),
        SessionError::Unrecognized
    );

    // 有 cwd、没有会话头：照常列出，`Format::Unrecognized`。
    let dir = TempDir::new();
    let file = write_lines(
        &dir,
        &[r#"{"type":"system","cwd":"/Users/dev/fixture/elsewhere","timestamp":"2026-10-02T10:00:00.000Z"}"#.to_owned()],
    );
    let (summary, _) = adapter.summarize(&file).unwrap();
    assert_eq!(summary.format, Format::Unrecognized);
    assert_eq!(&*summary.cwd, Path::new("/Users/dev/fixture/elsewhere"));
    assert_eq!(adapter.turns(&file), Err(SessionError::Unrecognized));
    assert_eq!(adapter.resume(&summary), None);

    // 空文件。
    let dir = TempDir::new();
    let file = write_lines(&dir, &[]);
    let parsed = adapter.read(&file, &Cursor::START, u64::MAX).unwrap().batch;
    assert!(parsed.records.is_empty() && parsed.at_end);
    assert_eq!(adapter.turns(&file), Err(SessionError::Unrecognized));

    // 文件不存在。
    std::fs::remove_file(&file.path).unwrap();
    assert_eq!(
        adapter.read(&file, &Cursor::START, 1),
        Err(SessionError::NotFound)
    );
    assert_eq!(adapter.turns(&file), Err(SessionError::NotFound));
    assert_eq!(
        adapter.summarize(&file).unwrap_err(),
        SessionError::NotFound
    );
}
