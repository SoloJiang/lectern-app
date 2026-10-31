//! The semantic trees (docs/design/conversation.md「测试」「语义树」), as insta
//! snapshots: the page with a conversation of every kind of entry, and the
//! page of a review that is not of a session. The text has no bounds and no
//! call sites (`stage::tree_of`), which a layout change would move.

use std::sync::Arc;

use gpui::TestAppContext;
use lectern_kernel::Name;
use lectern_services::conversation::{
    CallKind, CallState, ContextUsage, EditPreview, EndReason, Entry, EntryBody, Link,
    NoteLocation, OptionId, OptionKind, Passage, PermissionDetail, PermissionId, PermissionOption,
    PermissionRequest, PlanStep, PreviewRow, PromptOrigin, RowKind, StepState, Unavailable,
};
use lectern_services::project::{ProjectKey, WorktreeKey};
use lectern_services::review::ReviewTarget;
use lectern_services::session::Timestamp;
use lectern_services::workspace::{Route, Target, screens};

use crate::testing::{agent_text, entry_key, passage, prompt, session, tool, with_entries};

use super::stage::*;

#[gpui::test]
fn conversation_tree_history(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    let notes = Entry {
        key: entry_key("h:2:0"),
        body: EntryBody::Prompt {
            text: passage("the full prompt, not shown"),
            origin: PromptOrigin::ReviewNotes {
                count: 2,
                locations: Arc::from(vec![NoteLocation {
                    path: Arc::from(std::path::Path::new("/work/app/src/lib.rs")),
                    line: 40,
                }]),
            },
        },
    };
    let cut = Entry {
        key: entry_key("h:1:1"),
        body: EntryBody::AgentText {
            text: Passage {
                text: Arc::from("a long answer, cut"),
                truncated: true,
            },
            streaming: false,
        },
    };
    let injected = Entry {
        key: entry_key("h:0:5"),
        body: EntryBody::Injected {
            text: passage("a background task finished"),
        },
    };
    let entries = vec![
        prompt("h:0:0", "fix the redirect"),
        agent_text("h:0:1", "I will look at the router.", false),
        injected,
        tool("h:0:2", CallKind::Read, "router.rs", CallState::Completed),
        tool("h:0:3", CallKind::Edit, "router.rs", CallState::Completed),
        tool("h:0:4", CallKind::Execute, "cargo test", CallState::Failed),
        prompt("h:1:0", "again"),
        cut,
        notes,
        Entry {
            key: entry_key("h:2:1"),
            body: EntryBody::Ended(EndReason::MaxTokens),
        },
    ];
    let mut snap = with_entries(&key, 3, entries);
    snap.earlier = 2;
    snap.link = Link::Idle;
    snap.context = Some(ContextUsage {
        used: 72,
        size: 100,
    });
    snap.plan = Some(Arc::from(vec![
        PlanStep {
            text: Arc::from("find the bug"),
            state: StepState::Completed,
        },
        PlanStep {
            text: Arc::from("fix it"),
            state: StepState::InProgress,
        },
    ]));
    rig.conversations.set(snap);
    let (_host, vcx) = open(cx, &rig, &key);
    insta::assert_snapshot!(tree(vcx, all));
}

/// A working session that waits for two answers, the second one shown too:
/// the status line, the list, the first request whole, the line and the second
/// request under it, and the Stop button in the place of Send.
#[gpui::test]
fn conversation_tree_live_with_permission(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    let options = |name: &str| {
        [
            ("allow-always", OptionKind::AllowAlways),
            ("reject-once", OptionKind::RejectOnce),
            ("allow-once", OptionKind::AllowOnce),
        ]
        .map(|(id, kind)| PermissionOption {
            id: OptionId(Arc::from(format!("{name}-{id}"))),
            kind,
        })
    };
    let mut snap = with_entries(
        &key,
        4,
        vec![
            prompt("h:0:0", "fix the redirect"),
            tool("l:1:0", CallKind::Execute, "cargo test", CallState::Pending),
        ],
    );
    snap.link = Link::Working;
    snap.can_prompt = Err(Unavailable::Busy);
    snap.permissions = Arc::from(vec![
        PermissionRequest {
            id: PermissionId(7),
            kind: CallKind::Execute,
            title: Arc::from("Run cargo test"),
            detail: PermissionDetail::Command {
                text: Arc::from("cargo test -p router\ncargo clippy"),
                workdir: Some(Arc::from(std::path::Path::new("/work/app/crates/router"))),
            },
            options: Arc::from(options("first")),
            asked_at: Timestamp(1),
        },
        PermissionRequest {
            id: PermissionId(8),
            kind: CallKind::Edit,
            title: Arc::from("Edit router.rs"),
            detail: PermissionDetail::Edits(Arc::from(vec![EditPreview {
                path: Arc::from(std::path::Path::new("/work/app/src/router.rs")),
                new_file: false,
                added: 1,
                removed: 1,
                rows: Arc::from(vec![
                    PreviewRow {
                        kind: RowKind::Removed,
                        text: Arc::from("    redirect(old)"),
                    },
                    PreviewRow {
                        kind: RowKind::Added,
                        text: Arc::from("    redirect(new)"),
                    },
                ]),
                omitted: 0,
            }])),
            options: Arc::from(options("second")),
            asked_at: Timestamp(2),
        },
    ]);
    rig.conversations.set(snap);
    let (_host, vcx) = open(cx, &rig, &key);
    click(vcx, "conversation.permission.more");
    frame(vcx);
    insta::assert_snapshot!(tree(vcx, all));
}

#[gpui::test]
fn conversation_tree_not_session(cx: &mut TestAppContext) {
    let rig = rig(cx);
    rig.activate_main();
    rig.activate_live();
    rig.screen(Route {
        screen: Name::Borrowed(screens::REVIEW),
        target: Target::Review(ReviewTarget::Outside {
            project: ProjectKey::Folder {
                root: Arc::from(std::path::Path::new("/work/app")),
            },
            worktree: WorktreeKey(Arc::from(std::path::Path::new("/work/app"))),
        }),
    });
    let (_host, vcx) = window(cx, &rig);
    insta::assert_snapshot!(tree(vcx, all));
}
