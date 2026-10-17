//! 摘要、末尾字段、恢复命令与轮次划分（`resume_*`、`tail_*`、`claude_turns_*`）。

use std::fs::OpenOptions;
use std::io::Write as _;
use std::time::UNIX_EPOCH;

use lectern_services::session::{Cursor, Format, ResumeCommand, Timestamp, TurnItem};

use super::*;
use crate::claude::Activity;
use crate::lines::MAX_LINE;

fn mtime(path: &Path) -> Timestamp {
    let d = std::fs::metadata(path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap();
    Timestamp(d.as_millis() as i64)
}

fn ts(s: &str) -> Timestamp {
    crate::time::parse(s).unwrap()
}

#[test]
fn resume_gives_the_session_id_of_main_sessions_only() {
    let adapter = adapter();
    for (id, rel) in [
        (MAIN, format!("{MAIN}.jsonl")),
        (FORK, format!("{FORK}.jsonl")),
    ] {
        let (summary, _) = adapter.summarize(&fixture(&rel)).unwrap();
        assert_eq!(
            adapter.resume(&summary),
            Some(ResumeCommand::ClaudeCode {
                session_id: id.into()
            })
        );
    }
    for rel in [SUB1, SUB2] {
        let (summary, _) = adapter.summarize(&fixture(rel)).unwrap();
        assert_eq!(adapter.resume(&summary), None, "subagent sessions");
    }
    let (mut summary, _) = adapter
        .summarize(&fixture(&format!("{MAIN}.jsonl")))
        .unwrap();
    summary.format = Format::Unrecognized;
    assert_eq!(adapter.resume(&summary), None, "unrecognized sessions");
    summary.format = Format::Partial;
    assert!(adapter.resume(&summary).is_some());
    let other = ClaudeCode::new(SourceId("acp-claude-code".into()));
    assert_eq!(other.resume(&summary), None, "another source's session");
}

#[test]
fn tail_matches_the_parser_state() {
    let adapter = adapter();
    for (name, rel) in fixture_files() {
        let file = fixture(&rel);
        let (summary, _) = adapter.summarize(&file).unwrap();
        let cursor = adapter
            .read(&file, &Cursor::START, u64::MAX)
            .unwrap()
            .batch
            .cursor;
        assert_eq!(
            adapter.activity(&cursor),
            Some(Activity {
                last_activity: Some(summary.last_activity),
                open_turn: summary.open_turn
            }),
            "{name}"
        );
        assert!(!summary.open_turn, "{name} ends with a finished turn");
    }
    assert_eq!(adapter.activity(&Cursor::START), None);
}

#[test]
fn tail_skips_trailing_non_message_records() {
    // 主会话以 `system` 和没有时间戳的 `last-prompt` 结尾。
    let (summary, _) = adapter()
        .summarize(&fixture(&format!("{MAIN}.jsonl")))
        .unwrap();
    assert_eq!(summary.last_activity, ts("2026-10-01T09:00:53.371Z"));
    assert!(!summary.open_turn);
}

fn summarize_lines(
    lines: &[String],
) -> (
    lectern_services::session::SessionSummary,
    crate::ParseStats,
    TempDir,
) {
    let dir = TempDir::new();
    let file = write_lines(&dir, lines);
    let (summary, stats) = adapter().summarize(&file).unwrap();
    (summary, stats, dir)
}

#[test]
fn tail_open_turn_follows_the_last_message() {
    let tool = r#"{"type":"tool_use","id":"t","name":"Bash","input":{"command":"ls"}}"#;
    let cases = [
        (vec![prompt(1, "hi")], true),
        (
            vec![prompt(1, "hi"), assistant(2, "m", tool, r#""tool_use""#)],
            true,
        ),
        (
            vec![
                prompt(1, "hi"),
                assistant(2, "m", r#"{"type":"text","text":"x"}"#, "null"),
            ],
            true,
        ),
        (
            vec![
                prompt(1, "hi"),
                assistant(2, "m", r#"{"type":"text","text":"x"}"#, r#""end_turn""#),
            ],
            false,
        ),
        (
            vec![
                prompt(1, "hi"),
                assistant(2, "m", tool, r#""tool_use""#),
                record(
                    "user",
                    3,
                    r#","message":{"content":[{"type":"tool_result","tool_use_id":"t","content":"ok"}]}"#,
                ),
            ],
            true,
        ),
        (
            vec![prompt(1, "hi"), prompt(2, "[Request interrupted by user]")],
            false,
        ),
        (
            vec![
                prompt(1, "hi"),
                prompt(
                    2,
                    "<local-command-stdout>placeholder</local-command-stdout>",
                ),
            ],
            false,
        ),
    ];
    for (lines, open) in cases {
        let (summary, _, _dir) = summarize_lines(&lines);
        assert_eq!(summary.open_turn, open, "{lines:?}");
        // 读取时的解析器状态给出相同的结果。
        let dir = TempDir::new();
        let file = write_lines(&dir, &lines);
        let cursor = adapter()
            .read(&file, &Cursor::START, u64::MAX)
            .unwrap()
            .batch
            .cursor;
        assert_eq!(
            adapter().activity(&cursor).unwrap().open_turn,
            open,
            "{lines:?}"
        );
    }
}

#[test]
fn tail_reads_a_last_record_larger_than_a_chunk() {
    for size in [200 << 10, MAX_LINE - 1024] {
        let text = "t".repeat(size);
        let (summary, stats, _dir) = summarize_lines(&[
            prompt(1, "hi"),
            assistant(
                7,
                "m",
                &format!(r#"{{"type":"text","text":"{text}"}}"#),
                r#""end_turn""#,
            ),
        ]);
        assert_eq!(
            summary.last_activity,
            ts("2026-10-02T10:00:07.000Z"),
            "{size}"
        );
        assert!(!summary.open_turn);
        assert_eq!(stats.oversized_lines, 0);
    }
}

#[test]
fn tail_falls_back_after_an_oversized_last_record() {
    let text = "o".repeat(MAX_LINE + 1);
    let (summary, stats, dir) = summarize_lines(&[
        prompt(1, "hi"),
        assistant(2, "m", r#"{"type":"text","text":"x"}"#, r#""end_turn""#),
        record(
            "user",
            3,
            &format!(
                r#","message":{{"content":[{{"type":"tool_result","tool_use_id":"t","content":"{text}"}}]}}"#
            ),
        ),
    ]);
    assert!(summary.open_turn);
    assert_eq!(
        summary.last_activity,
        mtime(&dir.path().join("session.jsonl"))
    );
    assert_eq!(stats.oversized_lines, 1);
    assert_eq!(summary.format, Format::Partial);
}

#[test]
fn tail_falls_back_without_a_message_in_range() {
    let mut lines = vec![prompt(1, "hi")];
    lines.extend((0..70).map(|i| record("system", 10 + i, r#","subtype":"placeholder""#)));
    let (summary, stats, dir) = summarize_lines(&lines);
    assert!(summary.open_turn);
    assert_eq!(
        summary.last_activity,
        mtime(&dir.path().join("session.jsonl"))
    );
    assert_eq!(stats.oversized_lines, 0);
}

#[test]
fn tail_ignores_an_unfinished_last_line() {
    let dir = TempDir::new();
    let file = write_lines(
        &dir,
        &[
            prompt(1, "hi"),
            assistant(2, "m", r#"{"type":"text","text":"x"}"#, r#""end_turn""#),
        ],
    );
    let mut f = OpenOptions::new().append(true).open(&file.path).unwrap();
    f.write_all(br#"{"type":"user","timestamp":"2026-10-02T10:59:00.000Z","mess"#)
        .unwrap();
    let (summary, _) = adapter().summarize(&file).unwrap();
    assert_eq!(summary.last_activity, ts("2026-10-02T10:00:02.000Z"));
    assert!(!summary.open_turn);
}

#[test]
fn claude_summary_head_fields() {
    let (summary, stats, _dir) = summarize_lines(&[
        r#"{"type":"file-history-snapshot","messageId":"x","snapshot":{}}"#.to_owned(),
        record(
            "user",
            1,
            r#","isMeta":true,"message":{"content":"<local-command-caveat>c</local-command-caveat>"}"#,
        ),
        prompt(2, "  \\n  First prompt line\\nsecond line"),
    ]);
    assert_eq!(summary.title.as_deref(), Some("First prompt line"));
    assert_eq!(summary.started_at, ts("2026-10-02T10:00:01.000Z"));
    assert_eq!(summary.agent_version.as_deref(), Some("2.1.302"));
    assert_eq!(summary.logged_branch.as_deref(), Some("main"));
    assert_eq!(summary.format, Format::Known);
    assert_eq!(stats, crate::ParseStats::default());

    // 开头没有 prompt 时没有标题。
    let (summary, _, _dir) = summarize_lines(&[record(
        "user",
        1,
        r#","isMeta":true,"message":{"content":"meta only"}"#,
    )]);
    assert_eq!(summary.title, None);
}

#[test]
fn claude_turns_split_on_human_prompts_only() {
    let dir = TempDir::new();
    let file = write_lines(
        &dir,
        &[
            prompt(1, "one"),
            record(
                "user",
                2,
                r#","isMeta":true,"message":{"content":"hook output"}"#,
            ),
            record(
                "user",
                3,
                r#","isCompactSummary":true,"message":{"content":"This session is being continued"}"#,
            ),
            record(
                "user",
                4,
                r#","origin":{"kind":"peer"},"message":{"content":"from another agent"}"#,
            ),
            prompt(5, "<local-command-stdout>out</local-command-stdout>"),
            record(
                "user",
                6,
                r#","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}"#,
            ),
            record(
                "user",
                7,
                r#","origin":{"kind":"human"},"message":{"content":[{"type":"text","text":"two"},{"type":"image","source":{}}]}"#,
            ),
            assistant(8, "m", r#"{"type":"text","text":"   "}"#, r#""end_turn""#),
        ],
    );
    let adapter = adapter();
    let index = adapter.turns(&file).unwrap();
    let previews: Vec<&str> = index.turns.iter().map(|t| &*t.prompt_preview).collect();
    assert_eq!(previews, ["one", "two"]);
    assert_eq!(index.turns[0].items.len(), 5);
    assert!(
        index.turns[0]
            .items
            .iter()
            .all(|i| matches!(i, TurnItem::Injected(_)))
    );
    assert_eq!(index.turns[0].ended_at, None);
    // 空白的 text 块不进索引；`end_turn` 记下结束时间。
    assert!(index.turns[1].items.is_empty());
    assert_eq!(
        index.turns[1].ended_at,
        Some(ts("2026-10-02T10:00:08.000Z"))
    );
    let TurnItem::Injected(r) = &index.turns[0].items[1] else {
        unreachable!()
    };
    assert_eq!(
        adapter.load(&file, r).unwrap(),
        lectern_services::session::Content::Text("This session is being continued".into())
    );
}
