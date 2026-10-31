//! The canary test (docs/design/conversation.md「日志与诊断」): prompts, the
//! agent's text, injected text, tool summaries, permission titles, commands,
//! working directories, preview paths and lines, plan steps, the text of a
//! failed prompt, option IDs, review notes' places and the session title carry
//! the canary through the whole flow of step 1 at trace level (the plugins,
//! the page on a session, the history, the live updates, a failed link, the
//! stop key, the page switch, the route changes, the services leaving and
//! coming back), and no record carries it.

use std::path::Path;
use std::sync::Arc;

use gpui::TestAppContext;
use lectern_logging::testing;
use lectern_services::conversation::{
    CallKind, CallState, ConversationCommand, EditPreview, Entry, EntryBody, Link, LinkError,
    NoteLocation, OptionId, OptionKind, PermissionDetail, PermissionId, PermissionOption,
    PermissionRequest, PlanStep, PreviewRow, PromptOrigin, RowKind, StepState,
};
use lectern_services::session::Timestamp;

use crate::plugin::LIVE_ID;
use crate::testing::{
    Rig, agent_text, entry_key, passage, prompt, session, snapshot, summary, tool, with_entries,
};

use super::stage::*;

const CANARY: &str = "conversation-canary-8c41e07a";

/// A snapshot with the canary in every kind of content the crate shows.
fn loaded(generation: u64) -> lectern_services::conversation::ConversationSnapshot {
    let key = session(1);
    let notes = Entry {
        key: entry_key("h:1:0"),
        body: EntryBody::Prompt {
            text: passage(&format!("the notes prompt {CANARY}")),
            origin: PromptOrigin::ReviewNotes {
                count: 1,
                locations: Arc::from(vec![NoteLocation {
                    path: Arc::from(Path::new(&format!("/work/app/{CANARY}.rs"))),
                    line: 7,
                }]),
            },
        },
    };
    let injected = Entry {
        key: entry_key("h:0:2"),
        body: EntryBody::Injected {
            text: passage(&format!("background task {CANARY}")),
        },
    };
    let mut snap = with_entries(
        &key,
        generation,
        vec![
            prompt("h:0:0", &format!("prompt {CANARY}")),
            agent_text("h:0:1", &format!("answer {CANARY}"), false),
            injected,
            tool(
                "h:0:3",
                CallKind::Execute,
                &format!("run {CANARY}"),
                CallState::Completed,
            ),
            notes,
            // The provider's inconsistency, to write the warning.
            prompt("h:0:0", &format!("again {CANARY}")),
        ],
    );
    snap.earlier = 1;
    snap.plan = Some(Arc::from(vec![PlanStep {
        text: Arc::from(format!("step {CANARY}")),
        state: StepState::InProgress,
    }]));
    snap.permissions = Arc::from(vec![PermissionRequest {
        id: PermissionId(1),
        kind: CallKind::Execute,
        title: Arc::from(format!("title {CANARY}")),
        detail: PermissionDetail::Command {
            text: Arc::from(format!("rm -rf {CANARY}")),
            workdir: Some(Arc::from(Path::new(&format!("/work/{CANARY}")))),
        },
        options: Arc::from(vec![PermissionOption {
            id: OptionId(Arc::from(format!("option-{CANARY}"))),
            kind: OptionKind::AllowOnce,
        }]),
        asked_at: Timestamp(5),
    }]);
    let _ = EditPreview {
        path: Arc::from(Path::new(&format!("/work/{CANARY}.rs"))),
        new_file: false,
        added: 1,
        removed: 0,
        rows: Arc::from(vec![PreviewRow {
            kind: RowKind::Added,
            text: Arc::from(format!("+ {CANARY}")),
        }]),
        omitted: 0,
    };
    snap.link = Link::Working;
    snap
}

#[gpui::test]
fn logs_hold_no_contents(cx: &mut TestAppContext) {
    let capture = testing::capture();
    testing::set_callsite_limit(1_000_000);
    cx.executor().allow_parking();
    cx.update(|cx| {
        lectern_ui::init_fixed(
            cx,
            lectern_i18n::Strings::of(lectern_i18n::Locale::En),
            lectern_ui::Appearance::Light,
            false,
        );
    });
    let mut titled = summary(&session(1));
    titled.title = Some(Arc::from(format!("title {CANARY}")));
    titled.cwd = Arc::from(Path::new(&format!("/work/{CANARY}")));
    let rig = Rig::with_sessions(vec![titled, summary(&session(2))]);
    let key = session(1);
    // The window shows nothing yet when the plugin starts: it writes
    // `route unavailable`.
    rig.activate_main();
    rig.activate_live();
    rig.conversations.set(loaded(1));
    rig.review(&key);
    let (_host, vcx) = window(cx, &rig);
    frame(vcx);

    // Live updates, the stop key, the page switch.
    let mut snap = loaded(2);
    snap.entries = Arc::from(
        snap.entries
            .iter()
            .cloned()
            .chain([agent_text("l:2:0", &format!("streaming {CANARY}"), true)])
            .collect::<Vec<_>>(),
    );
    rig.conversations.set(snap);
    rig.changed(&key, 2);
    frame(vcx);
    let focus = {
        let tab = tab_of(&_host, vcx);
        vcx.update(|_, cx| tab.read(cx).focus_handle())
    };
    vcx.update(|window, cx| window.focus(&focus, cx));
    vcx.simulate_keystrokes("cmd-.");
    click(vcx, "conversation.pages.item[terminal]");
    click(vcx, "conversation.pages.item[messages]");

    // A failed link with the agent's own words, then dismissed.
    let mut failed = loaded(3);
    failed.link = Link::Failed(LinkError::PromptFailed {
        message: Arc::from(format!("error {CANARY}")),
    });
    rig.conversations.set(failed);
    rig.changed(&key, 3);
    frame(vcx);
    click(vcx, "conversation.notice.action");
    click(vcx, "conversation.earlier");
    assert!(
        rig.conversations
            .commands()
            .iter()
            .any(|(_, command)| matches!(command, ConversationCommand::Cancel)),
        "the stop key reached the provider"
    );

    // The routes, the services leaving and coming back, the plugins leaving.
    rig.screen(Rig::review_route(&session(2)));
    frame(vcx);
    rig.home();
    frame(vcx);
    rig.review(&key);
    frame(vcx);
    rig.deactivate_live();
    frame(vcx);
    rig.kernel.set_enabled(LIVE_ID, true);
    rig.activate_live();
    frame(vcx);
    rig.conversations.set(snapshot(&key, 99));
    rig.deactivate_main();

    capture.assert_absent(CANARY);
    capture.assert_absent("fixture");
    let lines = capture.lines();
    let find = |message: &str| {
        lines
            .iter()
            .find(|line| line.contains(&format!("\"msg\":\"{message}\"")))
            .unwrap_or_else(|| panic!("`{message}` is written: {lines:#?}"))
    };
    let shown = find("conversation shown");
    for field in [
        "\"page\":\"messages\"",
        "\"link\":",
        "\"entries\":",
        "\"pending\":",
    ] {
        assert!(shown.contains(field), "{field} in {shown}");
    }
    let inconsistent = find("snapshot inconsistent");
    assert!(
        inconsistent.contains("\"kind\":\"duplicate_key\""),
        "{inconsistent}"
    );
    assert!(inconsistent.contains("\"generation\":"), "{inconsistent}");
    let failed = find("conversation failed shown");
    assert!(failed.contains("\"error\":\"prompt_failed\""), "{failed}");
    find("stop requested");
    find("route unavailable");
    find("route changed");
    find("page switched");
    find("snapshot applied");
    // Every record about the session carries its tag, not its ID.
    assert!(
        find("conversation shown").contains("\"session\":"),
        "the session scope is on the record"
    );
}
