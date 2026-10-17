//! 记录的内容：工具分类与状态、交互式 shell、用量的两路、额度、继承前缀
//! （`tool_kind_*`、`codex_usage_stream_*`、`usage_keys_*`、`inherited_*`）。

use lectern_services::session::{
    RateLimitSample, RateWindow, RawTokens, Timestamp, ToolFinished, ToolKind, ToolStarted,
    ToolStatus, TurnId, UsageKey, UsageRecord,
};

use super::*;

fn all(name: &str) -> Vec<Positioned<Record>> {
    read_to_end(&adapter(), &fixture(name), &Cursor::START, u64::MAX).0
}

fn usages(records: &[Positioned<Record>]) -> Vec<(&UsageRecord, bool)> {
    records
        .iter()
        .filter_map(|r| match &r.value {
            Record::Usage(u) => Some((u, r.inherited)),
            _ => None,
        })
        .collect()
}

fn started<'a>(records: &'a [Positioned<Record>], call: &str) -> &'a ToolStarted {
    records
        .iter()
        .find_map(|r| match &r.value {
            Record::ToolStarted(t) if &*t.call.0 == call => Some(t),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no ToolStarted {call}"))
}

fn finished<'a>(records: &'a [Positioned<Record>], call: &str) -> Option<&'a ToolFinished> {
    records.iter().find_map(|r| match &r.value {
        Record::ToolFinished(f) if &*f.call.0 == call => Some(f),
        _ => None,
    })
}

fn paths(p: &[&str]) -> Arc<[PathBuf]> {
    p.iter().map(PathBuf::from).collect()
}

const REPO: &str = "/Users/dev/fixture/repo";

#[test]
fn tool_kind_codex_execute_tools() {
    let records = all("legacy_main");
    // `exec_command`：`cmd`，相对 `workdir` 按 cwd 拼接。
    assert_eq!(
        started(&records, "call_l1_0001").kind,
        ToolKind::Execute {
            command: "ls -la".into(),
            workdir: Path::new("/Users/dev/fixture/repo/src").into(),
            exec_session: None,
            workdir_exact: true,
        }
    );
    // `shell`：`["bash", "-lc", 脚本]` 取脚本。
    assert_eq!(
        started(&records, "call_l1_0004").kind,
        ToolKind::Execute {
            command: "cargo test --quiet".into(),
            workdir: Path::new(REPO).into(),
            exec_session: None,
            workdir_exact: true,
        }
    );
    // `local_shell_call`：没有工作目录时是调用时的 cwd（第二轮在 `crates/core`）。
    assert_eq!(
        started(&records, "call_l1_0007").kind,
        ToolKind::Execute {
            command: "git status".into(),
            workdir: Path::new(REPO).into(),
            exec_session: None,
            workdir_exact: true,
        }
    );
    assert_eq!(
        started(&records, "call_l1_0008").kind,
        ToolKind::Execute {
            command: "make build".into(),
            workdir: Path::new("/Users/dev/fixture/repo/crates/core").into(),
            exec_session: None,
            workdir_exact: true,
        }
    );
    // `shell_command`（分叉会话里）：`command` 是字符串。
    let fork = all("legacy_fork");
    assert_eq!(
        started(&fork, "call_l2_0001").kind,
        ToolKind::Execute {
            command: "git switch -c fork-branch".into(),
            workdir: Path::new(REPO).into(),
            exec_session: None,
            workdir_exact: true,
        }
    );
}

#[test]
fn tool_kind_codex_interactive_shell_sessions() {
    let records = all("legacy_main");
    // 打开会话的 `exec_command`：结束事件上带会话 ID。
    assert_eq!(
        finished(&records, "call_l1_0002")
            .unwrap()
            .exec_session
            .as_deref(),
        Some("7")
    );
    // 之后的 `write_stdin`：同一个会话 ID，工作目录是会话启动时的目录，不精确。
    assert_eq!(
        started(&records, "call_l1_0003").kind,
        ToolKind::Execute {
            command: "import os\n".into(),
            workdir: Path::new("/Users/dev/fixture/repo/tools").into(),
            exec_session: Some("7".into()),
            workdir_exact: false,
        }
    );
    // 已经退出的命令、其余工具的结束事件不带会话 ID。
    for call in ["call_l1_0001", "call_l1_0003", "call_l1_0004"] {
        assert_eq!(
            finished(&records, call).unwrap().exec_session,
            None,
            "{call}"
        );
    }
}

#[test]
fn tool_kind_codex_edit_read_spawn_and_other() {
    let records = all("legacy_main");
    let core = "/Users/dev/fixture/repo/crates/core";
    assert_eq!(
        started(&records, "call_l1_0005").kind,
        ToolKind::Edit {
            paths: paths(&[
                &format!("{core}/notes.txt"),
                &format!("{core}/src/lib.rs"),
                &format!("{core}/old.txt"),
                &format!("{core}/src/a.rs"),
                &format!("{core}/src/b.rs"),
            ])
        }
    );
    assert_eq!(
        started(&records, "call_l1_0006").kind,
        ToolKind::Edit {
            paths: paths(&["/Users/dev/fixture/repo/README.md"])
        }
    );
    assert_eq!(
        started(&records, "call_l1_0009").kind,
        ToolKind::Read {
            paths: paths(&[&format!("{core}/docs/diagram.png")])
        }
    );
    assert_eq!(started(&records, "call_l1_0010").kind, ToolKind::SpawnAgent);
    assert_eq!(started(&records, "call_l1_0011").kind, ToolKind::Other);
    let parsed = adapter()
        .read(&fixture("legacy_main"), &Cursor::START, u64::MAX)
        .unwrap();
    assert_eq!(parsed.stats.unknown_tools, [("fixture__lookup".into(), 1)]);
}

#[test]
fn tool_kind_codex_results_map_to_statuses() {
    let records = all("legacy_main");
    let status = |call: &str| finished(&records, call).map(|f| f.status);
    // 退出码 0；`Process running`（没有退出码）；非 0 的退出码在旧版本 JSON 输出的元数据里。
    assert_eq!(status("call_l1_0001"), Some(ToolStatus::Completed));
    assert_eq!(status("call_l1_0002"), Some(ToolStatus::Completed));
    assert_eq!(status("call_l1_0004"), Some(ToolStatus::Failed));
    // 补丁：`patch_apply_end` 的状态先于输出到达。
    assert_eq!(status("call_l1_0005"), Some(ToolStatus::Completed));
    assert_eq!(status("call_l1_0006"), Some(ToolStatus::Failed));
    // `local_shell_call`：`in_progress` 只开始，`completed` 结束，`incomplete` 是中断。
    assert_eq!(status("call_l1_0007"), Some(ToolStatus::Completed));
    assert_eq!(
        records
            .iter()
            .filter(|r| matches!(&r.value, Record::ToolStarted(t) if &*t.call.0 == "call_l1_0007"))
            .count(),
        1
    );
    assert_eq!(status("call_l1_0008"), Some(ToolStatus::Interrupted));
    // 没有退出码时 `success: false`。
    assert_eq!(status("call_l1_0011"), Some(ToolStatus::Failed));
    // paginated：内容项数组里的 `Process exited with code 1`。
    let records = all("paginated_main");
    assert_eq!(
        finished(&records, "call_p1_0001").map(|f| f.status),
        Some(ToolStatus::Failed)
    );
}

#[test]
fn tool_kind_codex_local_shell_without_completion_only_starts() {
    let dir = TempDir::new();
    let file = write_lines(
        &dir,
        L1,
        &[
            meta_line(L1, ""),
            event(1, r#"{"type":"task_started","turn_id":"t1"}"#),
            item(
                2,
                r#"{"type":"local_shell_call","call_id":"s1","status":"in_progress","action":{"type":"exec","command":["sleep","5"]}}"#,
            ),
        ],
    );
    let (records, _, _) = read_to_end(&adapter(), &file, &Cursor::START, u64::MAX);
    assert!(
        records
            .iter()
            .any(|r| matches!(&r.value, Record::ToolStarted(_)))
    );
    assert!(finished(&records, "s1").is_none());
    let index = adapter().turns(&file).unwrap();
    assert!(matches!(
        &index.turns[0].items[0],
        lectern_services::session::TurnItem::Tool(t) if t.finished.is_none()
    ));
}

fn raw(i: u64, c: u64, w: u64, o: u64, r: u64) -> RawTokens {
    RawTokens::OpenAi {
        input: i,
        cached_input: c,
        cache_write: w,
        output: o,
        reasoning_output: r,
    }
}

fn sum(tokens: impl Iterator<Item = RawTokens>) -> RawTokens {
    tokens.fold(raw(0, 0, 0, 0, 0), |acc, t| match (acc, t) {
        (
            RawTokens::OpenAi {
                input,
                cached_input,
                cache_write,
                output,
                reasoning_output,
            },
            RawTokens::OpenAi {
                input: i,
                cached_input: c,
                cache_write: w,
                output: o,
                reasoning_output: r,
            },
        ) => raw(
            input + i,
            cached_input + c,
            cache_write + w,
            output + o,
            reasoning_output + r,
        ),
        _ => unreachable!("Codex usage"),
    })
}

/// `usage` 计入的用量：全部 `CodexResponse`，加上 `superseded` 为 false 的 `CodexTokenCount`，都只取自己的。
fn counted(records: &[Positioned<Record>]) -> RawTokens {
    sum(usages(records)
        .into_iter()
        .filter(|(u, inherited)| {
            !inherited
                && match u.key {
                    UsageKey::CodexResponse { .. } => true,
                    UsageKey::CodexTokenCount { .. } => !u.superseded,
                    UsageKey::Claude { .. } => false,
                }
        })
        .map(|(u, _)| u.tokens))
}

#[test]
fn codex_usage_stream_switches_at_the_first_response_record() {
    let records = all("legacy_main");
    let usage = usages(&records);
    let kinds: Vec<(&str, bool)> = usage
        .iter()
        .map(|(u, _)| match u.key {
            UsageKey::CodexTokenCount { .. } => ("count", u.superseded),
            UsageKey::CodexResponse { .. } => ("response", u.superseded),
            UsageKey::Claude { .. } => unreachable!(),
        })
        .collect();
    assert_eq!(
        kinds,
        [
            ("count", false),
            ("count", false),
            ("response", false),
            ("count", true),
            ("response", false),
            ("count", true),
        ],
        "token_count before the first token_usage_record is not superseded, after it is"
    );
    // 旧版本开始、升级后续接：切换前后的合计等于最后的累计值（逐响应的真实合计）。
    let last_total = usage
        .iter()
        .rev()
        .find_map(|(u, _)| match &u.key {
            UsageKey::CodexTokenCount { total, .. } => Some(*total),
            _ => None,
        })
        .unwrap();
    assert_eq!(counted(&records), last_total);
    assert_eq!(last_total, raw(2500, 1000, 10, 250, 65));
    // 模型取最近一条 `turn_context`。
    assert!(
        usage[..2]
            .iter()
            .all(|(u, _)| u.model.as_deref() == Some("fixture-model-x"))
    );
    assert!(
        usage[2..]
            .iter()
            .all(|(u, _)| u.model.as_deref() == Some("fixture-model-y"))
    );
    // `token_usage_record` 的轮次取它自己的 `turn_id`。
    assert_eq!(usage[2].0.turn, Some(TurnId("turn-l1-0003".into())));
}

#[test]
fn codex_usage_stream_threads_without_response_records_are_never_superseded() {
    for name in ["legacy_fork", "legacy_fork_of_fork"] {
        let records = all(name);
        assert!(
            usages(&records).iter().all(|(u, _)| !u.superseded),
            "{name}"
        );
    }
}

#[test]
fn codex_usage_stream_follows_only_this_threads_response_records() {
    // 子 agent 重放的父线程 `token_usage_record` 不让自己的 `token_count` 被取代。
    let records = all("paginated_subagent");
    let usage = usages(&records);
    let shape: Vec<(bool, bool, &str)> = usage
        .iter()
        .map(|(u, inherited)| match &u.key {
            UsageKey::CodexTokenCount { thread_id, .. } => (*inherited, u.superseded, &**thread_id),
            UsageKey::CodexResponse { thread_id, .. } => (*inherited, u.superseded, &**thread_id),
            UsageKey::Claude { .. } => unreachable!(),
        })
        .collect();
    assert_eq!(
        shape,
        [
            (true, false, P1),
            (true, false, P2),
            (false, false, P2),
            (false, true, P2),
        ]
    );
    assert_eq!(counted(&records), raw(300, 100, 0, 30, 10));
}

#[test]
fn usage_keys_codex_threads_and_totals() {
    let records = all("legacy_main");
    let usage = usages(&records);
    assert_eq!(
        usage[0].0.key,
        UsageKey::CodexTokenCount {
            thread_id: L1.into(),
            total: raw(1000, 200, 0, 100, 40),
        }
    );
    assert_eq!(
        usage[2].0.key,
        UsageKey::CodexResponse {
            thread_id: L1.into(),
            response_id: "resp_l1_0001".into(),
        }
    );
    // 两个线程写出相同的累计值时键不同（分叉里自己的部分带自己的线程 ID）。
    let fork = all("legacy_fork");
    let inherited: Vec<_> = usages(&fork)
        .into_iter()
        .filter(|(_, i)| *i)
        .map(|(u, _)| u.key.clone())
        .collect();
    assert_eq!(
        inherited,
        [UsageKey::CodexTokenCount {
            thread_id: L2.into(),
            total: raw(1000, 200, 0, 100, 40),
        }]
    );
    assert_ne!(inherited[0], usage[0].0.key);
    // 同一线程重复写出的同一个累计值：键相同。
    let dir = TempDir::new();
    let count = r#"{"type":"token_count","info":{"total_token_usage":{"input_tokens":5,"cached_input_tokens":0,"output_tokens":1,"reasoning_output_tokens":0,"total_tokens":6},"last_token_usage":{"input_tokens":5,"cached_input_tokens":0,"output_tokens":1,"reasoning_output_tokens":0,"total_tokens":6}},"rate_limits":null}"#;
    let file = write_lines(
        &dir,
        L1,
        &[meta_line(L1, ""), event(1, count), event(2, count)],
    );
    let (records, _, _) = read_to_end(&adapter(), &file, &Cursor::START, u64::MAX);
    let keys: Vec<_> = usages(&records)
        .iter()
        .map(|(u, _)| u.key.clone())
        .collect();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0], keys[1]);
}

#[test]
fn usage_keys_codex_rate_limits_without_usage() {
    let records = all("legacy_main");
    let samples: Vec<(u64, &RateLimitSample)> = records
        .iter()
        .filter_map(|r| match &r.value {
            Record::RateLimits(s) => Some((r.pos, s)),
            _ => None,
        })
        .collect();
    assert_eq!(samples.len(), 2);
    // `info` 为 null 的 `token_count` 只给出额度。
    let only_limits = samples[1].0;
    assert!(
        records
            .iter()
            .filter(|r| r.pos == only_limits)
            .all(|r| matches!(r.value, Record::RateLimits(_) | Record::Context { .. }))
    );
    // 旧版本的 `resets_in_seconds` 按记录时间换算。
    let s = samples[1].1;
    assert_eq!(
        s.primary,
        Some(RateWindow {
            used_percent: 12.5,
            window_minutes: Some(300),
            resets_at: Some(Timestamp(s.at.0 + 3_600_000)),
        })
    );
    // 新版本的 `resets_at`（Unix 秒）、`limit_id`、`plan_type`。
    let records = all("paginated_main");
    let s = records
        .iter()
        .find_map(|r| match &r.value {
            Record::RateLimits(s) => Some(s.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(s.limit_id.as_deref(), Some("codex"));
    assert_eq!(s.plan_type.as_deref(), Some("plus"));
    assert_eq!(
        s.secondary,
        Some(RateWindow {
            used_percent: 61.5,
            window_minutes: Some(10080),
            resets_at: Some(Timestamp(1_791_802_800_000)),
        })
    );
}

/// 每条记录的 `inherited` 与它所在行的 `turn_id` 是否在 `inherited_turns` 里一致。
fn assert_inherited_by_turn(name: &str, inherited_turns: &[&str]) {
    let records = all(name);
    // 一行里 `TurnStarted` 之前可能先有一条 `Context`：按行记下轮次。
    let starts: HashMap<u64, String> = records
        .iter()
        .filter_map(|r| match &r.value {
            Record::TurnStarted { turn, .. } => Some((r.pos, turn.0.to_string())),
            _ => None,
        })
        .collect();
    let mut turn: Option<String> = None;
    for r in &records {
        if let Some(t) = starts.get(&r.pos) {
            turn = Some(t.clone());
        }
        let in_prefix = turn
            .as_deref()
            .is_some_and(|t| inherited_turns.contains(&t));
        assert_eq!(r.inherited, in_prefix, "{name}: {r:?}");
    }
    let index = adapter().turns(&fixture(name)).unwrap();
    assert!(
        index
            .turns
            .iter()
            .all(|t| !inherited_turns.contains(&&*t.id.0)),
        "{name}: the turn index has no inherited turns"
    );
    assert_eq!(index.turns[0].ordinal, 1);
}

#[test]
fn inherited_codex_legacy_fork_by_source_turn_ids() {
    assert_inherited_by_turn("legacy_fork", &["turn-l1-0001"]);
    let index = adapter().turns(&fixture("legacy_fork")).unwrap();
    assert_eq!(index.turns.len(), 1);
    assert_eq!(
        &*index.turns[0].prompt_preview,
        "Placeholder prompt in the fork: try another approach."
    );
    // 继承的记录引用继承的轮次，自己的引用自己的。
    for r in all("legacy_fork") {
        if let Record::Usage(u) = &r.value {
            let expected = if r.inherited {
                "turn-l1-0001"
            } else {
                "turn-l2-0001"
            };
            assert_eq!(u.turn.as_ref().map(|t| &*t.0), Some(expected));
        }
    }
}

#[test]
fn inherited_codex_fork_of_a_fork_uses_all_source_turns() {
    // 来源（第一级分叉）文件里也有它自己继承的 `turn-l1-0001`：集合取文件里的全部轮次，不是它的轮次索引。
    assert_inherited_by_turn("legacy_fork_of_fork", &["turn-l1-0001", "turn-l2-0001"]);
    let index = adapter().turns(&fixture("legacy_fork_of_fork")).unwrap();
    assert_eq!(index.turns.len(), 1);
    assert_eq!(&*index.turns[0].id.0, "turn-l3-0001");
}

#[test]
fn inherited_codex_subagent_by_ordinal() {
    let file = fixture("paginated_subagent");
    let bytes = std::fs::read(&file.path).unwrap();
    let records = all("paginated_subagent");
    // `ordinal` 小于 6 的行（第 2 到第 6 行）是继承前缀。
    let starts: Vec<u64> = std::iter::once(0)
        .chain(
            bytes
                .iter()
                .enumerate()
                .filter(|(_, b)| **b == b'\n')
                .map(|(i, _)| i as u64 + 1),
        )
        .collect();
    for r in &records {
        let line = starts.iter().position(|s| *s == r.pos).unwrap();
        assert_eq!(r.inherited, line < 6, "{r:?}");
    }
    let index = adapter().turns(&file).unwrap();
    assert_eq!(index.turns.len(), 1);
    assert_eq!(
        &*index.turns[0].prompt_preview,
        "Placeholder subagent task."
    );
    // 继承前缀之后补一条不标 `inherited` 的 `Context`。
    let first_own = records.iter().find(|r| !r.inherited).unwrap();
    assert!(matches!(first_own.value, Record::Context { .. }));
}

#[test]
fn inherited_codex_fork_read_before_its_source_gives_the_same_result() {
    // 来源会话从来没有被读过：`read` 在同一次调用里扫描来源，结果与先读来源时相同。
    let expected = all("legacy_fork_of_fork");
    let fresh = adapter();
    let (records, _, _) = read_to_end(&fresh, &fixture("legacy_fork_of_fork"), &Cursor::START, 300);
    assert_eq!(records, expected);
}

#[test]
fn inherited_codex_unknown_source_then_reset_when_it_appears() {
    let lineage = Arc::new(MapLineage::default());
    let codex =
        Codex::without_decoded_copies(SourceId(SOURCE_ID.into())).with_lineage(lineage.clone());
    let fork = fixture("legacy_fork");
    // 来源不在本机：全部按自己的计，摘要标 `inheritance_unknown`。
    let (records, cursor, batches) = read_to_end(&codex, &fork, &Cursor::START, u64::MAX);
    assert!(records.iter().all(|r| !r.inherited));
    assert!(!batches[0].batch.reset);
    assert!(codex.summarize(&fork).unwrap().0.inheritance_unknown);
    assert!(!adapter().summarize(&fork).unwrap().0.inheritance_unknown);
    let index = codex.turns(&fork).unwrap();
    assert_eq!(index.turns.len(), 2, "both turns count as its own");
    // 没有变化时继续读不重置。
    let again = codex.read(&fork, &cursor, u64::MAX).unwrap();
    assert!(!again.batch.reset && again.batch.records.is_empty());

    // 来源出现之后：下一次 `read` 从头回放，按边界标记。
    lineage.insert(L1, fixture("legacy_main"));
    let (replayed, _, batches) = read_to_end(&codex, &fork, &cursor, u64::MAX);
    assert!(batches[0].batch.reset);
    assert_eq!(replayed, all("legacy_fork"));
    assert!(!codex.summarize(&fork).unwrap().0.inheritance_unknown);
}

#[test]
fn inherited_codex_source_vanishing_mid_prefix_replays() {
    // 继承前缀分批读到一半，来源消失：从头读，按边界未定处理。
    let lineage = Arc::new(MapLineage::default());
    lineage.insert(L1, fixture("legacy_main"));
    let codex =
        Codex::without_decoded_copies(SourceId(SOURCE_ID.into())).with_lineage(lineage.clone());
    let fork = fixture("legacy_fork");
    let first = codex.read(&fork, &Cursor::START, 1).unwrap().batch;
    assert!(!first.at_end);
    *lineage.0.lock().unwrap() = HashMap::new();
    let (records, _, batches) = read_to_end(&codex, &fork, &first.cursor, u64::MAX);
    assert!(batches[0].batch.reset);
    assert!(records.iter().all(|r| !r.inherited));
}
