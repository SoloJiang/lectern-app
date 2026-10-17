use std::path::{Path, PathBuf};
use std::sync::Arc;

use lectern_kernel::{Event, Service, service};

use super::*;
use crate::project::{Location, OpenError, PackageMatch, ProjectKey, Resolved, WorktreeKey};

/// A `SessionSource` with no sessions, to check the trait is usable as a
/// shared trait object.
struct Empty;

impl SessionSource for Empty {
    fn id(&self) -> SourceId {
        source()
    }
    fn agent(&self) -> Agent {
        Agent::ClaudeCode
    }
    fn sessions(&self) -> Arc<SessionList> {
        Arc::new(SessionList::default())
    }
    fn read(
        &self,
        _session: &SessionKey,
        from: &Cursor,
        _max_bytes: u64,
    ) -> Result<Batch, SessionError> {
        Ok(Batch {
            reset: false,
            records: Vec::new(),
            cursor: from.clone(),
            at_end: true,
        })
    }
    fn turns(&self, _session: &SessionKey) -> Result<Arc<TurnIndex>, SessionError> {
        Err(SessionError::NotFound)
    }
    fn load(&self, _session: &SessionKey, _at: &ContentRef) -> Result<Content, SessionError> {
        Err(SessionError::Unrecognized)
    }
    fn resume(&self, _session: &SessionKey) -> Option<ResumeCommand> {
        None
    }
    fn complete(&self, _sessions: &[SessionKey]) {}
}

fn source() -> SourceId {
    SourceId(Arc::from("session-claude-code"))
}

fn key(id: &str) -> SessionKey {
    SessionKey {
        source: source(),
        id: Arc::from(id),
    }
}

fn path(p: &str) -> Arc<Path> {
    Arc::from(PathBuf::from(p))
}

fn text(s: &str) -> Arc<str> {
    Arc::from(s)
}

fn shareable<T: Send + Sync + 'static>() {}

/// Serializes to JSON (the process plugin protocol is JSON-RPC, ADR 0018)
/// and back.
fn round_trip<T>(value: &T) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let json = serde_json::to_string(value).expect("serializes");
    serde_json::from_str(&json).expect("deserializes")
}

#[test]
fn session_source_service_id_and_multiple() {
    assert_eq!(<dyn SessionSource as Service>::ID, "session-source");
    const { assert!(<dyn SessionSource as Service>::MULTIPLE) };
    let key = service::<dyn SessionSource>();
    assert_eq!(key.id(), "session-source");
    assert!(key.multiple());
}

#[test]
fn session_source_is_a_shared_trait_object() {
    let sessions: Arc<dyn SessionSource> = Arc::new(Empty);
    let k = key("a");
    assert_eq!(sessions.id(), source());
    assert_eq!(sessions.agent(), Agent::ClaudeCode);
    assert!(sessions.sessions().sessions.is_empty());
    let batch = sessions.read(&k, &Cursor::START, 1).expect("reads");
    assert!(batch.at_end && batch.cursor.is_start());
    assert_eq!(sessions.turns(&k), Err(SessionError::NotFound));
    assert_eq!(
        sessions.load(&k, &ContentRef::new(0, 1, 0)),
        Err(SessionError::Unrecognized)
    );
    assert_eq!(sessions.resume(&k), None);
    sessions.complete(&[k]);
    // Lists, batches and indexes cross threads inside `Arc`s or by value.
    shareable::<Arc<dyn SessionSource>>();
    shareable::<SessionList>();
    shareable::<Batch>();
    shareable::<TurnIndex>();
    shareable::<Content>();
}

#[test]
fn session_events_round_trip() {
    fn event<E: Event + Clone>() {}
    event::<SessionsChanged>();

    let changed = SessionsChanged {
        source: source(),
        generation: 4,
        changed: Arc::from(vec![key("a"), key("b")]),
        removed: Arc::from(vec![key("c")]),
    };
    assert_eq!(round_trip(&changed), changed);
    let empty = SessionsChanged {
        changed: Arc::from(Vec::new()),
        removed: Arc::from(Vec::new()),
        ..changed
    };
    assert_eq!(round_trip(&empty), empty);
}

#[test]
fn session_errors_and_resume_commands_round_trip() {
    for error in [
        SessionError::NotFound,
        SessionError::Io(text("permission denied")),
        SessionError::Unrecognized,
        SessionError::TooLarge(5 << 20),
    ] {
        assert_eq!(round_trip(&error), error);
    }
    let id = text("0b9c4c2e-3f43-4c1e-9a43-6a1d2b7e8f90");
    for command in [
        ResumeCommand::ClaudeCode {
            session_id: id.clone(),
        },
        ResumeCommand::Codex { session_id: id },
    ] {
        assert_eq!(round_trip(&command), command);
    }
}

fn project() -> ProjectKey {
    ProjectKey::Git {
        common_dir: path("/r/.git"),
    }
}

fn summary(id: &str, place: Place, last_activity: i64) -> SessionSummary {
    SessionSummary {
        key: key(id),
        agent: Agent::ClaudeCode,
        parent: None,
        file: path(&format!("/home/u/.claude/projects/-r/{id}.jsonl")),
        cwd: path("/r"),
        place,
        started_at: Timestamp(1_000),
        last_activity: Timestamp(last_activity),
        title: Some(text("Fix the build")),
        agent_version: Some(text("2.1.302")),
        logged_branch: Some(text("main")),
        open_turn: false,
        tail_pending: false,
        archived: false,
        inheritance_unknown: false,
        format: Format::Known,
    }
}

fn sample_list() -> SessionList {
    let located = Place::Located(Location {
        project: project(),
        worktree: Some(WorktreeKey(path("/r/.git"))),
        rel: text("crates/a"),
        package: PackageMatch::Found(text("crates/a")),
    });
    let resolved = Place::Resolved(Resolved {
        project: project(),
        worktree: Some(WorktreeKey(path("/r/.git/worktrees/b"))),
        rel: text(""),
    });
    let other = Place::Resolved(Resolved {
        project: ProjectKey::Folder { root: path("/f") },
        worktree: None,
        rel: text("x"),
    });
    let child = SessionSummary {
        agent: Agent::Codex,
        parent: Some(Parent {
            session: key("a"),
            tool_call: Some(ToolCallId(text("toolu_01"))),
        }),
        title: None,
        agent_version: None,
        logged_branch: None,
        open_turn: true,
        tail_pending: true,
        archived: true,
        inheritance_unknown: true,
        format: Format::Partial,
        ..summary("a/agent-1", resolved, 600)
    };
    SessionList {
        generation: 3,
        sessions: Arc::from(vec![
            summary("a", located, 900),
            SessionSummary {
                agent: Agent::Other(text("other-agent")),
                format: Format::Unrecognized,
                ..summary("m", Place::Missing, 800)
            },
            summary("o", other, 700),
            child,
            SessionSummary {
                parent: Some(Parent {
                    session: key("gone"),
                    tool_call: None,
                }),
                ..summary("p", Place::Pending, 500)
            },
            summary("u", Place::Unresolvable(OpenError::Io(text("EACCES"))), 400),
        ]),
        diagnostics: Diagnostics {
            unknown_types: Arc::from(vec![(text("world_state"), 2), (text("x/y"), 1)]),
            bad_lines: 1,
            oversized_lines: 2,
            unreadable_dirs: Arc::from(vec![path("/home/u/.codex/sessions/2026")]),
        },
    }
}

#[test]
fn session_list_round_trips() {
    let list = sample_list();
    assert_eq!(round_trip(&list), list);
    let empty = SessionList::default();
    assert_eq!(round_trip(&empty), empty);
    assert_eq!(empty.diagnostics, Diagnostics::default());
    for summary in list.sessions.iter() {
        assert_eq!(&round_trip(summary), summary);
        assert_eq!(round_trip(&summary.place), summary.place);
        assert_eq!(round_trip(&summary.agent), summary.agent);
        assert_eq!(round_trip(&summary.format), summary.format);
        if let Some(parent) = &summary.parent {
            assert_eq!(&round_trip(parent), parent);
        }
    }
    assert_eq!(round_trip(&Timestamp(-1)), Timestamp(-1));
    assert_eq!(round_trip(&source()), source());
    assert_eq!(round_trip(&key("a")), key("a"));
}

#[test]
fn session_list_get_and_in_project() {
    let list = sample_list();
    assert_eq!(
        list.get(&key("m")).map(|s| s.last_activity),
        Some(Timestamp(800))
    );
    assert!(list.get(&key("zzz")).is_none());
    let other_source = SessionKey {
        source: SourceId(text("session-codex")),
        id: text("a"),
    };
    assert!(list.get(&other_source).is_none(), "ids are per source");

    let ids = |project: &ProjectKey| {
        list.in_project(project)
            .map(|s| s.key.id.to_string())
            .collect::<Vec<_>>()
    };
    // Located and Resolved both count, sub-agent sessions included, newest first.
    assert_eq!(ids(&project()), ["a", "a/agent-1"]);
    assert_eq!(ids(&ProjectKey::Folder { root: path("/f") }), ["o"]);
    assert!(
        ids(&ProjectKey::Folder {
            root: path("/r/.git")
        })
        .is_empty()
    );
}

fn positioned_cursor() -> Cursor {
    let mut state = vec![b'\n'; 256];
    state.extend_from_slice(&[0, 1, 2, 255]);
    Cursor::new(source(), 7, 1 << 40, state)
}

#[test]
fn session_cursor_bytes_round_trip() {
    assert!(Cursor::START.is_start());
    assert_eq!(Cursor::START.source(), None);
    assert_eq!(Cursor::START.adapter_version(), None);
    assert_eq!(Cursor::START.offset(), 0);
    assert!(Cursor::START.state().is_empty());
    assert_eq!(
        Cursor::from_bytes(&Cursor::START.to_bytes()),
        Some(Cursor::START)
    );

    let cursor = positioned_cursor();
    assert!(!cursor.is_start());
    assert_eq!(cursor.source(), Some(&source()));
    assert_eq!(cursor.adapter_version(), Some(7));
    assert_eq!(cursor.offset(), 1 << 40);
    assert_eq!(cursor.state().len(), 260);
    let bytes = cursor.to_bytes();
    assert_eq!(Cursor::from_bytes(&bytes), Some(cursor.clone()));

    // At offset 0 with no state is still not `START`: it names its source.
    let zero = Cursor::new(SourceId(text("")), 0, 0, Vec::new());
    assert_ne!(zero, Cursor::START);
    assert_eq!(Cursor::from_bytes(&zero.to_bytes()), Some(zero));

    // Unknown formats: empty, another version, every truncation of the
    // fixed fields, a source that is not UTF-8.
    assert_eq!(Cursor::from_bytes(&[]), None);
    assert_eq!(Cursor::from_bytes(&[2]), None);
    let fixed = 1 + 2 + source().0.len() + 4 + 8;
    for len in 2..fixed {
        assert_eq!(Cursor::from_bytes(&bytes[..len]), None, "{len}");
    }
    assert!(Cursor::from_bytes(&bytes[..fixed]).is_some_and(|c| c.state().is_empty()));
    assert_eq!(
        Cursor::from_bytes(&[1, 1, 0, 0xff, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
        None
    );

    // Through serde, as part of a `Batch` or `TurnIndex`.
    assert_eq!(round_trip(&cursor), cursor);
    assert_eq!(round_trip(&Cursor::START), Cursor::START);
    assert!(serde_json::from_str::<Cursor>("[9]").is_err());
}

fn tools() -> Vec<ToolKind> {
    let paths = |ps: &[&str]| -> Arc<[PathBuf]> { ps.iter().map(PathBuf::from).collect() };
    vec![
        ToolKind::Edit {
            paths: paths(&["/r/a.rs", "/r/b.rs"]),
        },
        ToolKind::Execute {
            command: text("cargo test"),
            workdir: path("/r"),
            exec_session: None,
            workdir_exact: true,
        },
        ToolKind::Execute {
            command: text("cd x && ls\n"),
            workdir: path("/r"),
            exec_session: Some(text("3")),
            workdir_exact: false,
        },
        ToolKind::Read {
            paths: paths(&["/r/a.rs"]),
        },
        ToolKind::Search { paths: paths(&[]) },
        ToolKind::SpawnAgent,
        ToolKind::Other,
    ]
}

fn anthropic(input: u64) -> RawTokens {
    RawTokens::Anthropic {
        input,
        output: 2,
        cache_read: 3,
        cache_write: 4,
        cache_write_5m: 1,
        cache_write_1h: 3,
    }
}

fn openai(input: u64) -> RawTokens {
    RawTokens::OpenAi {
        input,
        cached_input: 60,
        cache_write: 0,
        output: 40,
        reasoning_output: 10,
    }
}

fn records() -> Vec<Record> {
    let turn = TurnId(text("t1"));
    let call = ToolCallId(text("call_1"));
    let mut records = vec![
        Record::Context {
            at: Timestamp(1),
            cwd: path("/r"),
            logged_branch: Some(text("main")),
            model: Some(text("model-a")),
        },
        Record::Context {
            at: Timestamp(2),
            cwd: path("/r/sub"),
            logged_branch: None,
            model: None,
        },
        Record::TurnStarted {
            turn: turn.clone(),
            at: Timestamp(3),
        },
    ];
    for kind in tools() {
        records.push(Record::ToolStarted(ToolStarted {
            call: call.clone(),
            turn: Some(turn.clone()),
            at: Timestamp(4),
            name: text("exec_command"),
            kind,
        }));
    }
    records.push(Record::ToolStarted(ToolStarted {
        call: call.clone(),
        turn: None,
        at: Timestamp(4),
        name: text("x"),
        kind: ToolKind::Other,
    }));
    for (status, exec_session) in [
        (ToolStatus::Completed, Some(text("3"))),
        (ToolStatus::Failed, None),
        (ToolStatus::Denied, None),
        (ToolStatus::Interrupted, None),
    ] {
        records.push(Record::ToolFinished(ToolFinished {
            call: call.clone(),
            at: Timestamp(5),
            status,
            exec_session,
        }));
    }
    let usage = UsageRecord {
        at: Timestamp(6),
        turn: Some(turn.clone()),
        model: Some(text("model-b")),
        tokens: anthropic(10),
        key: UsageKey::Claude {
            message_id: text("msg_1"),
            request_id: Some(text("req_1")),
        },
        advisor: Some(AdvisorUsage {
            model: text("model-c"),
            tokens: anthropic(1),
        }),
        superseded: false,
        sidechain: true,
        origin: UsageOrigin::SessionLog,
    };
    records.push(Record::Usage(usage.clone()));
    records.push(Record::Usage(UsageRecord {
        turn: None,
        model: None,
        key: UsageKey::Claude {
            message_id: text("msg_2"),
            request_id: None,
        },
        advisor: None,
        sidechain: false,
        ..usage.clone()
    }));
    records.push(Record::Usage(UsageRecord {
        tokens: openai(100),
        key: UsageKey::CodexTokenCount {
            thread_id: text("thread"),
            total: openai(1_000),
        },
        advisor: None,
        superseded: true,
        sidechain: false,
        ..usage.clone()
    }));
    records.push(Record::Usage(UsageRecord {
        tokens: openai(100),
        key: UsageKey::CodexResponse {
            thread_id: text("thread"),
            response_id: text("resp_1"),
        },
        advisor: None,
        sidechain: false,
        ..usage
    }));
    records.push(Record::RateLimits(RateLimitSample {
        at: Timestamp(7),
        limit_id: Some(text("codex")),
        plan_type: Some(text("pro")),
        primary: Some(RateWindow {
            used_percent: 42.5,
            window_minutes: Some(300),
            resets_at: Some(Timestamp(1_800_000_000_000)),
        }),
        secondary: Some(RateWindow {
            used_percent: 0.1,
            window_minutes: None,
            resets_at: None,
        }),
    }));
    records.push(Record::RateLimits(RateLimitSample {
        at: Timestamp(8),
        limit_id: None,
        plan_type: None,
        primary: None,
        secondary: None,
    }));
    records.push(Record::TurnEnded {
        turn: turn.clone(),
        at: Timestamp(9),
        aborted: false,
    });
    records.push(Record::TurnEnded {
        turn,
        at: Timestamp(10),
        aborted: true,
    });
    records
}

#[test]
fn session_batch_round_trips() {
    let records: Vec<_> = records()
        .into_iter()
        .enumerate()
        .map(|(i, value)| Positioned {
            pos: 100 * (i as u64 / 2),
            seq: (i % 2) as u16,
            inherited: i < 3,
            value,
        })
        .collect();
    for record in &records {
        assert_eq!(&round_trip(record), record);
        assert_eq!(round_trip(&record.value), record.value);
    }
    let batch = Batch {
        reset: true,
        records,
        cursor: positioned_cursor(),
        at_end: false,
    };
    assert_eq!(round_trip(&batch), batch);
    let empty = Batch {
        reset: false,
        records: Vec::new(),
        cursor: Cursor::START,
        at_end: true,
    };
    assert_eq!(round_trip(&empty), empty);
    let positioned = Positioned {
        pos: 0,
        seq: u16::MAX,
        inherited: false,
        value: Timestamp(1),
    };
    assert_eq!(round_trip(&positioned), positioned);
}

#[test]
fn session_tool_and_usage_types_round_trip() {
    for kind in tools() {
        assert_eq!(round_trip(&kind), kind);
    }
    for status in [
        ToolStatus::Completed,
        ToolStatus::Failed,
        ToolStatus::Denied,
        ToolStatus::Interrupted,
    ] {
        assert_eq!(round_trip(&status), status);
    }
    for tokens in [anthropic(u64::MAX), openai(0)] {
        assert_eq!(round_trip(&tokens), tokens);
    }
    for key in [
        UsageKey::Claude {
            message_id: text("msg_1"),
            request_id: None,
        },
        UsageKey::CodexTokenCount {
            thread_id: text("t"),
            total: openai(5),
        },
        UsageKey::CodexResponse {
            thread_id: text("t"),
            response_id: text("r"),
        },
    ] {
        assert_eq!(round_trip(&key), key);
    }
    let advisor = AdvisorUsage {
        model: text("m"),
        tokens: anthropic(1),
    };
    assert_eq!(round_trip(&advisor), advisor);
    assert_eq!(
        round_trip(&UsageOrigin::SessionLog),
        UsageOrigin::SessionLog
    );
    let window = RateWindow {
        used_percent: 99.75,
        window_minutes: Some(10_080),
        resets_at: Some(Timestamp(0)),
    };
    assert_eq!(round_trip(&window), window);
    let id = ToolCallId(text("toolu_01"));
    assert_eq!(round_trip(&id), id);
    let turn = TurnId(text("uuid"));
    assert_eq!(round_trip(&turn), turn);
}

#[test]
fn session_turn_index_round_trips() {
    let call = |kind, finished, result| ToolCall {
        id: ToolCallId(text("toolu_01")),
        name: text("Edit"),
        kind,
        started_at: Timestamp(2),
        finished,
        input: ContentRef::new(10, 20, 1),
        result,
    };
    let items: Arc<[TurnItem]> = Arc::from(vec![
        TurnItem::AgentText(ContentRef::new(0, 5, 0)),
        TurnItem::Injected(ContentRef::new(5, 5, 0)),
        TurnItem::Tool(call(
            ToolKind::Edit {
                paths: Arc::from(vec![PathBuf::from("/r/a.rs")]),
            },
            Some((Timestamp(3), ToolStatus::Completed)),
            Some(ContentRef::new(30, 40, 0)),
        )),
        TurnItem::Tool(call(ToolKind::Other, None, None)),
    ]);
    for item in items.iter() {
        assert_eq!(&round_trip(item), item);
    }
    let turn = Turn {
        id: TurnId(text("t1")),
        ordinal: 1,
        started_at: Timestamp(1),
        ended_at: Some(Timestamp(4)),
        prompt: Some(ContentRef::new(0, 1, 0)),
        prompt_preview: text("Fix the build"),
        items,
    };
    let open = Turn {
        id: TurnId(text("t2")),
        ordinal: 2,
        ended_at: None,
        prompt: None,
        prompt_preview: text(""),
        items: Arc::from(Vec::new()),
        ..turn.clone()
    };
    let index = TurnIndex {
        session: key("a"),
        cursor: positioned_cursor(),
        turns: Arc::from(vec![turn, open]),
        format: Format::Partial,
    };
    assert_eq!(round_trip(&index), index);

    let at = ContentRef::new(u64::MAX, u32::MAX, u16::MAX);
    assert_eq!(round_trip(&at), at);
    assert_eq!(
        (at.pos(), at.line_len(), at.part()),
        (u64::MAX, u32::MAX, u16::MAX)
    );
}

#[test]
fn session_content_round_trips() {
    let edits: Arc<[FileEdit]> = Arc::from(vec![
        FileEdit {
            path: PathBuf::from("/r/new.rs"),
            op: EditOp::Write {
                content: text("fn main() {}\n"),
                created: true,
            },
        },
        FileEdit {
            path: PathBuf::from("/r/a.rs"),
            op: EditOp::Replace {
                old: text("a"),
                new: text("b"),
                all: false,
            },
        },
        FileEdit {
            path: PathBuf::from("/r/b.rs"),
            op: EditOp::Patch {
                hunks: text("@@ -1 +1 @@\n-a\n+b\n"),
                move_to: Some(PathBuf::from("/r/c.rs")),
            },
        },
        FileEdit {
            path: PathBuf::from("/r/d.rs"),
            op: EditOp::Patch {
                hunks: text(""),
                move_to: None,
            },
        },
        FileEdit {
            path: PathBuf::from("/r/e.rs"),
            op: EditOp::Add { content: text("x") },
        },
        FileEdit {
            path: PathBuf::from("/r/f.rs"),
            op: EditOp::Delete,
        },
    ]);
    for edit in edits.iter() {
        assert_eq!(&round_trip(edit), edit);
        assert_eq!(round_trip(&edit.op), edit.op);
    }
    for content in [Content::Text(text("Done.\n")), Content::Edits(edits)] {
        assert_eq!(round_trip(&content), content);
    }
}
