//! 摘要、末尾字段、恢复命令与文件名（`resume_*`、`tail_*`、`codex_summary_*`、`codex_session_file_*`）。

use lectern_services::session::{Format, ResumeCommand, SessionError, Timestamp};

use super::*;
use crate::codex::Activity;

fn ts(s: &str) -> Timestamp {
    crate::time::parse(s).unwrap()
}

#[test]
fn codex_session_file_names() {
    let codex = adapter();
    let file = codex
        .session_file(Path::new(&format!(
            "/Users/dev/fixture/.codex/sessions/2026/10/02/rollout-2026-10-02T09-00-00-{L1}.jsonl"
        )))
        .unwrap();
    assert_eq!(&*file.key.id, L1);
    assert_eq!(file.key.source, *codex.source());
    assert!(!file.compressed && !file.archived);
    // 压缩、归档、带 rollout ID 的文件名。
    let file = codex
        .session_file(Path::new(&format!(
            "/Users/dev/fixture/.codex/archived_sessions/rollout-2026-10-02T09-00-00-{L1}_{L2}.jsonl.zst"
        )))
        .unwrap();
    assert_eq!(&*file.key.id, L1);
    assert!(file.compressed && file.archived);
    for not_a_rollout in [
        "x/rollout-2026-10-02T09-00-00-not-a-uuid.jsonl".to_owned(),
        format!("x/session-2026-10-02T09-00-00-{L1}.jsonl"),
        format!("x/rollout-{L1}.jsonl"),
        format!("x/rollout-2026-10-02T09-00-00-{L1}.json"),
        format!("archived_sessions/rollout-2026-10-02T09-00-00-{L1}.txt"),
    ] {
        assert_eq!(
            codex.session_file(Path::new(&not_a_rollout)),
            None,
            "{not_a_rollout}"
        );
    }
}

#[test]
fn resume_codex_main_sessions_and_forks_only() {
    let codex = adapter();
    for (name, id) in [
        ("legacy_main", L1),
        ("legacy_fork", L2),
        ("paginated_main", P1),
    ] {
        let (summary, _) = codex.summarize(&fixture(name)).unwrap();
        assert_eq!(
            codex.resume(&summary),
            Some(ResumeCommand::Codex {
                session_id: id.into()
            }),
            "{name}"
        );
    }
    let (summary, _) = codex.summarize(&fixture("paginated_subagent")).unwrap();
    assert_eq!(codex.resume(&summary), None, "subagent sessions");
    let (mut summary, _) = codex.summarize(&fixture("legacy_main")).unwrap();
    summary.format = Format::Unrecognized;
    assert_eq!(codex.resume(&summary), None, "unrecognized sessions");
    summary.format = Format::Known;
    let other = Codex::without_decoded_copies(SourceId("acp-codex".into()));
    assert_eq!(other.resume(&summary), None, "another source's session");
}

#[test]
fn codex_summary_fields() {
    let codex = adapter();
    let (main, _) = codex.summarize(&fixture("legacy_main")).unwrap();
    assert_eq!(
        main.title.as_deref(),
        Some("Placeholder prompt one: list the files.")
    );
    assert_eq!(main.agent_version.as_deref(), Some("0.140.0"));
    assert_eq!(main.logged_branch.as_deref(), Some("main"));
    assert_eq!(main.started_at, ts("2026-10-02T09:00:00.000Z"));
    assert_eq!(&*main.cwd, Path::new("/Users/dev/fixture/repo"));
    assert_eq!(main.parent, None);
    // 分叉：标题取自己的第一个 prompt。
    let (fork, _) = codex.summarize(&fixture("legacy_fork_of_fork")).unwrap();
    assert_eq!(
        fork.title.as_deref(),
        Some("Placeholder prompt in the fork of the fork.")
    );
    assert_eq!(fork.logged_branch.as_deref(), Some("fork-branch"));
    // 子 agent：父会话是 `thread_spawn.parent_thread_id`，派出它的调用不在它的 rollout 里。
    let (sub, _) = codex.summarize(&fixture("paginated_subagent")).unwrap();
    let parent = sub.parent.unwrap();
    assert_eq!(parent.session, fixture("paginated_main").key);
    assert_eq!(parent.tool_call, None);
    assert_eq!(sub.title.as_deref(), Some("Placeholder subagent task."));
    // 归档：路径在 `archived_sessions/` 之下。
    let dir = TempDir::new();
    let archived = dir.path().join("archived_sessions");
    std::fs::create_dir_all(&archived).unwrap();
    let path = archived.join(rollout_name(L1, false));
    std::fs::copy(fixture("legacy_main").path, &path).unwrap();
    let (summary, _) = codex
        .summarize(&codex.session_file(&path).unwrap())
        .unwrap();
    assert!(summary.archived);
}

#[test]
fn tail_codex_matches_the_parser_state() {
    let codex = adapter();
    for (name, _) in fixture_files() {
        let file = fixture(name);
        let (summary, _) = codex.summarize(&file).unwrap();
        let cursor = codex
            .read(&file, &Cursor::START, u64::MAX)
            .unwrap()
            .batch
            .cursor;
        assert_eq!(
            codex.activity(&cursor),
            Some(Activity {
                last_activity: Some(summary.last_activity),
                open_turn: summary.open_turn
            }),
            "{name}"
        );
        assert!(!summary.open_turn, "{name} ends with a finished turn");
        assert!(!summary.tail_pending);
    }
    assert_eq!(codex.activity(&Cursor::START), None);
}

#[test]
fn tail_codex_open_turn_follows_the_last_turn_event() {
    let started = event(5, r#"{"type":"task_started","turn_id":"t1"}"#);
    let message = event(6, r#"{"type":"agent_message","message":"working"}"#);
    let complete = event(7, r#"{"type":"task_complete","turn_id":"t1"}"#);
    let aborted = event(
        7,
        r#"{"type":"turn_aborted","turn_id":"t1","reason":"interrupted"}"#,
    );
    let unknown = line("world_state", 8, r#"{"full":true,"state":{}}"#);
    let cases = [
        (vec![started.clone()], true, "2026-10-09T09:00:05.000Z"),
        (
            vec![started.clone(), message.clone()],
            true,
            "2026-10-09T09:00:06.000Z",
        ),
        (
            vec![started.clone(), complete],
            false,
            "2026-10-09T09:00:07.000Z",
        ),
        (
            vec![started.clone(), aborted, unknown],
            false,
            "2026-10-09T09:00:08.000Z",
        ),
    ];
    for (body, open, last) in cases {
        let dir = TempDir::new();
        let mut lines = vec![meta_line(L1, "")];
        lines.extend(body);
        let file = write_lines(&dir, L1, &lines);
        let (summary, _) = adapter().summarize(&file).unwrap();
        assert_eq!(summary.open_turn, open, "{lines:?}");
        assert_eq!(summary.last_activity, ts(last), "{lines:?}");
        let cursor = adapter()
            .read(&file, &Cursor::START, u64::MAX)
            .unwrap()
            .batch
            .cursor;
        assert_eq!(adapter().activity(&cursor).unwrap().open_turn, open);
    }
}

#[test]
fn codex_format_unknown_and_bad_lines() {
    let dir = TempDir::new();
    let file = write_lines(
        &dir,
        L1,
        &[
            // 会话头之前的记录无法归属。
            event(0, r#"{"type":"task_started","turn_id":"early"}"#),
            meta_line(L1, ""),
            event(1, r#"{"type":"task_started","turn_id":"t1"}"#),
            "{not json".to_owned(),
            r#"{"no_type":true}"#.to_owned(),
            // 没有时间戳、没有 `payload`、没有标记、缺少 `call_id`。
            r#"{"type":"event_msg","payload":{"type":"agent_message","message":"x"}}"#.to_owned(),
            r#"{"timestamp":"2026-10-09T09:00:02.000Z","type":"event_msg"}"#.to_owned(),
            event(2, r#"{"turn_id":"t1"}"#),
            item(
                3,
                r#"{"type":"function_call","name":"exec_command","arguments":"{}"}"#,
            ),
            // 不认识的类型；认识、不需要的类型。
            line("fixture_future_record", 4, "{}"),
            event(5, r#"{"type":"fixture_future_event"}"#),
            item(6, r#"{"type":"fixture_future_item"}"#),
            line("world_state", 7, "{}"),
            event(
                8,
                r#"{"type":"thread_settings_applied","thread_settings":{}}"#,
            ),
            item(9, r#"{"type":"reasoning","summary":[]}"#),
            String::new(),
            event(10, r#"{"type":"task_complete","turn_id":"t1"}"#),
        ],
    );
    let codex = adapter();
    let parsed = codex.read(&file, &Cursor::START, u64::MAX).unwrap();
    assert_eq!(parsed.stats.bad_lines, 7);
    assert_eq!(
        parsed.stats.unknown_types,
        [
            ("fixture_future_record".into(), 1),
            ("event_msg/fixture_future_event".into(), 1),
            ("response_item/fixture_future_item".into(), 1),
        ]
    );
    assert_eq!(parsed.stats.format(), Format::Partial);
    assert!(parsed.batch.at_end);
    let kinds: Vec<&str> = parsed
        .batch
        .records
        .iter()
        .map(|r| match r.value {
            Record::Context { .. } => "context",
            Record::TurnStarted { .. } => "started",
            Record::TurnEnded { .. } => "ended",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["context", "started", "ended"]);
    assert_eq!(codex.turns(&file).unwrap().format, Format::Partial);
}

#[test]
fn codex_format_unrecognized_files() {
    let codex = adapter();
    // 开头 256 KB 里没有 `session_meta`。
    let dir = TempDir::new();
    let filler = line("world_state", 1, r#"{"full":true,"state":{}}"#);
    let mut lines: Vec<String> =
        std::iter::repeat_n(filler.clone(), 300 * 1024 / filler.len()).collect();
    lines.push(meta_line(L1, ""));
    let file = write_lines(&dir, L1, &lines);
    let parsed = codex.read(&file, &Cursor::START, u64::MAX).unwrap().batch;
    assert!(parsed.records.is_empty() && parsed.at_end && !parsed.reset);
    assert_eq!(parsed.cursor, Cursor::START);
    assert_eq!(codex.turns(&file), Err(SessionError::Unrecognized));
    assert_eq!(
        codex.summarize(&file).unwrap_err(),
        SessionError::Unrecognized
    );

    // 有 cwd（`turn_context`）、没有会话头：照常列出，`Format::Unrecognized`。
    let dir = TempDir::new();
    let file = write_lines(
        &dir,
        L1,
        &[line(
            "turn_context",
            1,
            r#"{"cwd":"/Users/dev/fixture/elsewhere","model":"m"}"#,
        )],
    );
    let (summary, _) = codex.summarize(&file).unwrap();
    assert_eq!(summary.format, Format::Unrecognized);
    assert_eq!(&*summary.cwd, Path::new("/Users/dev/fixture/elsewhere"));
    assert_eq!(codex.turns(&file), Err(SessionError::Unrecognized));
    assert_eq!(codex.resume(&summary), None);

    // 空文件、文件不存在。
    let dir = TempDir::new();
    let file = write_lines(&dir, L1, &[]);
    assert_eq!(codex.turns(&file), Err(SessionError::Unrecognized));
    std::fs::remove_file(&file.path).unwrap();
    assert_eq!(
        codex.read(&file, &Cursor::START, 1),
        Err(SessionError::NotFound)
    );
    assert_eq!(codex.summarize(&file).unwrap_err(), SessionError::NotFound);
}
