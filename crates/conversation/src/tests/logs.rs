//! The canary test (docs/design/conversation.md「日志与诊断」): prompts, the
//! agent's text, injected text, tool summaries, permission titles, commands,
//! working directories, preview paths and lines, plan steps, the text of a
//! failed prompt, option IDs, review notes' places and the session title carry
//! the canary through the whole flow of steps 1 to 3 at trace level (the
//! plugins, the page on a session, the history, the live updates, a failed
//! link, the stop key, the page switch, the reply box, a request and its
//! answer, a session that starts to wait, a terminal that runs and ends, a
//! draft that is saved and put back, the Terminal page that resumes a session,
//! shows a terminal whose title and picture carry the canary, closes it and
//! is asked by the leave guard, the route changes, the services leaving and
//! coming back), and no record carries it.

use std::path::Path;
use std::sync::Arc;

use gpui::TestAppContext;
use lectern_logging::testing;
use lectern_services::conversation::{
    CallKind, CallState, ConversationCommand, EditPreview, Entry, EntryBody, Link, LinkError,
    NoteLocation, OptionId, OptionKind, PermissionDetail, PermissionId, PermissionOption,
    PermissionRequest, PlanStep, PreviewRow, PromptOrigin, RowKind, StepState, Unavailable,
};
use lectern_services::session::Timestamp;
use lectern_services::terminal::{LaunchError, RunState};
use lectern_services::workspace::LeaveScope;
use lectern_terminal_view::testing::frame as grid;

use crate::plugin::LIVE_ID;
use crate::testing::{
    Rig, agent_text, entry_key, folder, passage, prompt, session, snapshot, summary, tool,
    with_entries,
};

use super::reply::{focus_composer, push};
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
    snap.link = Link::Working;
    snap
}

/// A working session that waits for two answers: a command in a folder of its
/// own and an edit, both with the canary in every text and option ID.
fn asking(generation: u64) -> lectern_services::conversation::ConversationSnapshot {
    let option = |kind| PermissionOption {
        id: OptionId(Arc::from(format!("option-{CANARY}"))),
        kind,
    };
    let mut snap = loaded(generation);
    snap.link = Link::Working;
    snap.can_prompt = Err(Unavailable::Busy);
    snap.permissions = Arc::from(vec![
        PermissionRequest {
            id: PermissionId(1),
            kind: CallKind::Execute,
            title: Arc::from(format!("title {CANARY}")),
            detail: PermissionDetail::Command {
                text: Arc::from(format!("rm -rf {CANARY}")),
                workdir: Some(Arc::from(Path::new(&format!("/work/{CANARY}")))),
            },
            options: Arc::from(vec![
                option(OptionKind::AllowOnce),
                option(OptionKind::RejectOnce),
            ]),
            asked_at: Timestamp(5),
        },
        PermissionRequest {
            id: PermissionId(2),
            kind: CallKind::Edit,
            title: Arc::from(format!("edit {CANARY}")),
            detail: PermissionDetail::Edits(Arc::from(vec![EditPreview {
                path: Arc::from(Path::new(&format!("/work/app/{CANARY}.rs"))),
                new_file: false,
                added: 1,
                removed: 0,
                rows: Arc::from(vec![PreviewRow {
                    kind: RowKind::Added,
                    text: Arc::from(format!("+ {CANARY}")),
                }]),
                omitted: 0,
            }])),
            options: Arc::from(vec![option(OptionKind::AllowOnce)]),
            asked_at: Timestamp(6),
        },
    ]);
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
    rig.activate_terminal();
    rig.conversations.set(loaded(1));
    rig.review(&key);
    let (host, vcx) = window(cx, &rig);
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
        let tab = tab_of(&host, vcx);
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

    // The reply box: an empty send is skipped, a draft with the canary is
    // sent, and the provider takes the turn.
    rig.conversations.respond({
        let key = key.clone();
        move |provider, _, command| match command {
            ConversationCommand::Prompt { .. } => {
                let mut snap = loaded(6);
                snap.session = key.clone();
                snap.link = Link::Connecting;
                snap.can_prompt = Err(Unavailable::Busy);
                provider.set(snap);
            }
            ConversationCommand::Disconnect => {
                let mut snap = loaded(20);
                snap.session = key.clone();
                snap.link = Link::None;
                snap.can_prompt = Ok(());
                provider.set(snap);
            }
            ConversationCommand::Answer { request, .. } => {
                let mut snap = asking(8);
                let left: Vec<PermissionRequest> = snap
                    .permissions
                    .iter()
                    .filter(|asked| asked.id.0 > request.0)
                    .cloned()
                    .collect();
                snap.permissions = Arc::from(left);
                provider.set(snap);
            }
            _ => {}
        }
    });
    let mut idle = loaded(5);
    idle.link = Link::Idle;
    idle.can_prompt = Ok(());
    push(&rig, vcx, idle);
    focus_composer(&host, vcx);
    vcx.simulate_keystrokes("cmd-enter");
    vcx.simulate_input(&format!("draft {CANARY}"));
    vcx.run_until_parked();
    vcx.simulate_keystrokes("cmd-enter");
    frame(vcx);

    // The session starts to wait: the page comes up, the requests are shown
    // and answered one after the other.
    rig.conversations.set_waiting(vec![key.clone()]);
    rig.waiting_changed();
    push(&rig, vcx, asking(7));
    click(vcx, "conversation.permission.more");
    click(
        vcx,
        "conversation.permission.item[1].option.item[allow-once]",
    );
    frame(vcx);
    click(
        vcx,
        "conversation.permission.item[2].option.item[allow-once]",
    );
    rig.conversations.set_waiting(vec![]);
    rig.waiting_changed();
    frame(vcx);

    // A terminal runs the session and ends; the box comes back.
    let mut idle = loaded(9);
    idle.link = Link::Idle;
    idle.can_prompt = Ok(());
    push(&rig, vcx, idle);
    for state in [
        RunState::Starting,
        RunState::Running { pid: 41 },
        RunState::Exited {
            code: 0,
            signaled: false,
        },
    ] {
        rig.terminals.set(&key, state);
        rig.terminals_listed();
        frame(vcx);
    }

    // Step 3: the Terminal page. The session is idle, so a resume
    // disconnects first; the first attempt is refused (too many terminals),
    // the second starts. The terminal's title and picture carry the canary.
    click(vcx, "conversation.pages.item[terminal]");
    frame(vcx);
    rig.terminals.refuse_next(LaunchError::TooMany);
    click(vcx, "conversation.terminal.resume");
    frame(vcx);
    click(vcx, "conversation.terminal.resume");
    frame(vcx);
    let id = rig.terminals.id_of(&key).expect("the terminal was opened");
    rig.terminals.set_title(id, &format!("title {CANARY}"));
    rig.terminals.set_frame(
        id,
        grid(&[&format!("$ claude --resume {CANARY}"), "output"]).build(),
    );
    rig.terminals.set(&key, RunState::Running { pid: 77 });
    rig.terminals_listed();
    rig.terminal_frame(id, 2);
    frame(vcx);
    // The page is built again on the Terminal page, which the session was
    // last on.
    rig.review(&session(2));
    frame(vcx);
    rig.review(&key);
    frame(vcx);
    click(vcx, "conversation.terminal.close");
    frame(vcx);
    // The guard asks about another terminal that runs, and is answered.
    rig.terminals.set_in(
        &session(2),
        folder("/work/app"),
        RunState::Running { pid: 78 },
    );
    let guard = rig.contributions.guards().remove(0);
    guard.ask(LeaveScope::App, Box::new(|_| {}));
    vcx.run_until_parked();
    vcx.simulate_prompt_answer("End Terminals");
    vcx.run_until_parked();
    click(vcx, "conversation.pages.item[messages]");
    frame(vcx);

    // A draft with the canary is kept when the page goes and is put back
    // when it comes again.
    focus_composer(&host, vcx);
    vcx.simulate_input(&format!("unsent {CANARY}"));
    vcx.run_until_parked();
    rig.review(&session(2));
    frame(vcx);
    rig.review(&key);
    frame(vcx);

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
    // Step 2: the reply box, the answers, the waiting sessions and the draft.
    let sent = find("prompt sent");
    for field in ["\"bytes\":", "\"connects\":", "\"session\":"] {
        assert!(sent.contains(field), "{field} in {sent}");
    }
    let answered = find("permission answered");
    for field in [
        "\"option\":\"allow_once\"",
        "\"kind\":\"execute\"",
        "\"waited_ms\":",
        "\"session\":",
    ] {
        assert!(answered.contains(field), "{field} in {answered}");
    }
    assert!(
        lines
            .iter()
            .any(|line| line.contains("\"msg\":\"permission answered\"")
                && line.contains("\"kind\":\"edit\"")),
        "the second request is answered too: {lines:#?}"
    );
    let selected = find("conversation tab selected for a request");
    assert!(selected.contains("\"pending\":"), "{selected}");
    assert!(selected.contains("\"session\":"), "{selected}");
    let waiting = find("waiting changed");
    for field in ["\"sessions\":", "\"new\":"] {
        assert!(waiting.contains(field), "{field} in {waiting}");
    }
    // Step 3: the Terminal page and the leave guard.
    let resumes: Vec<&String> = lines
        .iter()
        .filter(|line| line.contains("\"msg\":\"terminal resume requested\""))
        .collect();
    assert_eq!(resumes.len(), 2, "{resumes:#?}");
    assert!(
        resumes[0].contains("\"outcome\":\"too_many\""),
        "{}",
        resumes[0]
    );
    assert!(
        resumes[1].contains("\"outcome\":\"started\""),
        "{}",
        resumes[1]
    );
    // The first one disconnected the idle link; the second found none.
    assert!(
        resumes[0].contains("\"disconnected\":true"),
        "{}",
        resumes[0]
    );
    assert!(
        resumes[1].contains("\"disconnected\":false"),
        "{}",
        resumes[1]
    );
    for resume in &resumes {
        assert!(resume.contains("\"session\":"), "{resume}");
    }
    let closed = find("terminal closed");
    assert!(closed.contains("\"state\":\"running\""), "{closed}");
    let left = find("terminal leave answered");
    for field in ["\"scope\":\"app\"", "\"running\":1", "\"answer\":\"end\""] {
        assert!(left.contains(field), "{field} in {left}");
    }
    assert!(
        lines.iter().any(|line| {
            line.contains("\"msg\":\"conversation shown\"")
                && line.contains("\"page\":\"terminal\"")
        }),
        "the page is shown on the Terminal page: {lines:#?}"
    );
    let restored = find("draft restored");
    assert!(restored.contains("\"bytes\":"), "{restored}");
    assert!(restored.contains("\"session\":"), "{restored}");
    let skipped = find("send skipped");
    assert!(skipped.contains("\"reason\":\"empty\""), "{skipped}");
    // Every record about the session carries its tag, not its ID.
    assert!(
        find("conversation shown").contains("\"session\":"),
        "the session scope is on the record"
    );
}
