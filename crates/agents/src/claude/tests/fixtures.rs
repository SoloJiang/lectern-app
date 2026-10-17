//! 每份 fixture 的 `Record` 序列、`TurnIndex`、`SessionSummary` 快照，以及 `load`（`claude_*`）。

use std::path::Path;

use lectern_services::session::{
    Content, ContentRef, Cursor, EditOp, FileEdit, SessionError, ToolStatus, TurnItem,
};

use super::*;

#[test]
fn claude_fixture_snapshots() {
    let adapter = adapter();
    for (name, rel) in fixture_files() {
        let file = fixture(&rel);
        let (parsed, index) = adapter.parse_all(&file).unwrap();
        assert!(parsed.batch.at_end);
        insta::assert_snapshot!(
            format!("claude_{name}_records"),
            render(&parsed.batch.records)
        );
        insta::assert_debug_snapshot!(format!("claude_{name}_turns"), index.turns);
        assert_eq!(index.cursor, parsed.batch.cursor);
        assert_eq!(index.format, parsed.stats.format());

        let (mut summary, stats) = adapter.summarize(&file).unwrap();
        // 快照里不出现本机的检出路径。
        summary.file = Path::new(&rel).into();
        insta::assert_debug_snapshot!(format!("claude_{name}_summary"), (summary, stats));
    }
}

#[test]
fn claude_parse_file_matches_read() {
    for (_, rel) in fixture_files() {
        let path = project_dir().join(&rel);
        let (records, index) =
            crate::testing::parse_file(lectern_services::session::Agent::ClaudeCode, &path)
                .unwrap();
        let file = fixture(&rel);
        let (read, _, _) = read_to_end(&adapter(), &file, &Cursor::START, u64::MAX);
        assert_eq!(records, read);
        assert_eq!(index, adapter().turns(&file).unwrap());
    }
    assert_eq!(
        crate::testing::parse_file(
            lectern_services::session::Agent::Codex,
            &project_dir().join(format!("{MAIN}.jsonl"))
        ),
        Err(SessionError::Unrecognized)
    );
}

/// 主会话索引里的工具调用，按名字找第 n 个。
fn tool(
    index: &lectern_services::session::TurnIndex,
    id: &str,
) -> lectern_services::session::ToolCall {
    index
        .turns
        .iter()
        .flat_map(|t| t.items.iter())
        .find_map(|i| match i {
            TurnItem::Tool(t) if &*t.id.0 == id => Some(t.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no tool call {id}"))
}

fn edits(content: Content) -> Vec<FileEdit> {
    match content {
        Content::Edits(e) => e.to_vec(),
        Content::Text(t) => panic!("expected edits, got text {t:?}"),
    }
}

fn text(content: Content) -> String {
    match content {
        Content::Text(t) => t.to_string(),
        Content::Edits(e) => panic!("expected text, got edits {e:?}"),
    }
}

#[test]
fn claude_load_reads_prompts_text_and_edits() {
    let adapter = adapter();
    let file = fixture(&format!("{MAIN}.jsonl"));
    let index = adapter.turns(&file).unwrap();
    let repo = Path::new("/Users/dev/fixture/repo");

    let turn = &index.turns[0];
    assert_eq!(
        text(adapter.load(&file, &turn.prompt.unwrap()).unwrap()),
        "Placeholder prompt one: add the fixture module.\nSecond line of the placeholder prompt."
    );
    let TurnItem::AgentText(first) = &turn.items[0] else {
        panic!("agent text first")
    };
    assert_eq!(
        text(adapter.load(&file, first).unwrap()),
        "I will read the placeholder file first."
    );

    // 成功的 Edit：输入是替换，结果是统一 diff。
    let edit = tool(&index, "toolu_fixture_0002");
    assert_eq!(
        edits(adapter.load(&file, &edit.input).unwrap()),
        [FileEdit {
            path: repo.join("src/lib.rs"),
            op: EditOp::Replace {
                old: "old placeholder line".into(),
                new: "new placeholder line\nadded placeholder line".into(),
                all: false,
            },
        }]
    );
    assert_eq!(
        edits(adapter.load(&file, &edit.result.unwrap()).unwrap()),
        [FileEdit {
            path: repo.join("src/lib.rs"),
            op: EditOp::Patch {
                hunks: "@@ -1,1 +1,2 @@\n-old placeholder line\n+new placeholder line\n+added placeholder line\n"
                    .into(),
                move_to: None,
            },
        }]
    );

    // 失败的 Edit：结果只有错误文本。
    let failed = tool(&index, "toolu_fixture_0003");
    assert_eq!(failed.finished.unwrap().1, ToolStatus::Failed);
    assert!(text(adapter.load(&file, &failed.result.unwrap()).unwrap()).contains("not found"));

    // Write：输入看不出是否新建，结果里 `originalFile: null` 是新建。
    let write = tool(&index, "toolu_fixture_0004");
    let created = |content| match &edits(content)[0].op {
        EditOp::Write { created, content } => (*created, content.to_string()),
        op => panic!("{op:?}"),
    };
    assert_eq!(
        created(adapter.load(&file, &write.input).unwrap()),
        (false, "pub fn placeholder() {}\n".into())
    );
    assert_eq!(
        created(adapter.load(&file, &write.result.unwrap()).unwrap()),
        (true, "pub fn placeholder() {}\n".into())
    );

    // 其他工具：输入是原样的 JSON，结果是文本。
    let bash = tool(&index, "toolu_fixture_0006");
    assert_eq!(
        text(adapter.load(&file, &bash.input).unwrap()),
        r#"{"command":"cd crates && cargo test --quiet","description":"Placeholder description"}"#
    );
    let task = tool(&index, "toolu_fixture_0007a");
    assert_eq!(
        text(adapter.load(&file, &task.result.unwrap()).unwrap()),
        "Placeholder agent one report."
    );
}

#[test]
fn claude_load_multi_edit_and_stale_refs() {
    let dir = TempDir::new();
    let file = write_lines(
        &dir,
        &[
            prompt(1, "edit twice"),
            assistant(
                2,
                "m1",
                r#"{"type":"tool_use","id":"t1","name":"MultiEdit","input":{"file_path":"src/a.rs","edits":[{"old_string":"a","new_string":"b"},{"old_string":"c","new_string":"d","replace_all":true}]}}"#,
                r#""tool_use""#,
            ),
        ],
    );
    let adapter = adapter();
    let index = adapter.turns(&file).unwrap();
    let call = tool(&index, "t1");
    let path = Path::new("/Users/dev/fixture/repo/src/a.rs").to_path_buf();
    assert_eq!(
        edits(adapter.load(&file, &call.input).unwrap()),
        [
            FileEdit {
                path: path.clone(),
                op: EditOp::Replace {
                    old: "a".into(),
                    new: "b".into(),
                    all: false
                }
            },
            FileEdit {
                path,
                op: EditOp::Replace {
                    old: "c".into(),
                    new: "d".into(),
                    all: true
                }
            },
        ]
    );

    // 引用对不上文件：越界、不在行首、部分不存在。
    let stale = |r: ContentRef| adapter.load(&file, &r);
    let input = call.input;
    for r in [
        ContentRef::new(input.pos(), input.line_len() + 100, input.part()),
        ContentRef::new(input.pos() + 1, input.line_len(), input.part()),
        ContentRef::new(input.pos(), input.line_len(), 7),
        ContentRef::new(1 << 40, 10, 0),
    ] {
        assert!(matches!(stale(r), Err(SessionError::Io(_))), "{r:?}");
    }
    std::fs::remove_file(&file.path).unwrap();
    assert_eq!(adapter.load(&file, &input), Err(SessionError::NotFound));
}
