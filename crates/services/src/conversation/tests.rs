use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use lectern_kernel::{Event, Service};

use super::super::session::{Agent, SourceId, Timestamp};
use super::*;

/// `Conversations` with nothing to show, to check the trait is usable as a
/// shared trait object.
struct Empty;

impl Conversations for Empty {
    fn watch(&self, _session: &SessionKey) -> ConversationInterest {
        ConversationInterest::new(|| {})
    }
    fn snapshot(&self, _session: &SessionKey) -> Option<Arc<ConversationSnapshot>> {
        None
    }
    fn command(&self, _session: &SessionKey, _command: ConversationCommand) {}
    fn waiting(&self) -> Arc<[SessionKey]> {
        Arc::from(Vec::new())
    }
}

fn round_trip<T>(value: &T) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let json = serde_json::to_string(value).expect("serializes");
    serde_json::from_str(&json).expect("deserializes")
}

fn key() -> SessionKey {
    SessionKey {
        source: SourceId(Arc::from("builtin")),
        id: Arc::from("11111111-2222-3333-4444-555555555555"),
    }
}

fn passage(text: &str) -> Passage {
    Passage {
        text: Arc::from(text),
        truncated: false,
    }
}

fn entry(key: &str, body: EntryBody) -> Entry {
    Entry {
        key: EntryKey(Arc::from(key)),
        body,
    }
}

fn snapshot() -> ConversationSnapshot {
    let notes = PromptOrigin::ReviewNotes {
        count: 2,
        locations: Arc::from(vec![
            NoteLocation {
                path: Arc::from(Path::new("/repo/src/a.rs")),
                line: 12,
            },
            NoteLocation {
                path: Arc::from(Path::new("/repo/src/b.rs")),
                line: 3,
            },
        ]),
    };
    let edit = EditPreview {
        path: Arc::from(Path::new("/repo/src/a.rs")),
        new_file: false,
        added: 1,
        removed: 1,
        rows: Arc::from(vec![
            PreviewRow {
                kind: RowKind::Context,
                text: Arc::from("fn a() {"),
            },
            PreviewRow {
                kind: RowKind::Removed,
                text: Arc::from("    1"),
            },
            PreviewRow {
                kind: RowKind::Added,
                text: Arc::from("    2"),
            },
        ]),
        omitted: 4,
    };
    ConversationSnapshot {
        session: key(),
        generation: 9,
        agent: Agent::ClaudeCode,
        load: Load::Ready,
        link: Link::Working,
        can_prompt: Err(Unavailable::Busy),
        context: Some(ContextUsage {
            used: 1200,
            size: 200_000,
        }),
        earlier: 3,
        entries: Arc::from(vec![
            entry(
                "h:0:0",
                EntryBody::Prompt {
                    text: passage("hello"),
                    origin: PromptOrigin::User,
                },
            ),
            entry(
                "h:0:1",
                EntryBody::AgentText {
                    text: passage("hi"),
                    streaming: false,
                },
            ),
            entry(
                "h:0:2",
                EntryBody::Injected {
                    text: Passage {
                        text: Arc::from("note"),
                        truncated: true,
                    },
                },
            ),
            entry(
                "h:0:3",
                EntryBody::Tool(ToolEntry {
                    kind: CallKind::Edit,
                    state: CallState::Completed,
                    summary: Arc::from("a.rs"),
                }),
            ),
            entry("h:0:4", EntryBody::Ended(EndReason::MaxTokens)),
            entry(
                "l:1:0",
                EntryBody::Prompt {
                    text: passage(""),
                    origin: notes,
                },
            ),
            entry(
                "l:1:1",
                EntryBody::AgentText {
                    text: passage("working"),
                    streaming: true,
                },
            ),
        ]),
        plan: Some(Arc::from(vec![
            PlanStep {
                text: Arc::from("read"),
                state: StepState::Completed,
            },
            PlanStep {
                text: Arc::from("edit"),
                state: StepState::InProgress,
            },
            PlanStep {
                text: Arc::from("test"),
                state: StepState::Pending,
            },
        ])),
        permissions: Arc::from(vec![PermissionRequest {
            id: PermissionId(4),
            kind: CallKind::Edit,
            title: Arc::from("Edit a.rs"),
            detail: PermissionDetail::Edits(Arc::from(vec![edit])),
            options: Arc::from(vec![
                PermissionOption {
                    id: OptionId(Arc::from("allow")),
                    kind: OptionKind::AllowOnce,
                },
                PermissionOption {
                    id: OptionId(Arc::from("reject")),
                    kind: OptionKind::RejectOnce,
                },
            ]),
            asked_at: Timestamp(1_700_000_000_000),
        }]),
    }
}

#[test]
fn conversations_is_a_single_service() {
    assert_eq!(<dyn Conversations as Service>::ID, "conversations");
    const { assert!(!<dyn Conversations as Service>::MULTIPLE) };
    let conversations: Arc<dyn Conversations> = Arc::new(Empty);
    assert!(conversations.snapshot(&key()).is_none());
    assert!(conversations.waiting().is_empty());
    drop(conversations.watch(&key()));
    conversations.command(&key(), ConversationCommand::Cancel);
}

#[test]
fn conversation_events_round_trip() {
    fn event<E: Event + Clone>() {}
    event::<ConversationEvent>();

    let events = [
        ConversationEvent::Changed {
            session: key(),
            generation: u64::MAX,
        },
        ConversationEvent::WaitingChanged,
    ];
    for event in &events {
        assert_eq!(&round_trip(event), event);
    }
}

#[test]
fn snapshot_round_trips() {
    let snapshot = snapshot();
    assert_eq!(round_trip(&snapshot), snapshot);

    let bare = ConversationSnapshot {
        load: Load::Failed(HistoryError::Unrecognized),
        link: Link::None,
        can_prompt: Ok(()),
        context: None,
        earlier: 0,
        entries: Arc::from(Vec::new()),
        plan: None,
        permissions: Arc::from(Vec::new()),
        ..snapshot
    };
    assert_eq!(round_trip(&bare), bare);
}

#[test]
fn commands_round_trip() {
    let commands = [
        ConversationCommand::Prompt {
            text: Arc::from("go"),
            origin: PromptOrigin::User,
        },
        ConversationCommand::Prompt {
            text: Arc::from("fix these"),
            origin: PromptOrigin::ReviewNotes {
                count: 1,
                locations: Arc::from(vec![NoteLocation {
                    path: Arc::from(Path::new("/repo/x.rs")),
                    line: 7,
                }]),
            },
        },
        ConversationCommand::Cancel,
        ConversationCommand::Answer {
            request: PermissionId(1),
            option: OptionId(Arc::from("o1")),
        },
        ConversationCommand::LoadEarlier,
        ConversationCommand::Disconnect,
        ConversationCommand::Dismiss,
    ];
    for command in &commands {
        assert_eq!(&round_trip(command), command);
    }
}

#[test]
fn states_and_errors_round_trip() {
    let links = [
        Link::None,
        Link::Connecting,
        Link::Idle,
        Link::Working,
        Link::Stopping,
        Link::Failed(LinkError::Spawn),
        Link::Failed(LinkError::Unsupported),
        Link::Failed(LinkError::LoadFailed),
        Link::Failed(LinkError::Protocol),
        Link::Failed(LinkError::Exited { code: Some(1) }),
        Link::Failed(LinkError::Exited { code: None }),
        Link::Failed(LinkError::AuthRequired),
        Link::Failed(LinkError::PromptFailed {
            message: Arc::from("rejected"),
        }),
    ];
    for link in &links {
        assert_eq!(&round_trip(link), link);
    }
    let unavailable = [
        Unavailable::NotResumable,
        Unavailable::OutsideProject,
        Unavailable::NotInstalled(AcpProgram::ClaudeCode),
        Unavailable::NotInstalled(AcpProgram::Codex),
        Unavailable::Unsupported,
        Unavailable::TooMany,
        Unavailable::Busy,
    ];
    for reason in &unavailable {
        assert_eq!(&round_trip(reason), reason);
    }
    let loads = [
        Load::Loading,
        Load::Ready,
        Load::Failed(HistoryError::NotFound),
        Load::Failed(HistoryError::Unrecognized),
        Load::Failed(HistoryError::Io),
    ];
    for load in &loads {
        assert_eq!(&round_trip(load), load);
    }
    let ended = [
        EndReason::MaxTokens,
        EndReason::MaxTurnRequests,
        EndReason::Refusal,
        EndReason::Cancelled,
    ];
    for reason in &ended {
        assert_eq!(&round_trip(reason), reason);
    }
}

#[test]
fn permission_details_round_trip() {
    let details = [
        PermissionDetail::Command {
            text: Arc::from("cargo test"),
            workdir: Some(Arc::from(Path::new("/repo"))),
        },
        PermissionDetail::Command {
            text: Arc::from("ls"),
            workdir: None,
        },
        PermissionDetail::None,
    ];
    for detail in &details {
        assert_eq!(&round_trip(detail), detail);
    }
    let kinds = [
        OptionKind::AllowOnce,
        OptionKind::AllowAlways,
        OptionKind::RejectOnce,
        OptionKind::RejectAlways,
    ];
    for kind in &kinds {
        assert_eq!(&round_trip(kind), kind);
    }
}

#[test]
fn option_kinds_are_in_button_order() {
    assert!(OptionKind::AllowOnce < OptionKind::AllowAlways);
    assert!(OptionKind::AllowAlways < OptionKind::RejectOnce);
    assert!(OptionKind::RejectOnce < OptionKind::RejectAlways);
}

#[test]
fn acp_program_names_are_distinct() {
    assert!(!AcpProgram::ClaudeCode.name().is_empty());
    assert!(!AcpProgram::Codex.name().is_empty());
    assert_ne!(AcpProgram::ClaudeCode.name(), AcpProgram::Codex.name());
}

#[test]
fn interest_runs_its_callback_once_on_drop() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let interest = ConversationInterest::new(move || {
        seen.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(format!("{interest:?}"), "ConversationInterest { .. }");
    drop(interest);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
