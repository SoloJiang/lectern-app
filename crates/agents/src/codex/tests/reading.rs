//! 增量读取、改写与截断、读取进度（Codex 部分的 `incremental_*`、`reset_*`、`progress_*`）。

use std::fs::OpenOptions;
use std::io::Write as _;

use lectern_services::session::{Format, SessionError, TurnIndex};

use super::*;
use crate::codex::ADAPTER_VERSION;
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

fn round_trip(cursor: &Cursor) -> Cursor {
    let back = Cursor::from_bytes(&cursor.to_bytes()).unwrap();
    let json = serde_json::to_string(&back).unwrap();
    serde_json::from_str(&json).unwrap()
}

/// 一份 fixture 复制到临时目录（文件名相同，所以线程 ID 相同）。
fn copy(dir: &TempDir, name: &str) -> SessionFile {
    let source = fixture(name);
    let path = dir.path().join(source.path.file_name().unwrap());
    std::fs::copy(&source.path, &path).unwrap();
    adapter().session_file(&path).unwrap()
}

#[test]
fn incremental_codex_random_appends_match_a_single_read() {
    let codex = adapter();
    for (name, _) in fixture_files() {
        let fixture = fixture(name);
        let bytes = std::fs::read(&fixture.path).unwrap();
        let (expected, expected_cursor, _) =
            read_to_end(&codex, &fixture, &Cursor::START, u64::MAX);
        let expected_index = codex.turns(&fixture).unwrap();
        for seed in 1..=8u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let dir = TempDir::new();
            let path = dir.path().join(fixture.path.file_name().unwrap());
            std::fs::write(&path, b"").unwrap();
            let file = codex.session_file(&path).unwrap();
            let mut cursor = Cursor::START;
            let mut records = Vec::new();
            let mut index: Option<TurnIndex> = None;
            let mut written = 0;
            while written < bytes.len() {
                let n = 1 + rng.below(bytes.len() as u64 / 3) as usize;
                let end = (written + n).min(bytes.len());
                append(&path, &bytes[written..end]);
                written = end;
                let max_bytes = [1, 97, 4096, u64::MAX][rng.below(4) as usize];
                let (batch, next, parsed) = read_to_end(&codex, &file, &cursor, max_bytes);
                assert!(
                    parsed.iter().all(|p| !p.batch.reset),
                    "{name}: appending never resets"
                );
                records.extend(batch);
                cursor = round_trip(&next);
                index = Some(
                    codex
                        .turns_after(&file, index.as_ref())
                        .unwrap_or_else(|e| {
                            assert_eq!(e, SessionError::Unrecognized);
                            TurnIndex {
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
fn reset_codex_after_truncation_and_rewrite() {
    let codex = adapter();
    for rewrite in [false, true] {
        let dir = TempDir::new();
        let file = copy(&dir, "legacy_main");
        let (fresh, _, _) = read_to_end(&codex, &file, &Cursor::START, u64::MAX);
        let (_, cursor, _) = read_to_end(&codex, &file, &Cursor::START, u64::MAX);
        let mut bytes = std::fs::read(&file.path).unwrap();
        if rewrite {
            // 改写游标之前 256 字节里文本中的一个字节，长度不变、仍是合法的 JSON。
            let at = bytes
                .windows(11)
                .rposition(|w| w == b"Placeholder")
                .unwrap();
            assert!(at > bytes.len() - 256);
            bytes[at] = b'Q';
        } else {
            bytes.truncate(bytes.len() / 2);
        }
        std::fs::write(&file.path, &bytes).unwrap();
        let (replayed, _, batches) = read_to_end(&codex, &file, &cursor, 1000);
        assert!(batches[0].batch.reset);
        assert!(batches[1..].iter().all(|b| !b.batch.reset));
        let (now, _, _) = read_to_end(&codex, &file, &Cursor::START, u64::MAX);
        assert_eq!(replayed, now);
        if rewrite {
            assert_eq!(replayed.len(), fresh.len());
        }
    }
}

#[test]
fn reset_codex_for_foreign_or_unreadable_cursors() {
    let codex = adapter();
    let file = fixture("legacy_main");
    let (_, cursor, _) = read_to_end(&codex, &file, &Cursor::START, u64::MAX);
    let state = cursor.state().to_vec();
    let source = cursor.source().unwrap().clone();
    for from in [
        Cursor::new(
            SourceId("session-claude-code".into()),
            ADAPTER_VERSION,
            cursor.offset(),
            state.clone(),
        ),
        Cursor::new(
            source.clone(),
            ADAPTER_VERSION + 1,
            cursor.offset(),
            state.clone(),
        ),
        Cursor::new(source.clone(), ADAPTER_VERSION, cursor.offset(), vec![9, 9]),
        Cursor::new(
            source.clone(),
            ADAPTER_VERSION,
            cursor.offset(),
            state[..state.len() - 1].to_vec(),
        ),
        Cursor::new(source, ADAPTER_VERSION, cursor.offset() - 3, state),
    ] {
        let (replayed, _, batches) = read_to_end(&codex, &file, &from, u64::MAX);
        assert!(batches[0].batch.reset);
        assert_eq!(
            replayed,
            read_to_end(&codex, &file, &Cursor::START, u64::MAX).0
        );
    }
}

#[test]
fn progress_codex_one_line_per_call_and_oversized_lines() {
    let codex = adapter();
    let file = fixture("paginated_main");
    let bytes = std::fs::read(&file.path).unwrap();
    let ends: Vec<u64> = bytes
        .iter()
        .enumerate()
        .filter(|(_, b)| **b == b'\n')
        .map(|(i, _)| i as u64 + 1)
        .collect();
    let (records, _, batches) = read_to_end(&codex, &file, &Cursor::START, 1);
    let offsets: Vec<u64> = batches.iter().map(|b| b.batch.cursor.offset()).collect();
    assert_eq!(offsets, ends, "one line per call");
    assert_eq!(
        records,
        read_to_end(&codex, &file, &Cursor::START, u64::MAX).0
    );

    // 超过 16 MB 的行跳过、计数，游标越过它。
    let dir = TempDir::new();
    let huge = format!(
        r#"{{"type":"agent_message","message":"{}"}}"#,
        "z".repeat(MAX_LINE + 10)
    );
    let file = write_lines(
        &dir,
        L1,
        &[
            meta_line(L1, ""),
            event(1, r#"{"type":"task_started","turn_id":"t1"}"#),
            event(2, &huge),
            event(3, r#"{"type":"task_complete","turn_id":"t1"}"#),
        ],
    );
    let parsed = codex.read(&file, &Cursor::START, u64::MAX).unwrap();
    assert_eq!(parsed.stats.oversized_lines, 1);
    assert!(parsed.batch.at_end);
    assert_eq!(
        parsed.batch.cursor.offset(),
        std::fs::metadata(&file.path).unwrap().len()
    );
    assert!(
        parsed
            .batch
            .records
            .iter()
            .any(|r| matches!(r.value, Record::TurnEnded { .. }))
    );
}
