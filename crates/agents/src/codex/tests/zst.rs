//! 压缩会话与解压副本（`zst_*`、`decoded_*`）。`.jsonl.zst` 在临时目录里用 ruzstd 的编码器从合成的 fixtures 生成。

use std::time::{Duration, SystemTime};

use ruzstd::encoding::{CompressionLevel, compress_to_vec};

use super::*;
use crate::codex::decoded::DEFAULT_BUDGET;

fn compress(bytes: &[u8]) -> Vec<u8> {
    compress_to_vec(bytes, CompressionLevel::Fastest)
}

/// 把一份 fixture 压缩进 `dir`，文件名加 `.zst`。
fn compressed(codex: &Codex, dir: &Path, name: &str) -> SessionFile {
    let source = fixture(name);
    let path = dir.join(format!(
        "{}.zst",
        source.path.file_name().unwrap().to_str().unwrap()
    ));
    std::fs::write(&path, compress(&std::fs::read(&source.path).unwrap())).unwrap();
    codex.session_file(&path).unwrap()
}

/// 带解压副本目录、能找到全部 fixtures 作为来源的适配器。
fn with_copies(decoded: &Path) -> Codex {
    let lineage = MapLineage::default();
    for (n, _) in fixture_files() {
        let f = fixture(n);
        lineage.insert(&f.key.id.clone(), f);
    }
    Codex::new(SourceId(SOURCE_ID.into()), decoded.to_path_buf()).with_lineage(Arc::new(lineage))
}

fn copies(decoded: &Path) -> Vec<PathBuf> {
    let dir = decoded.join(SOURCE_ID);
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    out.sort();
    out
}

/// 索引里的全部正文引用。
fn refs(
    index: &lectern_services::session::TurnIndex,
) -> Vec<lectern_services::session::ContentRef> {
    use lectern_services::session::TurnItem;
    let mut out = Vec::new();
    for t in index.turns.iter() {
        out.extend(t.prompt);
        for i in t.items.iter() {
            match i {
                TurnItem::AgentText(r) | TurnItem::Injected(r) => out.push(*r),
                TurnItem::Tool(c) => {
                    out.push(c.input);
                    out.extend(c.result);
                }
            }
        }
    }
    out
}

#[test]
fn zst_records_turns_and_loads_match_the_jsonl() {
    let plain = adapter();
    for (name, _) in fixture_files() {
        let dir = TempDir::new();
        let decoded = dir.path().join("decoded");
        let codex = with_copies(&decoded);
        let file = compressed(&codex, dir.path(), name);
        assert!(file.compressed);
        let original = fixture(name);
        let (expected, _, _) = read_to_end(&plain, &original, &Cursor::START, u64::MAX);
        let (records, _, _) = read_to_end(&codex, &file, &Cursor::START, 700);
        assert_eq!(records, expected, "{name}");
        let index = codex.turns(&file).unwrap();
        let mut expected_index = plain.turns(&original).unwrap();
        expected_index.session = file.key.clone();
        assert_eq!(index, expected_index, "{name}");
        for r in refs(&index) {
            assert_eq!(
                codex.load(&file, &r),
                plain.load(&original, &r),
                "{name} {r:?}"
            );
        }
        assert_eq!(codex.decodes(), 1, "{name}: the archive is decoded once");
    }
}

#[test]
fn zst_cursor_taken_before_compression_stays_valid() {
    let codex_dir = TempDir::new();
    let codex = with_copies(&codex_dir.path().join("decoded"));
    let dir = TempDir::new();
    let source = fixture("legacy_main");
    let plain_path = dir.path().join(source.path.file_name().unwrap());
    std::fs::copy(&source.path, &plain_path).unwrap();
    let plain = codex.session_file(&plain_path).unwrap();
    let first = codex.read(&plain, &Cursor::START, 5000).unwrap().batch;
    assert!(!first.at_end);
    // Codex 把冷文件压缩，原文件删除。
    let bytes = std::fs::read(&plain_path).unwrap();
    let zst_path = dir.path().join(format!(
        "{}.zst",
        plain_path.file_name().unwrap().to_str().unwrap()
    ));
    std::fs::write(&zst_path, compress(&bytes)).unwrap();
    std::fs::remove_file(&plain_path).unwrap();
    let zst = codex.session_file(&zst_path).unwrap();
    assert_eq!(zst.key, plain.key, "the same session");
    let (rest, _, batches) = read_to_end(&codex, &zst, &first.cursor, u64::MAX);
    assert!(!batches[0].batch.reset);
    let mut joined = first.records;
    joined.extend(rest);
    assert_eq!(
        joined,
        read_to_end(&adapter(), &source, &Cursor::START, u64::MAX).0
    );
}

#[test]
fn zst_batches_and_loads_do_not_decode_again() {
    let dir = TempDir::new();
    let decoded = dir.path().join("decoded");
    let codex = with_copies(&decoded);
    let file = compressed(&codex, dir.path(), "paginated_main");
    assert_eq!(codex.decodes(), 0);
    let (_, _, batches) = read_to_end(&codex, &file, &Cursor::START, 1);
    assert!(batches.len() > 10, "many batches");
    let index = codex.turns(&file).unwrap();
    for r in refs(&index) {
        codex.load(&file, &r).unwrap();
    }
    assert_eq!(codex.decodes(), 1);
    assert_eq!(copies(&decoded).len(), 1);
    // 副本是只读的。
    let copy = &copies(&decoded)[0];
    assert!(std::fs::metadata(copy).unwrap().permissions().readonly());
}

#[test]
fn zst_identity_change_rebuilds_the_copy() {
    let dir = TempDir::new();
    let decoded = dir.path().join("decoded");
    let codex = with_copies(&decoded);
    let file = compressed(&codex, dir.path(), "legacy_fork");
    let before = read_to_end(&codex, &file, &Cursor::START, u64::MAX).0;
    assert_eq!(codex.decodes(), 1);
    let old = copies(&decoded);
    // 被重新压缩成另一份内容（追加了一行）。
    let mut bytes = std::fs::read(fixture("legacy_fork").path).unwrap();
    bytes.extend_from_slice(
        event(59, r#"{"type":"task_started","turn_id":"turn-l2-0002"}"#).as_bytes(),
    );
    bytes.push(b'\n');
    std::fs::write(&file.path, compress(&bytes)).unwrap();
    let after = read_to_end(&codex, &file, &Cursor::START, u64::MAX).0;
    assert_eq!(codex.decodes(), 2);
    assert!(after.len() > before.len());
    let new = copies(&decoded);
    assert_eq!(new.len(), 1, "the old copy is removed");
    assert_ne!(new, old);
    // 压缩文件消失：发现时删掉副本。
    codex.remove_decoded(&file.key);
    assert!(copies(&decoded).is_empty());
}

#[test]
fn zst_summary_reads_the_head_and_marks_the_tail_pending() {
    let plain = adapter();
    for (name, _) in fixture_files() {
        let dir = TempDir::new();
        let decoded = dir.path().join("decoded");
        let codex = with_copies(&decoded);
        let file = compressed(&codex, dir.path(), name);
        let (expected, expected_stats) = plain.summarize(&fixture(name)).unwrap();
        let mtime = std::fs::metadata(&file.path).unwrap().modified().unwrap();
        let mtime = lectern_services::session::Timestamp(
            mtime
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64,
        );
        let (summary, stats) = codex.summarize(&file).unwrap();
        assert!(summary.tail_pending, "{name}");
        assert_eq!(summary.last_activity, mtime);
        assert!(!summary.open_turn);
        assert_eq!(summary.title, expected.title, "{name}");
        assert_eq!(summary.cwd, expected.cwd);
        assert_eq!(summary.started_at, expected.started_at);
        assert_eq!(summary.parent, expected.parent);
        assert_eq!(summary.format, expected.format);
        assert_eq!(stats, expected_stats, "{name}");
        // 发现不建副本。
        assert_eq!(codex.decodes(), 0);
        assert!(copies(&decoded).is_empty());

        // 补齐：整份流式解码一次，末尾字段与未压缩的相同。
        let (complete, _) = codex.complete(&file).unwrap();
        assert!(!complete.tail_pending);
        assert_eq!(complete.last_activity, expected.last_activity, "{name}");
        assert_eq!(complete.open_turn, expected.open_turn);
        assert!(copies(&decoded).is_empty());
    }
}

#[test]
fn zst_complete_finds_an_open_turn() {
    let dir = TempDir::new();
    let file = write_lines(
        &dir,
        L1,
        &[
            meta_line(L1, ""),
            event(1, r#"{"type":"task_started","turn_id":"t1"}"#),
            event(2, r#"{"type":"agent_message","message":"working"}"#),
        ],
    );
    let zst = dir.path().join(rollout_name(L1, true));
    std::fs::write(&zst, compress(&std::fs::read(&file.path).unwrap())).unwrap();
    let codex = with_copies(&dir.path().join("decoded"));
    let (summary, _) = codex.complete(&codex.session_file(&zst).unwrap()).unwrap();
    assert!(summary.open_turn);
    assert_eq!(
        summary.last_activity,
        crate::time::parse("2026-10-09T09:00:02.000Z").unwrap()
    );
}

#[test]
fn decoded_copies_are_evicted_by_lru_except_those_in_use() {
    let dir = TempDir::new();
    let decoded = dir.path().join("decoded");
    let plain_len = std::fs::metadata(fixture("legacy_main").path)
        .unwrap()
        .len();
    // 放得下两份。
    let codex = with_copies(&decoded).with_decoded_budget(plain_len * 2 + plain_len / 2);
    assert!(plain_len * 3 < DEFAULT_BUDGET);
    let mut files = Vec::new();
    for thread in [
        "dddddddd-0000-4000-8000-000000000001",
        "dddddddd-0000-4000-8000-000000000002",
        "dddddddd-0000-4000-8000-000000000003",
    ] {
        let path = dir.path().join(rollout_name(thread, true));
        std::fs::write(
            &path,
            compress(&std::fs::read(fixture("legacy_main").path).unwrap()),
        )
        .unwrap();
        files.push(codex.session_file(&path).unwrap());
    }
    // 第一份一直在使用（持有打开的副本）；第二份比它新、但没有在用。
    let held = codex.hold_copy(&files[0]).unwrap();
    let old = SystemTime::now() - Duration::from_secs(3600);
    std::fs::File::open(&held)
        .unwrap()
        .set_modified(old)
        .unwrap();
    codex.read(&files[1], &Cursor::START, u64::MAX).unwrap();
    let second = copies(&decoded).into_iter().find(|p| *p != held).unwrap();
    std::fs::File::open(&second)
        .unwrap()
        .set_modified(old + Duration::from_secs(60))
        .unwrap();
    assert_eq!(copies(&decoded).len(), 2);
    // 第三份让合计超过预算：删掉最旧的、没有在用的那一份（第二份）。
    codex.read(&files[2], &Cursor::START, u64::MAX).unwrap();
    let left = copies(&decoded);
    assert_eq!(left.len(), 2);
    assert!(left.contains(&held), "the copy in use is kept");
    assert!(
        !left.contains(&second),
        "the least recently used copy is evicted"
    );
    // 被淘汰的副本下次需要时重建，结果不变。
    let before = codex.decodes();
    let again = read_to_end(&codex, &files[1], &Cursor::START, u64::MAX).0;
    assert_eq!(codex.decodes(), before + 1);
    assert_eq!(
        again,
        read_to_end(
            &adapter(),
            &fixture("legacy_main"),
            &Cursor::START,
            u64::MAX
        )
        .0
    );
}

#[test]
fn decoded_unwritable_dir_falls_back_to_decoding_from_the_start() {
    let dir = TempDir::new();
    // 副本目录的上一级是一个普通文件：建不了目录（以 root 运行时同样如此）。
    let blocker = dir.path().join("not-a-dir");
    std::fs::write(&blocker, b"").unwrap();
    let codex = with_copies(&blocker.join("decoded"));
    let plain = adapter();
    for name in ["legacy_main", "paginated_subagent"] {
        let file = compressed(&codex, dir.path(), name);
        let original = fixture(name);
        // 退回从头解码时不分批：一次读到末尾。
        let first = codex.read(&file, &Cursor::START, 1).unwrap();
        assert!(first.batch.at_end, "{name}");
        assert_eq!(
            first.batch.records,
            read_to_end(&plain, &original, &Cursor::START, u64::MAX).0
        );
        let index = codex.turns(&file).unwrap();
        for r in refs(&index) {
            assert_eq!(
                codex.load(&file, &r),
                plain.load(&original, &r),
                "{name} {r:?}"
            );
        }
        assert_eq!(codex.decodes(), 0);
    }
}

#[test]
fn zst_missing_or_corrupt_archives() {
    let dir = TempDir::new();
    let codex = with_copies(&dir.path().join("decoded"));
    let path = dir.path().join(rollout_name(L1, true));
    let file = codex.session_file(&path).unwrap();
    assert_eq!(
        codex.read(&file, &Cursor::START, 1),
        Err(lectern_services::session::SessionError::NotFound)
    );
    std::fs::write(&path, b"not a zstd frame").unwrap();
    assert!(matches!(
        codex.read(&file, &Cursor::START, 1),
        Err(lectern_services::session::SessionError::Io(_))
    ));
    assert!(copies(&dir.path().join("decoded")).is_empty());
}
