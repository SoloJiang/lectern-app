//! 每份 fixture 的 `Record` 序列、`TurnIndex`、`SessionSummary` 快照，以及 `load`（`codex_*`）。

use lectern_services::session::{
    Agent, Content, ContentRef, EditOp, FileEdit, SessionError, ToolCall, ToolStatus, TurnIndex,
    TurnItem,
};

use super::*;

#[test]
fn codex_fixture_snapshots() {
    let adapter = adapter();
    for (name, rel) in fixture_files() {
        let file = fixture(name);
        let (parsed, index) = adapter.parse_all(&file).unwrap();
        assert!(parsed.batch.at_end);
        insta::assert_snapshot!(
            format!("codex_{name}_records"),
            render(&parsed.batch.records)
        );
        insta::assert_debug_snapshot!(format!("codex_{name}_turns"), index.turns);
        assert_eq!(index.cursor, parsed.batch.cursor);
        assert_eq!(index.format, parsed.stats.format());

        let (mut summary, stats) = adapter.summarize(&file).unwrap();
        // 快照里不出现本机的检出路径。
        summary.file = Path::new(&rel).into();
        insta::assert_debug_snapshot!(format!("codex_{name}_summary"), (summary, stats));
    }
}

#[test]
fn codex_parse_file_matches_read() {
    // `parse_file` 找不到来源会话：只比较不需要来源的 fixtures。
    for name in ["legacy_main", "paginated_main", "paginated_subagent"] {
        let path = fixtures_dir().join(rel(name));
        let (records, index) = crate::testing::parse_file(Agent::Codex, &path).unwrap();
        let file = fixture(name);
        let (read, _, _) = read_to_end(&adapter(), &file, &Cursor::START, u64::MAX);
        assert_eq!(records, read, "{name}");
        assert_eq!(index, adapter().turns(&file).unwrap(), "{name}");
    }
    let claude =
        crate::claude::tests::project_dir().join("11111111-1111-4111-8111-111111111111.jsonl");
    assert_eq!(
        crate::testing::parse_file(Agent::Codex, &claude),
        Err(SessionError::Unrecognized)
    );
}

pub(super) fn tool(index: &TurnIndex, id: &str) -> ToolCall {
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

pub(super) fn edits(content: Content) -> Vec<FileEdit> {
    match content {
        Content::Edits(e) => e.to_vec(),
        Content::Text(t) => panic!("expected edits, got text {t:?}"),
    }
}

pub(super) fn text(content: Content) -> String {
    match content {
        Content::Text(t) => t.to_string(),
        Content::Edits(e) => panic!("expected text, got edits {e:?}"),
    }
}

#[test]
fn codex_load_reads_prompts_messages_and_tool_io() {
    let adapter = adapter();
    let file = fixture("legacy_main");
    let index = adapter.turns(&file).unwrap();
    let turn = &index.turns[0];
    assert_eq!(
        text(adapter.load(&file, &turn.prompt.unwrap()).unwrap()),
        "Placeholder prompt one: list the files.\nSecond line of the placeholder prompt."
    );
    let TurnItem::AgentText(first) = &turn.items[0] else {
        panic!("agent text first: {:?}", turn.items)
    };
    assert_eq!(
        text(adapter.load(&file, first).unwrap()),
        "I will look at the placeholder files first."
    );
    let exec = tool(&index, "call_l1_0001");
    assert_eq!(
        text(adapter.load(&file, &exec.input).unwrap()),
        r#"{"cmd":"ls -la","workdir":"src","yield_time_ms":1000}"#
    );
    assert!(
        text(adapter.load(&file, &exec.result.unwrap()).unwrap())
            .ends_with("Output:\nplaceholder.rs\n")
    );
    // 内容项数组的输出：只取文本项。
    let image = tool(&index, "call_l1_0009");
    assert_eq!(
        text(adapter.load(&file, &image.result.unwrap()).unwrap()),
        "Placeholder image attached."
    );
    // 旧版本的 `{content, success}`。
    let lookup = tool(&index, "call_l1_0011");
    assert_eq!(
        text(adapter.load(&file, &lookup.result.unwrap()).unwrap()),
        "Placeholder lookup failed."
    );
    // `local_shell_call` 的输入是 `action`，没有结果记录。
    let shell = tool(&index, "call_l1_0007");
    assert!(
        text(adapter.load(&file, &shell.input).unwrap()).contains(r#""command":["git","status"]"#)
    );
    assert_eq!(shell.result, None);
    assert_eq!(shell.finished.map(|f| f.1), Some(ToolStatus::Completed));

    // paginated：`UserMessage` / `AgentMessage` 的内容。
    let file = fixture("paginated_main");
    let index = adapter.turns(&file).unwrap();
    assert_eq!(
        text(
            adapter
                .load(&file, &index.turns[0].prompt.unwrap())
                .unwrap()
        ),
        "Placeholder paginated prompt: fix the bug."
    );
    let agent = index.turns[0]
        .items
        .iter()
        .find_map(|i| match i {
            TurnItem::AgentText(r) => Some(*r),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        text(adapter.load(&file, &agent).unwrap()),
        "Fixed the placeholder bug."
    );
}

#[test]
fn codex_load_patches_from_input_and_result() {
    let adapter = adapter();
    let file = fixture("legacy_main");
    let index = adapter.turns(&file).unwrap();
    // 第二轮的 cwd 是 `crates/core`：补丁里的相对路径按它拼接。
    let core = Path::new("/Users/dev/fixture/repo/crates/core");
    let patch = tool(&index, "call_l1_0005");
    assert_eq!(
        edits(adapter.load(&file, &patch.input).unwrap()),
        [
            FileEdit {
                path: core.join("notes.txt"),
                op: EditOp::Add {
                    content: "placeholder note\n".into()
                },
            },
            FileEdit {
                path: core.join("src/lib.rs"),
                op: EditOp::Patch {
                    hunks: "@@\n-old placeholder line\n+new placeholder line\n".into(),
                    move_to: None,
                },
            },
            FileEdit {
                path: core.join("old.txt"),
                op: EditOp::Delete,
            },
            FileEdit {
                path: core.join("src/a.rs"),
                op: EditOp::Patch {
                    hunks: "@@\n-a\n+b\n".into(),
                    move_to: Some(core.join("src/b.rs")),
                },
            },
        ]
    );
    // 结果指向 `patch_apply_end` 的改动（按路径排序）。
    assert_eq!(
        edits(adapter.load(&file, &patch.result.unwrap()).unwrap()),
        [
            FileEdit {
                path: core.join("notes.txt"),
                op: EditOp::Add {
                    content: "placeholder note\n".into()
                },
            },
            FileEdit {
                path: core.join("old.txt"),
                op: EditOp::Delete,
            },
            FileEdit {
                path: core.join("src/a.rs"),
                op: EditOp::Patch {
                    hunks: "@@ -1 +1 @@\n-a\n+b\n".into(),
                    move_to: Some(core.join("src/b.rs")),
                },
            },
            FileEdit {
                path: core.join("src/lib.rs"),
                op: EditOp::Patch {
                    hunks: "@@ -1 +1 @@\n-old placeholder line\n+new placeholder line\n".into(),
                    move_to: None,
                },
            },
        ]
    );
    // `function_call` 形式的 `apply_patch`：失败的补丁，结果是空的改动。
    let failed = tool(&index, "call_l1_0006");
    assert_eq!(failed.finished.map(|f| f.1), Some(ToolStatus::Failed));
    assert_eq!(
        edits(adapter.load(&file, &failed.input).unwrap()),
        [FileEdit {
            path: "/Users/dev/fixture/repo/README.md".into(),
            op: EditOp::Patch {
                hunks: "@@\n-x\n+y\n".into(),
                move_to: None
            },
        }]
    );
    assert_eq!(
        edits(adapter.load(&file, &failed.result.unwrap()).unwrap()),
        []
    );

    // paginated：`FileChange`。
    let file = fixture("paginated_main");
    let index = adapter.turns(&file).unwrap();
    let patch = tool(&index, "call_p1_0002");
    assert_eq!(
        edits(adapter.load(&file, &patch.result.unwrap()).unwrap()),
        [FileEdit {
            path: "/Users/dev/fixture/repo/src/main.rs".into(),
            op: EditOp::Patch {
                hunks: "@@ -1 +1 @@\n-bug\n+fix\n".into(),
                move_to: None
            },
        }]
    );
}

#[test]
fn codex_load_relative_patch_paths_far_from_the_turn_context() {
    // 补丁离 `turn_context` 超过 64 行：取 `session_meta` 的 cwd（已知限制，见「与设计的差异」）。
    let dir = TempDir::new();
    let mut lines = vec![
        meta_line(L1, ""),
        line(
            "turn_context",
            1,
            r#"{"turn_id":"t1","cwd":"/Users/dev/fixture/repo/sub","model":"m"}"#,
        ),
        event(2, r#"{"type":"task_started","turn_id":"t1"}"#),
    ];
    for i in 0..70 {
        lines.push(event(
            3,
            &format!(r#"{{"type":"agent_message","message":"note {i}"}}"#),
        ));
    }
    lines.push(item(
        4,
        r#"{"type":"custom_tool_call","call_id":"c1","name":"apply_patch","input":"*** Begin Patch\n*** Delete File: gone.txt\n*** End Patch\n"}"#,
    ));
    let file = write_lines(&dir, L1, &lines);
    let adapter = adapter();
    let index = adapter.turns(&file).unwrap();
    assert_eq!(
        edits(adapter.load(&file, &tool(&index, "c1").input).unwrap()),
        [FileEdit {
            path: "/Users/dev/fixture/repo/gone.txt".into(),
            op: EditOp::Delete
        }]
    );
}

#[test]
fn codex_load_stale_refs() {
    let adapter = adapter();
    let file = fixture("legacy_main");
    let index = adapter.turns(&file).unwrap();
    let input = tool(&index, "call_l1_0001").input;
    for r in [
        ContentRef::new(input.pos(), input.line_len() + 100, input.part()),
        ContentRef::new(input.pos() + 1, input.line_len(), input.part()),
        ContentRef::new(input.pos(), input.line_len(), 7),
        ContentRef::new(1 << 40, 10, 0),
    ] {
        assert!(
            matches!(adapter.load(&file, &r), Err(SessionError::Io(_))),
            "{r:?}"
        );
    }
}
