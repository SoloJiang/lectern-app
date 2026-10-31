//! 历史窗口（`history_*`）。

use std::path::{Path, PathBuf};

use lectern_services::conversation::{
    CallKind, CallState, Conversations, EntryBody, HistoryError, Load, PromptOrigin,
};
use lectern_services::session::{SessionError, SessionKey, ToolKind, ToolStatus};

use super::support::*;

fn rig() -> (Rig, SessionKey) {
    let rig = Rig::new(Path::new("/nonexistent"));
    let session = rig.claude_session();
    (rig, session)
}

fn text_bytes(entries: &[lectern_services::conversation::Entry]) -> usize {
    entries
        .iter()
        .map(|e| match &e.body {
            EntryBody::AgentText { text, .. } | EntryBody::Prompt { text, .. } => text.text.len(),
            _ => 0,
        })
        .sum()
}

#[test]
fn history_orders_entries_and_keys() {
    let (rig, session) = rig();
    let s = &rig.claude;
    let turns = vec![
        s.turn(
            1,
            "first",
            vec![
                s.agent_text("one"),
                s.tool(
                    "t1",
                    "Edit",
                    ToolKind::Edit {
                        paths: vec![
                            PathBuf::from("/work/project/src/a.rs"),
                            PathBuf::from("/work/project/b.rs"),
                        ]
                        .into(),
                    },
                    Some(ToolStatus::Completed),
                ),
            ],
        ),
        s.turn(7, "second", vec![s.agent_text("two")]),
    ];
    s.set_turns(&session, turns);
    let _interest = rig.engine.watch(&session);
    let snapshot = rig.engine.snapshot(&session).unwrap();
    assert_eq!(snapshot.load, Load::Ready);
    assert_eq!(snapshot.earlier, 0);
    let keys: Vec<&str> = snapshot.entries.iter().map(|e| &*e.key.0).collect();
    assert_eq!(keys, ["h:1:0", "h:1:1", "h:1:2", "h:7:0", "h:7:1"]);
    assert!(matches!(
        &snapshot.entries[0].body,
        EntryBody::Prompt { text, origin: PromptOrigin::User } if &*text.text == "first" && !text.truncated
    ));
    assert!(matches!(
        &snapshot.entries[1].body,
        EntryBody::AgentText { text, streaming: false } if &*text.text == "one"
    ));
    let EntryBody::Tool(tool) = &snapshot.entries[2].body else {
        panic!("a tool entry");
    };
    assert_eq!(tool.kind, CallKind::Edit);
    assert_eq!(tool.state, CallState::Completed);
    assert_eq!(&*tool.summary, "a.rs, b.rs");
}

#[test]
fn history_tool_rows_use_the_index_only() {
    let (rig, session) = rig();
    let s = &rig.claude;
    let mut open_turn = s.turn(
        2,
        "open",
        vec![s.tool(
            "t5",
            "Bash",
            ToolKind::Execute {
                command: "cargo test\n--all".into(),
                workdir: Path::new("/work").into(),
                exec_session: None,
                workdir_exact: true,
            },
            None,
        )],
    );
    open_turn.ended_at = None;
    let closed = s.turn(
        1,
        "closed",
        vec![
            s.tool(
                "t1",
                "Grep",
                ToolKind::Search {
                    paths: vec![].into(),
                },
                Some(ToolStatus::Failed),
            ),
            s.tool("t2", "Task", ToolKind::SpawnAgent, Some(ToolStatus::Denied)),
            s.tool(
                "t3",
                "WebFetch",
                ToolKind::Other,
                Some(ToolStatus::Interrupted),
            ),
            s.tool(
                "t4",
                "Read",
                ToolKind::Read {
                    paths: vec![PathBuf::from("/x/y/z.md")].into(),
                },
                None,
            ),
        ],
    );
    s.set_turns(&session, vec![closed, open_turn]);
    let _interest = rig.engine.watch(&session);
    let snapshot = rig.engine.snapshot(&session).unwrap();
    let tools: Vec<_> = snapshot
        .entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::Tool(t) => Some((t.kind, t.state, t.summary.to_string())),
            _ => None,
        })
        .collect();
    assert_eq!(
        tools,
        [
            (CallKind::Search, CallState::Failed, "Grep".to_owned()),
            (CallKind::SpawnAgent, CallState::Denied, "Task".to_owned()),
            (CallKind::Other, CallState::Cancelled, "WebFetch".to_owned()),
            // No result in a turn that ended: it was cut off.
            (CallKind::Read, CallState::Cancelled, "z.md".to_owned()),
            // No result in the turn that is still open: still running; the first line only.
            (
                CallKind::Execute,
                CallState::Running,
                "cargo test".to_owned()
            ),
        ]
    );
}

#[test]
fn history_takes_the_last_turns_and_counts_the_earlier_ones() {
    let (rig, session) = rig();
    let turns = (1..=30)
        .map(|n| rig.claude.turn(n, &format!("prompt {n}"), vec![]))
        .collect();
    rig.claude.set_turns(&session, turns);
    let _interest = rig.engine.watch(&session);
    let snapshot = rig.engine.snapshot(&session).unwrap();
    assert_eq!(snapshot.entries.len(), 20);
    assert_eq!(snapshot.earlier, 10);
    assert_eq!(&*snapshot.entries[0].key.0, "h:11:0");
}

#[test]
fn history_stops_at_one_mebibyte_and_counts_the_rest_as_earlier() {
    let (rig, session) = rig();
    let big = "x".repeat(64 << 10);
    let turns = (1..=3)
        .map(|n| {
            let items = (0..10).map(|_| rig.claude.agent_text(&big)).collect();
            rig.claude.turn(n, "p", items)
        })
        .collect();
    rig.claude.set_turns(&session, turns);
    let _interest = rig.engine.watch(&session);
    let snapshot = rig.engine.snapshot(&session).unwrap();
    // Each turn is about 640 KiB: only the newest fits.
    assert_eq!(snapshot.earlier, 2);
    assert!(snapshot.entries.iter().all(|e| e.key.0.starts_with("h:3:")));
    assert!(text_bytes(&snapshot.entries) <= 1 << 20);
}

#[test]
fn history_trims_the_front_of_one_turn_over_the_limit() {
    let (rig, session) = rig();
    let big = "x".repeat(64 << 10);
    let items = (0..20).map(|_| rig.claude.agent_text(&big)).collect();
    let turn = rig.claude.turn(1, "p", items);
    rig.claude.set_turns(&session, vec![turn]);
    let _interest = rig.engine.watch(&session);
    let snapshot = rig.engine.snapshot(&session).unwrap();
    let total = text_bytes(&snapshot.entries);
    assert!(total <= 1 << 20, "{total}");
    assert!(total > (1 << 20) - (64 << 10));
    // The newest entry is still there.
    assert_eq!(&*snapshot.entries.last().unwrap().key.0, "h:1:20");
}

#[test]
fn history_marks_long_passages_truncated() {
    let (rig, session) = rig();
    let long = "界".repeat(30_000);
    let turn = rig.claude.turn(1, &long, vec![]);
    rig.claude.set_turns(&session, vec![turn]);
    let _interest = rig.engine.watch(&session);
    let snapshot = rig.engine.snapshot(&session).unwrap();
    let EntryBody::Prompt { text, .. } = &snapshot.entries[0].body else {
        panic!("a prompt");
    };
    assert!(text.truncated);
    assert!(text.text.len() <= 64 << 10);
    assert!(text.text.chars().all(|c| c == '界'));
}

#[test]
fn history_maps_read_errors() {
    for (error, expected) in [
        (SessionError::NotFound, HistoryError::NotFound),
        (SessionError::Unrecognized, HistoryError::Unrecognized),
        (SessionError::Io("disk".into()), HistoryError::Io),
        (SessionError::TooLarge(9), HistoryError::Io),
    ] {
        let (rig, session) = rig();
        rig.claude.fail_turns(&session, error);
        let _interest = rig.engine.watch(&session);
        let snapshot = rig.engine.snapshot(&session).unwrap();
        assert_eq!(snapshot.load, Load::Failed(expected));
        assert!(snapshot.entries.is_empty());
    }
}

#[test]
fn history_fails_whole_when_a_body_cannot_be_read() {
    let (rig, session) = rig();
    let unreadable = rig.claude.content(Err(SessionError::Io("gone".into())));
    let mut turn = rig.claude.turn(1, "p", vec![]);
    turn.prompt = Some(unreadable);
    rig.claude.set_turns(&session, vec![turn]);
    let _interest = rig.engine.watch(&session);
    let snapshot = rig.engine.snapshot(&session).unwrap();
    assert_eq!(snapshot.load, Load::Failed(HistoryError::Io));
    assert!(snapshot.entries.is_empty());
}
