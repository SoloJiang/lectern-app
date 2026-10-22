//! 运行日志（`logs_*`，docs/design/agents.md「日志与诊断」）：经插件发现、读取、建索引、取正文一遍，
//! 跳过的行、副本写失败和第一次发布写出记录、带会话的 `session`；会话 JSONL 的内容、提示词、占位令牌和
//! 形似真实的令牌都不进日志。全部会话都是测试里生成的合成数据。

use lectern_logging::testing::{self, Capture};
use lectern_services::session::{Cursor, TurnItem};

use super::support::*;

/// 会话 JSONL 的内容（坏行、记录类型名、agent 的回复）。
const CANARY_LINE: &str = "canary-l1ne-5c0d7a";
/// 提示词。
const CANARY_PROMPT: &str = "canary-pr0mpt-91be42";
/// Analyze 交给命令行的那种占位令牌。
const CANARY_PLACEHOLDER: &str = "lectern-placeholder-token-3f8e1d";
/// 形似真实的令牌（合成，不是任何人的凭据）。
const CANARY_TOKEN: &str = "sk-ant-api03-Zq7XbR2mK9vT4wLp8NcY3hJd6FsA1gUe5oIkQ0rVxW-Bn2Ct7Mz4Ly9Pa";

const CLAUDE_ID: &str = "c1a0de00-1111-4111-8111-0000000000a1";
const CODEX_ID: &str = "c0de0000-2222-4222-8222-0000000000b2";

/// 带 `msg` 且含 `needle` 的记录（`cargo test` 在同一进程里共用捕获器）。
fn records(log: &Capture, msg: &str, needle: &str) -> Vec<String> {
    let msg = format!("\"msg\":\"{msg}\"");
    log.lines()
        .into_iter()
        .filter(|l| l.contains(&msg) && l.contains(needle))
        .collect()
}

/// `line` 里字段 `name` 的 JSON 原文。
fn field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let start = line.find(&format!("\"{name}\":"))? + name.len() + 3;
    let rest = &line[start..];
    let end = if let Some(text) = rest.strip_prefix('"') {
        text.find('"')? + 2
    } else {
        rest.find([',', '}'])?
    };
    Some(&rest[..end])
}

fn quoted(s: &str) -> String {
    format!("\"{s}\"")
}

/// 一个 Claude Code 会话：已知忽略的一行、一条坏行、一条不认识类型的记录，再是两轮；提示词和回复里有金丝雀。
/// 返回内容和坏行的偏移与长度。
fn claude_body(cwd: &std::path::Path) -> (String, u64, u64) {
    let mut body = String::from("{\"type\":\"summary\",\"summary\":\"placeholder\"}\n");
    let pos = body.len() as u64;
    let bad = format!("{{\"type\":\"user\",\"message\": {CANARY_LINE} not json");
    body.push_str(&bad);
    body.push('\n');
    body.push_str(&format!(
        "{{\"type\":\"{CANARY_LINE}\",\"timestamp\":\"2026-10-09T09:00:00.000Z\",\"note\":\"{CANARY_LINE}\"}}\n"
    ));
    body.push_str(&claude_session(
        CLAUDE_ID,
        cwd,
        &[
            &format!("{CANARY_PROMPT} use {CANARY_PLACEHOLDER}"),
            &format!("export ANTHROPIC_API_KEY={CANARY_TOKEN}"),
        ],
        0,
    ));
    (body, pos, bad.len() as u64)
}

/// 一个 Codex rollout（之后压缩）：提示词、工具参数和输出里有金丝雀，中间一条坏行。
fn codex_body(cwd: &std::path::Path) -> String {
    let cwd = cwd.display();
    let ts = |n: u32| format!("2026-10-09T10:00:{n:02}.000Z");
    let lines = [
        format!(
            r#"{{"timestamp":"{}","type":"session_meta","payload":{{"id":"{CODEX_ID}","session_id":"{CODEX_ID}","timestamp":"{}","cwd":"{cwd}","originator":"codex_cli_rs","cli_version":"0.140.0","source":"cli","model_provider":"fixture-provider","history_mode":"legacy","git":{{"branch":"main"}}}}}}"#,
            ts(0),
            ts(0)
        ),
        format!(
            r#"{{"timestamp":"{}","type":"turn_context","payload":{{"turn_id":"turn-1","cwd":"{cwd}","model":"fixture-model-x"}}}}"#,
            ts(1)
        ),
        format!(
            r#"{{"timestamp":"{}","type":"event_msg","payload":{{"type":"task_started","turn_id":"turn-1"}}}}"#,
            ts(2)
        ),
        format!(
            r#"{{"timestamp":"{}","type":"event_msg","payload":{{"type":"user_message","message":"{CANARY_PROMPT} with {CANARY_PLACEHOLDER}","images":[]}}}}"#,
            ts(3)
        ),
        format!("{{\"timestamp\":\"{}\",\"type\":{CANARY_LINE}", ts(4)),
        format!(
            r#"{{"timestamp":"{}","type":"response_item","payload":{{"type":"function_call","name":"shell_command","arguments":"{{\"command\":\"curl -H 'x-api-key: {CANARY_TOKEN}' {CANARY_LINE}\"}}","call_id":"call_1"}}}}"#,
            ts(5)
        ),
        format!(
            r#"{{"timestamp":"{}","type":"response_item","payload":{{"type":"function_call_output","call_id":"call_1","output":"{CANARY_LINE} {CANARY_TOKEN}"}}}}"#,
            ts(6)
        ),
        format!(
            r#"{{"timestamp":"{}","type":"event_msg","payload":{{"type":"task_complete","turn_id":"turn-1","last_agent_message":"{CANARY_LINE}"}}}}"#,
            ts(7)
        ),
    ];
    let mut body = lines.join("\n");
    body.push('\n');
    body
}

#[test]
fn logs_hold_no_session_contents() {
    testing::set_callsite_limit(u32::MAX);
    let log = testing::capture();
    let mut h = Harness::new();
    let cwd = h.base.join("work");
    std::fs::create_dir_all(&cwd).unwrap();

    let (claude, bad_pos, bad_len) = claude_body(&cwd);
    let claude_path = h.claude_root().join(format!("-work/{CLAUDE_ID}.jsonl"));
    std::fs::create_dir_all(claude_path.parent().unwrap()).unwrap();
    std::fs::write(&claude_path, claude).unwrap();

    let dir = h.codex_root().join("2026/10/09");
    std::fs::create_dir_all(&dir).unwrap();
    let plain = dir.join(format!("rollout-2026-10-09T10-00-00-{CODEX_ID}.jsonl"));
    std::fs::write(&plain, codex_body(&cwd)).unwrap();
    compress(&plain);
    std::fs::remove_file(&plain).unwrap();
    // 解压副本的目录是一个普通文件：副本写不出来（以 root 运行时同样如此）。
    std::fs::write(h.base.join("decoded"), b"").unwrap();

    h.start();
    for (source, id) in [(claude_source(), CLAUDE_ID), (codex_source(), CODEX_ID)] {
        let key = key(&source, id);
        assert!(h.list(&source).get(&key).is_some(), "{id} is listed");
        let source = h.source(&source);
        let batch = source.read(&key, &Cursor::START, u64::MAX).unwrap();
        assert!(!batch.records.is_empty());
        let index = source.turns(&key).unwrap();
        for turn in index.turns.iter() {
            for r in turn
                .prompt
                .iter()
                .copied()
                .chain(turn.items.iter().flat_map(|i| match i {
                    TurnItem::AgentText(r) | TurnItem::Injected(r) => vec![*r],
                    TurnItem::Tool(c) => c.result.iter().copied().chain([c.input]).collect(),
                }))
            {
                source.load(&key, &r).unwrap();
            }
        }
    }
    h.settle();

    // Claude Code：摘要与读取各记一条，指向第一条坏行。
    let claude_session = quoted(CLAUDE_ID);
    let skipped = records(log, "session lines skipped", &claude_session);
    let stages: Vec<&str> = skipped.iter().filter_map(|l| field(l, "stage")).collect();
    assert!(stages.contains(&"\"summary\""), "{skipped:#?}");
    assert!(stages.contains(&"\"read\""), "{skipped:#?}");
    for line in &skipped {
        assert_eq!(field(line, "lv"), Some("\"warn\""), "{line}");
        assert_eq!(field(line, "tg"), Some("\"agents\""), "{line}");
        assert_eq!(field(line, "session"), Some(claude_session.as_str()));
        assert_eq!(field(line, "pos"), Some(bad_pos.to_string().as_str()));
        assert_eq!(field(line, "line_len"), Some(bad_len.to_string().as_str()));
        assert_eq!(field(line, "error"), Some("\"syntax\""), "{line}");
        assert_eq!(field(line, "bad_lines"), Some("1"), "{line}");
        assert!(
            field(line, "path")
                .unwrap()
                .ends_with(&format!("{CLAUDE_ID}.jsonl\""))
        );
    }
    let read = skipped
        .iter()
        .find(|l| field(l, "stage") == Some("\"read\""))
        .unwrap();
    assert_eq!(field(read, "unknown_types"), Some("1"), "{read}");
    assert_eq!(field(read, "from"), Some("0"));

    // Codex：压缩会话的副本写不出来，退回从头解码；坏行同样记下。
    let codex_session = quoted(CODEX_ID);
    let fallback = records(
        log,
        "decoded copy not written; decoding from the start",
        &codex_session,
    );
    assert!(!fallback.is_empty(), "{:#?}", log.lines());
    for line in &fallback {
        assert_eq!(field(line, "lv"), Some("\"warn\""));
        assert_eq!(field(line, "step"), Some("\"create_dir\""), "{line}");
        assert!(field(line, "io").is_some(), "{line}");
        assert!(
            field(line, "path").unwrap().ends_with(".jsonl.zst\""),
            "{line}"
        );
    }
    let codex_skipped = records(log, "session lines skipped", &codex_session);
    assert!(
        codex_skipped
            .iter()
            .any(|l| field(l, "stage") == Some("\"read\"")
                && field(l, "error") == Some("\"syntax\"")),
        "{codex_skipped:#?}"
    );

    // 第一次发布：每个来源一条 info（`cargo test` 里别的测试也写这条，按会话数找这一次的）。
    for agent in ["claude_code", "codex"] {
        let discovered = records(
            log,
            "sessions discovered",
            &format!("\"agent\":\"{agent}\""),
        );
        assert!(
            discovered.iter().any(|l| field(l, "lv") == Some("\"info\"")
                && field(l, "sessions") == Some("1")
                && field(l, "took_us").is_some_and(|t| t != "null")),
            "{agent}: {discovered:#?}"
        );
    }

    for canary in [CANARY_LINE, CANARY_PROMPT, CANARY_PLACEHOLDER, CANARY_TOKEN] {
        log.assert_absent(canary);
    }
    // 令牌的前缀也不出现（不只是整串）。
    log.assert_absent("sk-ant-api03");
}

#[test]
fn logs_clean_sessions_write_no_warnings() {
    testing::set_callsite_limit(u32::MAX);
    let log = testing::capture();
    let mut h = Harness::new();
    let cwd = h.base.join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let id = uuid(41);
    let path = h.claude_root().join(format!("-work/{id}.jsonl"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, claude_session(&id, &cwd, &["first", "second"], 0)).unwrap();
    h.start();
    let source = h.source(&claude_source());
    source.turns(&key(&claude_source(), &id)).unwrap();
    h.settle();
    let session = quoted(&id);
    let warns: Vec<String> = log
        .lines()
        .into_iter()
        .filter(|l| l.contains(&session) && field(l, "lv") == Some("\"warn\""))
        .collect();
    assert!(warns.is_empty(), "{warns:#?}");
}
