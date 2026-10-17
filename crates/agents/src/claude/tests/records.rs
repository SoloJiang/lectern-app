//! 记录的内容：来源与键、子 agent 的 ID、用量键、工具分类、继承前缀
//! （`source_*`、`subagent_*`、`usage_keys_*`、`tool_kind_*`、`inherited_*`）。

use std::path::PathBuf;

use lectern_services::session::{
    Cursor, Positioned, RawTokens, Record, ToolKind, ToolStatus, TurnId, UsageKey, UsageRecord,
};

use super::*;

fn usages(records: &[Positioned<Record>]) -> Vec<&UsageRecord> {
    records
        .iter()
        .filter_map(|r| match &r.value {
            Record::Usage(u) => Some(u),
            _ => None,
        })
        .collect()
}

fn claude_key(message: &str, request: &str) -> UsageKey {
    UsageKey::Claude {
        message_id: message.into(),
        request_id: Some(request.into()),
    }
}

#[test]
fn source_keys_carry_the_provider_id() {
    let a = ClaudeCode::new(SourceId(SOURCE_ID.into()));
    let b = ClaudeCode::new(SourceId("acp-claude-code".into()));
    let path = project_dir().join(format!("{MAIN}.jsonl"));
    let fa = a.session_file(&path).unwrap();
    let fb = b.session_file(&path).unwrap();
    assert_eq!(fa.key.id, fb.key.id, "the same upstream session id");
    assert_ne!(fa.key, fb.key, "two different session keys");
    assert_eq!(fa.key.source, *a.source());
    assert_eq!(fb.key.source, *b.source());
    assert_eq!(a.summarize(&fa).unwrap().0.key, fa.key);
    assert_eq!(b.summarize(&fb).unwrap().0.key, fb.key);
    assert_eq!(a.turns(&fa).unwrap().session, fa.key);

    // 游标记着生成它的来源：交给另一个来源时从头读。
    let cursor = a.read(&fa, &Cursor::START, u64::MAX).unwrap().batch.cursor;
    assert_eq!(cursor.source(), Some(a.source()));
    assert!(b.read(&fb, &cursor, u64::MAX).unwrap().batch.reset);
    assert!(!a.read(&fa, &cursor, u64::MAX).unwrap().batch.reset);
    // 用量键与来源无关。
    let ka: Vec<_> = usages(&a.read(&fa, &Cursor::START, u64::MAX).unwrap().batch.records)
        .iter()
        .map(|u| u.key.clone())
        .collect();
    let kb: Vec<_> = usages(&b.read(&fb, &Cursor::START, u64::MAX).unwrap().batch.records)
        .iter()
        .map(|u| u.key.clone())
        .collect();
    assert_eq!(ka, kb);
}

#[test]
fn subagent_ids_are_parent_slash_agent() {
    let adapter = adapter();
    let sub1 = fixture(SUB1);
    assert_eq!(&*sub1.key.id, format!("{MAIN}/agent-a0fa0001"));
    assert_eq!(
        sub1.owner,
        Owner::Subagent {
            parent_session_id: MAIN.into(),
            agent_id: "a0fa0001".into()
        }
    );
    // 嵌套在 `workflows/<id>/` 下的也归属于 `subagents/` 的上一级目录。
    let sub2 = fixture(SUB2);
    assert_eq!(&*sub2.key.id, format!("{MAIN}/agent-a0fa0002"));
    // 不同父会话里的同名 `agentId` 是不同的会话。
    let other = adapter
        .session_file(Path::new(
            "/Users/dev/fixture/projects/p/33333333-3333-4333-8333-333333333333/subagents/agent-a0fa0001.jsonl",
        ))
        .unwrap();
    assert_ne!(other.key, sub1.key);
    assert_eq!(
        &*other.key.id,
        "33333333-3333-4333-8333-333333333333/agent-a0fa0001"
    );

    let main = fixture(&format!("{MAIN}.jsonl"));
    assert_eq!(&*main.key.id, MAIN);
    for not_a_session in ["x/README.md", "x/.jsonl", "x/agent-a.meta.json"] {
        assert_eq!(adapter.session_file(Path::new(not_a_session)), None);
    }
}

#[test]
fn subagent_parent_and_spawning_tool_call() {
    let adapter = adapter();
    let main_index = adapter.turns(&fixture(&format!("{MAIN}.jsonl"))).unwrap();
    let spawns: Vec<_> = main_index
        .turns
        .iter()
        .flat_map(|t| t.items.iter())
        .filter_map(|i| match i {
            lectern_services::session::TurnItem::Tool(t) if t.kind == ToolKind::SpawnAgent => {
                Some(t.id.clone())
            }
            _ => None,
        })
        .collect();
    for (rel, spawned_by) in [(SUB1, "toolu_fixture_0007a"), (SUB2, "toolu_fixture_0007b")] {
        let (summary, _) = adapter.summarize(&fixture(rel)).unwrap();
        let parent = summary.parent.unwrap();
        assert_eq!(parent.session, fixture(&format!("{MAIN}.jsonl")).key);
        assert_eq!(parent.tool_call.as_ref().map(|c| &*c.0), Some(spawned_by));
        assert!(spawns.contains(parent.tool_call.as_ref().unwrap()));
    }
    // 子 agent 按自己的 cwd：一个在 worktree 里。
    let (sub1, _) = adapter.summarize(&fixture(SUB1)).unwrap();
    assert_eq!(
        &*sub1.cwd,
        Path::new("/Users/dev/fixture/repo/.claude/worktrees/agent-a0fa0001")
    );

    // `meta.json` 缺失或认不出时 `tool_call` 为 None。
    let dir = TempDir::new();
    let sub = dir.path().join(MAIN).join("subagents");
    std::fs::create_dir_all(&sub).unwrap();
    let path = sub.join("agent-a0fa0001.jsonl");
    std::fs::copy(project_dir().join(SUB1), &path).unwrap();
    let file = adapter.session_file(&path).unwrap();
    let parent = adapter.summarize(&file).unwrap().0.parent.unwrap();
    assert_eq!(parent.session.id, MAIN.into());
    assert_eq!(parent.tool_call, None);
    std::fs::write(sub.join("agent-a0fa0001.meta.json"), "{not json").unwrap();
    assert_eq!(
        adapter
            .summarize(&file)
            .unwrap()
            .0
            .parent
            .unwrap()
            .tool_call,
        None
    );
}

#[test]
fn usage_keys_repeat_for_one_response() {
    let adapter = adapter();
    let (records, _, _) = read_to_end(
        &adapter,
        &fixture(&format!("{MAIN}.jsonl")),
        &Cursor::START,
        u64::MAX,
    );
    let usage = usages(&records);
    // 一个响应的三行（thinking、text、tool_use）重复同一个键，usage 逐步变完整。
    let first: Vec<_> = usage
        .iter()
        .filter(|u| u.key == claude_key("msg_fixture_0001", "req_fixture_0001"))
        .collect();
    assert_eq!(first.len(), 3);
    let outputs: Vec<u64> = first
        .iter()
        .map(|u| match u.tokens {
            RawTokens::Anthropic { output, .. } => output,
            RawTokens::OpenAi { .. } => unreachable!(),
        })
        .collect();
    assert_eq!(outputs, [5, 20, 42]);
    assert!(
        first
            .iter()
            .all(|u| u.model.as_deref() == Some("fixture-model-a"))
    );
    assert_eq!(
        first[0].tokens,
        RawTokens::Anthropic {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 100,
            cache_write_5m: 100,
            cache_write_1h: 0
        }
    );

    // 带 advisor 的行与不带的行键相同；advisor 按自己的模型计，不加进 `tokens`。
    let fifth: Vec<_> = usage
        .iter()
        .filter(|u| u.key == claude_key("msg_fixture_0005", "req_fixture_0005"))
        .collect();
    assert_eq!(fifth.len(), 2);
    assert_eq!(fifth[0].advisor, None);
    let advisor = fifth[1].advisor.as_ref().unwrap();
    assert_eq!(&*advisor.model, "fixture-model-b");
    assert_eq!(
        advisor.tokens,
        RawTokens::Anthropic {
            input: 400,
            output: 50,
            cache_read: 0,
            cache_write: 0,
            cache_write_5m: 0,
            cache_write_1h: 0
        }
    );
    assert!(matches!(
        fifth[1].tokens,
        RawTokens::Anthropic {
            input: 3,
            output: 12,
            ..
        }
    ));
    assert!(usage.iter().all(|u| !u.superseded && !u.sidechain));

    // 子 agent 的用量标 `sidechain`。
    let (sub, _, _) = read_to_end(&adapter, &fixture(SUB1), &Cursor::START, u64::MAX);
    assert!(usages(&sub).iter().all(|u| u.sidechain));
}

#[test]
fn usage_keys_without_a_request_id_and_advisor_model_fallback() {
    let dir = TempDir::new();
    let file = write_lines(
        &dir,
        &[
            prompt(1, "hi"),
            record(
                "assistant",
                2,
                r#","message":{"id":"msg_x","model":"fixture-model-a","content":[],"usage":{"input_tokens":1,"output_tokens":1,"iterations":[{"type":"advisor_message","input_tokens":9,"output_tokens":9,"advisorModel":"fixture-model-c"}]}}"#,
            ),
            // 没有 `message.id` 的不交出用量。
            record(
                "assistant",
                3,
                r#","message":{"model":"fixture-model-a","content":[],"usage":{"input_tokens":1}}"#,
            ),
        ],
    );
    let (records, _, _) = read_to_end(&adapter(), &file, &Cursor::START, u64::MAX);
    let usage = usages(&records);
    assert_eq!(usage.len(), 1);
    assert_eq!(
        usage[0].key,
        UsageKey::Claude {
            message_id: "msg_x".into(),
            request_id: None
        }
    );
    assert_eq!(
        usage[0].advisor.as_ref().map(|a| &*a.model),
        Some("fixture-model-c")
    );
    assert_eq!(
        usage[0].turn,
        Some(TurnId("bbbbbbbb-0000-4000-8000-000000000001".into()))
    );
}

/// 一个工具调用的分类，cwd 为 `/Users/dev/fixture/repo`。
fn kind_of(name: &str, input: &str) -> (ToolKind, crate::ParseStats) {
    let dir = TempDir::new();
    let block = format!(r#"{{"type":"tool_use","id":"t","name":"{name}","input":{input}}}"#);
    let file = write_lines(
        &dir,
        &[prompt(1, "go"), assistant(2, "m", &block, r#""tool_use""#)],
    );
    let parsed = adapter().read(&file, &Cursor::START, u64::MAX).unwrap();
    let kind = parsed
        .batch
        .records
        .iter()
        .find_map(|r| match &r.value {
            Record::ToolStarted(t) => Some(t.kind.clone()),
            _ => None,
        })
        .unwrap();
    (kind, parsed.stats)
}

fn paths(p: &[&str]) -> Arc<[PathBuf]> {
    p.iter().map(PathBuf::from).collect()
}

#[test]
fn tool_kind_edit_tools() {
    for (name, input, path) in [
        (
            "Write",
            r#"{"file_path":"/Users/dev/fixture/repo/a.rs","content":"x"}"#,
            "/Users/dev/fixture/repo/a.rs",
        ),
        (
            "Edit",
            r#"{"file_path":"/Users/dev/fixture/repo/b.rs","old_string":"a","new_string":"b"}"#,
            "/Users/dev/fixture/repo/b.rs",
        ),
        (
            "MultiEdit",
            r#"{"file_path":"c.rs","edits":[]}"#,
            "/Users/dev/fixture/repo/c.rs",
        ),
        (
            "NotebookEdit",
            r#"{"notebook_path":"/Users/dev/fixture/repo/n.ipynb","new_source":"x"}"#,
            "/Users/dev/fixture/repo/n.ipynb",
        ),
    ] {
        let (kind, stats) = kind_of(name, input);
        assert_eq!(
            kind,
            ToolKind::Edit {
                paths: paths(&[path])
            },
            "{name}"
        );
        assert!(stats.unknown_tools.is_empty());
    }
    // 没有路径时路径为空。
    assert_eq!(
        kind_of("Write", "{}").0,
        ToolKind::Edit { paths: paths(&[]) }
    );
}

#[test]
fn tool_kind_execute_uses_the_record_cwd() {
    assert_eq!(
        kind_of("Bash", r#"{"command":"cd sub && make","timeout":5}"#).0,
        ToolKind::Execute {
            command: "cd sub && make".into(),
            workdir: Path::new("/Users/dev/fixture/repo").into(),
            exec_session: None,
            workdir_exact: true,
        }
    );
}

#[test]
fn tool_kind_read_tools() {
    assert_eq!(
        kind_of(
            "Read",
            r#"{"file_path":"/Users/dev/fixture/repo/README.md","limit":10}"#
        )
        .0,
        ToolKind::Read {
            paths: paths(&["/Users/dev/fixture/repo/README.md"])
        }
    );
    assert_eq!(
        kind_of("NotebookRead", r#"{"notebook_path":"nb.ipynb"}"#).0,
        ToolKind::Read {
            paths: paths(&["/Users/dev/fixture/repo/nb.ipynb"])
        }
    );
}

#[test]
fn tool_kind_search_tools() {
    for name in ["Grep", "Glob", "LS"] {
        assert_eq!(
            kind_of(
                name,
                r#"{"pattern":"x","path":"/Users/dev/fixture/repo/src"}"#
            )
            .0,
            ToolKind::Search {
                paths: paths(&["/Users/dev/fixture/repo/src"])
            },
            "{name}"
        );
        // 没给路径时是调用时的 cwd。
        assert_eq!(
            kind_of(name, r#"{"pattern":"x"}"#).0,
            ToolKind::Search {
                paths: paths(&["/Users/dev/fixture/repo"])
            },
            "{name}"
        );
    }
}

#[test]
fn tool_kind_spawn_agent_tools() {
    for name in ["Task", "Agent"] {
        let (kind, stats) = kind_of(name, r#"{"prompt":"x"}"#);
        assert_eq!(kind, ToolKind::SpawnAgent, "{name}");
        assert!(stats.unknown_tools.is_empty());
    }
}

#[test]
fn tool_kind_other_tools_are_counted() {
    for name in ["WebFetch", "TodoWrite", "mcp__fixture__lookup"] {
        let (kind, stats) = kind_of(name, r#"{"url":"https://example.invalid","path":[1]}"#);
        assert_eq!(kind, ToolKind::Other, "{name}");
        assert_eq!(stats.unknown_tools, [(name.into(), 1)]);
        assert!(!stats.skipped(), "classification never skips a record");
    }
    let (records, _, batches) = read_to_end(
        &adapter(),
        &fixture(&format!("{MAIN}.jsonl")),
        &Cursor::START,
        u64::MAX,
    );
    assert_eq!(
        batches[0].stats.unknown_tools,
        [("mcp__fixture__lookup".into(), 1)]
    );
    // 输入里类型不对的字段不影响这一行的其他记录。
    assert!(
        records.iter().any(
            |r| matches!(&r.value, Record::ToolStarted(t) if &*t.name == "mcp__fixture__lookup")
        )
    );
}

#[test]
fn tool_kind_results_map_to_statuses() {
    let (records, _, _) = read_to_end(
        &adapter(),
        &fixture(&format!("{MAIN}.jsonl")),
        &Cursor::START,
        u64::MAX,
    );
    let status = |id: &str| {
        records
            .iter()
            .find_map(|r| match &r.value {
                Record::ToolFinished(f) if &*f.call.0 == id => Some(f.status),
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(status("toolu_fixture_0002"), ToolStatus::Completed);
    assert_eq!(status("toolu_fixture_0003"), ToolStatus::Failed);
    assert_eq!(status("toolu_fixture_0006"), ToolStatus::Interrupted);
    assert_eq!(status("toolu_fixture_0008"), ToolStatus::Denied);
}

#[test]
fn inherited_fork_prefix_is_marked_and_left_out_of_turns() {
    let adapter = adapter();
    let fork = fixture(&format!("{FORK}.jsonl"));
    let (records, _, _) = read_to_end(&adapter, &fork, &Cursor::START, u64::MAX);
    let bytes = std::fs::read(&fork.path).unwrap();
    // 前四行是复制来的（主会话的 `sessionId`）。
    let fifth_line = bytes
        .iter()
        .enumerate()
        .filter(|(_, b)| **b == b'\n')
        .nth(3)
        .map(|(i, _)| i as u64 + 1)
        .unwrap();
    for r in &records {
        assert_eq!(r.inherited, r.pos < fifth_line, "{r:?}");
    }
    let inherited_turn = records.iter().find_map(|r| match &r.value {
        Record::TurnStarted { turn, .. } if r.inherited => Some(turn.clone()),
        _ => None,
    });
    assert!(
        inherited_turn.is_some(),
        "the copied prompt is still reported"
    );
    // 继承的用量引用继承的轮次，自己的引用自己的。
    for r in &records {
        if let Record::Usage(u) = &r.value {
            if r.inherited {
                assert_eq!(u.turn, inherited_turn);
            } else {
                assert_ne!(u.turn, inherited_turn);
                assert!(u.turn.is_some());
            }
        }
    }
    let index = adapter.turns(&fork).unwrap();
    assert_eq!(index.turns.len(), 1);
    assert_eq!(index.turns[0].ordinal, 1);
    assert_eq!(
        &*index.turns[0].prompt_preview,
        "Placeholder prompt in the fork: try another approach."
    );
    // 摘要的标题和开始时间取自己的部分。
    let (summary, _) = adapter.summarize(&fork).unwrap();
    assert_eq!(
        summary.title.as_deref(),
        Some("Placeholder prompt in the fork: try another approach.")
    );
    assert_eq!(summary.started_at, index.turns[0].started_at);
}

#[test]
fn inherited_subagent_records_with_its_own_agent_id_are_its_own() {
    let adapter = adapter();
    for rel in [SUB1, SUB2] {
        let file = fixture(rel);
        let (records, _, _) = read_to_end(&adapter, &file, &Cursor::START, u64::MAX);
        assert!(records.iter().all(|r| !r.inherited), "{rel}");
        assert_eq!(usages(&records).len(), 2);
        let index = adapter.turns(&file).unwrap();
        assert_eq!(index.turns.len(), 1);
        assert!(
            index.turns[0]
                .prompt_preview
                .starts_with("Placeholder task for agent")
        );
        assert_eq!(
            index.turns[0].items.len(),
            2,
            "the tool call and the report"
        );
    }
}

#[test]
fn inherited_records_from_another_session_or_agent() {
    let owner = Owner::Subagent {
        parent_session_id: MAIN.into(),
        agent_id: "a0fa0009".into(),
    };
    let line = |session: &str, agent: Option<&str>, sec: u32, text: &str| {
        let agent = agent
            .map(|a| format!(r#","agentId":"{a}""#))
            .unwrap_or_default();
        format!(
            r#"{{"type":"user","sessionId":"{session}"{agent},"cwd":"/Users/dev/fixture/repo","uuid":"cccccccc-0000-4000-8000-{sec:012}","timestamp":"2026-10-02T11:00:{sec:02}.000Z","message":{{"content":"{text}"}}}}"#
        )
    };
    let dir = TempDir::new();
    let file = temp_file(&dir, owner);
    let lines = [
        line(MAIN, Some("a0fa0001"), 1, "copied from another agent"),
        line(FORK, Some("a0fa0009"), 2, "copied from another session"),
        line(MAIN, None, 3, "no agent id"),
        line(MAIN, Some("a0fa0009"), 4, "the task"),
        line(MAIN, Some("a0fa0009"), 5, "a later prompt"),
    ];
    std::fs::write(&file.path, lines.join("\n") + "\n").unwrap();
    let (records, _, _) = read_to_end(&adapter(), &file, &Cursor::START, u64::MAX);
    let turns: Vec<(bool, String)> = records
        .iter()
        .filter_map(|r| match &r.value {
            Record::TurnStarted { turn, .. } => {
                Some((r.inherited, turn.0[turn.0.len() - 1..].to_owned()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        turns,
        [
            (true, "1".into()),
            (true, "2".into()),
            (true, "3".into()),
            (false, "4".into()),
            (false, "5".into())
        ]
    );
    let index = adapter().turns(&file).unwrap();
    let previews: Vec<&str> = index.turns.iter().map(|t| &*t.prompt_preview).collect();
    assert_eq!(previews, ["the task", "a later prompt"]);
    assert_eq!(index.turns[1].ordinal, 2);
}
