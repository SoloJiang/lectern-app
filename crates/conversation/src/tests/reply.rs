//! The reply box, Stop, the permission cards and the drafts in GPUI's test
//! windows (docs/design/conversation.md「测试」的 `#[gpui::test]`, step 2 of
//! 「验收」). No test opens a real window.

use std::path::Path;
use std::sync::Arc;

use gpui::{Entity, TestAppContext, VisualTestContext};
use lectern_kernel::Name;
use lectern_services::conversation::{
    AcpProgram, CallKind, ConversationCommand, ConversationSnapshot, EditPreview, Link, OptionId,
    OptionKind, PermissionDetail, PermissionId, PermissionOption, PermissionRequest, PreviewRow,
    PromptOrigin, RowKind, Unavailable,
};
use lectern_services::session::Timestamp;
use lectern_services::terminal::{FailReason, RunState};
use lectern_services::workspace::WorkspaceCommand;

use crate::actions::{AllowOnce, RejectAlways, Send};
use crate::drive;
use crate::testing::{Rig, prompt, session, snapshot, summary, with_entries};
use crate::view::Page;

use super::stage::*;

const CANARY: &str = "reply-canary-5d02b9c1";

// ----------------------------------------------------------------- helpers

/// Sets a snapshot in the provider, tells the page and lets it settle.
pub(super) fn push(rig: &Rig, vcx: &mut VisualTestContext, snap: ConversationSnapshot) {
    let (key, generation) = (snap.session.clone(), snap.generation);
    rig.conversations.set(snap);
    rig.changed(&key, generation);
    frame(vcx);
}

/// An idle session whose box can send.
fn idle(key: &lectern_services::session::SessionKey, generation: u64) -> ConversationSnapshot {
    let mut snap = with_entries(key, generation, vec![prompt("h:0:0", "an earlier prompt")]);
    snap.link = Link::Idle;
    snap
}

/// The state a provider swaps in once it has taken a prompt.
fn connecting(
    key: &lectern_services::session::SessionKey,
    generation: u64,
) -> ConversationSnapshot {
    let mut snap = with_entries(
        key,
        generation,
        vec![
            prompt("h:0:0", "an earlier prompt"),
            prompt("l:1:0", "the prompt just sent"),
        ],
    );
    snap.link = Link::Connecting;
    snap.can_prompt = Err(Unavailable::Busy);
    snap
}

pub(super) fn focus_composer(host: &Entity<Host>, vcx: &mut VisualTestContext) {
    let model = messages_of(host, vcx).expect("a page");
    let handle = vcx.update(|_, cx| model.read(cx).composer().read(cx).focus_handle());
    vcx.update(|window, cx| window.focus(&handle, cx));
}

fn composer_text(host: &Entity<Host>, vcx: &mut VisualTestContext) -> String {
    let model = messages_of(host, vcx).expect("a page");
    vcx.update(|_, cx| model.read(cx).composer().read(cx).text().to_string())
}

/// The line of the tree that starts with the node `id`.
fn line<'a>(shown: &'a str, id: &str) -> &'a str {
    shown
        .lines()
        .map(str::trim_start)
        .find(|line| line.starts_with(&format!("{id} ")) || *line == id)
        .unwrap_or_else(|| panic!("{id} is not in the tree:\n{shown}"))
}

fn has_node(shown: &str, id: &str) -> bool {
    shown
        .lines()
        .map(str::trim_start)
        .any(|line| line.starts_with(&format!("{id} ")) || line == id)
}

fn position(shown: &str, needle: &str) -> usize {
    shown
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} is not in the tree:\n{shown}"))
}

fn option(id: &str, kind: OptionKind) -> PermissionOption {
    PermissionOption {
        id: OptionId(Arc::from(id)),
        kind,
    }
}

/// The four kinds, in an order that is not the display order, with option IDs
/// that carry the canary.
fn scrambled_options() -> Vec<PermissionOption> {
    vec![
        option(&format!("{CANARY}-reject-always"), OptionKind::RejectAlways),
        option(&format!("{CANARY}-allow-once"), OptionKind::AllowOnce),
        option(&format!("{CANARY}-reject-once"), OptionKind::RejectOnce),
        option(&format!("{CANARY}-allow-always"), OptionKind::AllowAlways),
    ]
}

fn request(
    id: u64,
    kind: CallKind,
    detail: PermissionDetail,
    options: Vec<PermissionOption>,
) -> PermissionRequest {
    PermissionRequest {
        id: PermissionId(id),
        kind,
        title: Arc::from(format!("title {id}")),
        detail,
        options: Arc::from(options),
        asked_at: Timestamp(1),
    }
}

fn command(text: &str, workdir: Option<&str>) -> PermissionDetail {
    PermissionDetail::Command {
        text: Arc::from(text),
        workdir: workdir.map(|dir| Arc::from(Path::new(dir))),
    }
}

/// A working session that waits for answers to `requests`.
fn waiting(
    key: &lectern_services::session::SessionKey,
    generation: u64,
    requests: Vec<PermissionRequest>,
) -> ConversationSnapshot {
    let mut snap = with_entries(key, generation, vec![prompt("h:0:0", "go")]);
    snap.link = Link::Working;
    snap.can_prompt = Err(Unavailable::Busy);
    snap.permissions = Arc::from(requests);
    snap
}

// ------------------------------------------------------------------- sending

#[gpui::test]
fn send_with_cmd_enter_sends_a_prompt(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(idle(&key, 1));
    // The provider swaps the snapshot in before `command` returns.
    let responder_key = key.clone();
    rig.conversations.respond(move |provider, _, command| {
        if matches!(command, ConversationCommand::Prompt { .. }) {
            provider.set(connecting(&responder_key, 2));
        }
    });
    let (host, vcx) = open(cx, &rig, &key);
    focus_composer(&host, vcx);
    vcx.simulate_input("  hello there  ");
    vcx.run_until_parked();
    assert_eq!(composer_text(&host, vcx), "  hello there  ");

    vcx.simulate_keystrokes("cmd-enter");
    vcx.run_until_parked();
    assert_eq!(
        rig.conversations.commands(),
        [(
            key.clone(),
            ConversationCommand::Prompt {
                text: Arc::from("hello there"),
                origin: PromptOrigin::User,
            }
        )],
        "the text is sent without the blanks around it"
    );
    // The link is `Connecting`: the box is emptied and the list has the prompt.
    frame(vcx);
    assert_eq!(composer_text(&host, vcx), "");
    let shown = tree(vcx, all);
    assert!(
        shown.contains("label=[conversation-status-connecting] link=connecting"),
        "{shown}"
    );
    assert!(has_node(&shown, "conversation.list.item[l:1:0]"), "{shown}");
}

#[gpui::test]
fn send_ignores_blank_text_and_keeps_text_the_provider_refused(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(idle(&key, 1));
    let (host, vcx) = open(cx, &rig, &key);
    focus_composer(&host, vcx);

    // Only blanks: nothing is sent, the Send button is off.
    vcx.simulate_input("   ");
    vcx.simulate_keystrokes("cmd-enter");
    vcx.run_until_parked();
    assert!(rig.conversations.commands().is_empty());
    frame(vcx);
    let shown = tree(vcx, all);
    assert!(
        line(&shown, "conversation.composer.send").contains("disabled"),
        "{shown}"
    );

    // Text: the Send button is on, and a click sends. The provider does not
    // start a turn (no responder): the text stays.
    vcx.simulate_input("do it");
    frame(vcx);
    let shown = tree(vcx, all);
    assert!(
        !line(&shown, "conversation.composer.send").contains("disabled"),
        "{shown}"
    );
    click(vcx, "conversation.composer.send");
    assert_eq!(rig.conversations.commands().len(), 1);
    frame(vcx);
    assert_eq!(composer_text(&host, vcx), "   do it");
}

#[gpui::test]
fn the_send_action_sends_like_the_key(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(idle(&key, 1));
    let responder_key = key.clone();
    rig.conversations.respond(move |provider, _, command| {
        if matches!(command, ConversationCommand::Prompt { .. }) {
            provider.set(connecting(&responder_key, 2));
        }
    });
    let (host, vcx) = open(cx, &rig, &key);
    focus_composer(&host, vcx);
    vcx.simulate_input("by name");
    vcx.run_until_parked();
    vcx.dispatch_action(Send);
    vcx.run_until_parked();
    assert_eq!(rig.conversations.commands().len(), 1);
    frame(vcx);
    assert_eq!(composer_text(&host, vcx), "");
}

#[gpui::test]
fn composer_shows_why_it_cannot_send(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(idle(&key, 1));
    let (_host, vcx) = open(cx, &rig, &key);
    // A session that can be continued: no reason, an enabled box.
    let shown = tree(vcx, all);
    assert!(!has_node(&shown, "conversation.composer.notice"), "{shown}");
    assert!(
        !line(&shown, "conversation.composer.field").contains("disabled"),
        "{shown}"
    );

    let cases = [
        (
            Unavailable::NotResumable,
            "conversation-unavailable-not-resumable",
        ),
        (
            Unavailable::OutsideProject,
            "conversation-unavailable-outside-project",
        ),
        (
            Unavailable::NotInstalled(AcpProgram::ClaudeCode),
            "conversation-unavailable-not-installed",
        ),
        (
            Unavailable::Unsupported,
            "conversation-unavailable-unsupported",
        ),
        (Unavailable::TooMany, "conversation-unavailable-too-many"),
    ];
    for (n, (why, label)) in cases.into_iter().enumerate() {
        let mut snap = idle(&key, 2 + n as u64);
        snap.can_prompt = Err(why);
        push(&rig, vcx, snap);
        let shown = tree(vcx, all);
        assert!(
            shown.contains(&format!(
                "conversation.composer.notice Text label=[{label}]"
            )),
            "{label}: {shown}"
        );
        assert!(
            line(&shown, "conversation.composer.field").contains("disabled"),
            "{label}: {shown}"
        );
        assert!(
            line(&shown, "conversation.composer.send").contains("disabled"),
            "{label}: {shown}"
        );
    }

    // Busy: the status line says so, the box takes text, Send is off.
    let mut busy = idle(&key, 20);
    busy.link = Link::Stopping;
    busy.can_prompt = Err(Unavailable::Busy);
    push(&rig, vcx, busy);
    let shown = tree(vcx, all);
    assert!(!has_node(&shown, "conversation.composer.notice"), "{shown}");
    assert!(
        !line(&shown, "conversation.composer.field").contains("disabled"),
        "{shown}"
    );
    assert!(
        line(&shown, "conversation.composer.send").contains("disabled"),
        "{shown}"
    );
}

#[gpui::test]
fn the_stop_button_replaces_send_while_a_turn_runs(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(idle(&key, 1));
    let responder_key = key.clone();
    rig.conversations.respond(move |provider, _, command| {
        if matches!(command, ConversationCommand::Cancel) {
            let mut snap = idle(&responder_key, 3);
            snap.link = Link::Stopping;
            snap.can_prompt = Err(Unavailable::Busy);
            provider.set(snap);
        }
    });
    let (_host, vcx) = open(cx, &rig, &key);
    let shown = tree(vcx, all);
    assert!(has_node(&shown, "conversation.composer.send"), "{shown}");
    assert!(!has_node(&shown, "conversation.composer.stop"), "{shown}");

    let mut working = idle(&key, 2);
    working.link = Link::Working;
    working.can_prompt = Err(Unavailable::Busy);
    push(&rig, vcx, working);
    let shown = tree(vcx, all);
    assert!(!has_node(&shown, "conversation.composer.send"), "{shown}");
    assert!(
        !line(&shown, "conversation.composer.stop").contains("disabled"),
        "{shown}"
    );
    click(vcx, "conversation.composer.stop");
    assert_eq!(
        rig.conversations.commands(),
        [(key.clone(), ConversationCommand::Cancel)]
    );
    frame(vcx);
    // Stopping: Send again, off.
    let shown = tree(vcx, all);
    assert!(
        line(&shown, "conversation.composer.send").contains("disabled"),
        "{shown}"
    );
}

// ----------------------------------------------------------- the terminal

#[gpui::test]
fn composer_is_replaced_while_the_terminal_drives(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(idle(&key, 1));
    let (host, vcx) = open(cx, &rig, &key);
    let shown = tree(vcx, all);
    assert!(has_node(&shown, "conversation.composer.field"), "{shown}");

    for state in [RunState::Starting, RunState::Running { pid: 41 }] {
        rig.terminals.set(&key, state);
        rig.terminals_listed();
        frame(vcx);
        let shown = tree(vcx, all);
        assert!(
            shown.contains("conversation.composer.notice Text label=[conversation-in-terminal]"),
            "{shown}"
        );
        assert!(!has_node(&shown, "conversation.composer.field"), "{shown}");
        assert!(!has_node(&shown, "conversation.composer.send"), "{shown}");
        assert!(
            shown.contains("label=[conversation-status-terminal] link=idle"),
            "{shown}"
        );
        assert!(
            shown.contains(
                "conversation.composer.notice.action Button label=[conversation-show-terminal]"
            ),
            "{shown}"
        );
    }

    // "Show Terminal" goes to the Terminal page.
    click(vcx, "conversation.composer.notice.action");
    frame(vcx);
    let tab = tab_of(&host, vcx);
    assert_eq!(vcx.update(|_, cx| tab.read(cx).page()), Page::Terminal);
    click(vcx, "conversation.pages.item[messages]");
    frame(vcx);

    // The terminal ends, or fails to start: the box is back.
    for state in [
        RunState::Exited {
            code: 0,
            signaled: false,
        },
        RunState::Exited {
            code: 9,
            signaled: true,
        },
        RunState::Failed(FailReason::CwdMissing),
    ] {
        rig.terminals.set(&key, RunState::Running { pid: 41 });
        rig.terminals_listed();
        frame(vcx);
        assert!(!has_node(&tree(vcx, all), "conversation.composer.field"));
        rig.terminals.set(&key, state);
        rig.terminals_listed();
        frame(vcx);
        let shown = tree(vcx, all);
        assert!(has_node(&shown, "conversation.composer.field"), "{shown}");
        assert!(!has_node(&shown, "conversation.composer.notice"), "{shown}");
        assert!(
            shown.contains("label=[conversation-status-idle]"),
            "{shown}"
        );
    }

    // The terminal service leaving frees the box too.
    rig.terminals.set(&key, RunState::Running { pid: 41 });
    rig.terminals_listed();
    frame(vcx);
    assert!(!has_node(&tree(vcx, all), "conversation.composer.field"));
    rig.deactivate_terminal();
    frame(vcx);
    assert!(has_node(&tree(vcx, all), "conversation.composer.field"));
}

#[gpui::test]
fn a_terminal_that_drives_stops_the_send_key(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(idle(&key, 1));
    let (host, vcx) = open(cx, &rig, &key);
    focus_composer(&host, vcx);
    vcx.simulate_input("typed before");
    vcx.run_until_parked();
    // The terminal starts after the text was written; the action still must
    // not send. The text is kept.
    rig.terminals.set(&key, RunState::Starting);
    vcx.dispatch_action(Send);
    vcx.run_until_parked();
    assert!(rig.conversations.commands().is_empty());
    assert_eq!(composer_text(&host, vcx), "typed before");
}

// -------------------------------------------------------------- elsewhere

#[gpui::test]
fn the_page_hints_that_the_session_may_run_elsewhere(cx: &mut TestAppContext) {
    let mut recent = summary(&session(1));
    recent.open_turn = true;
    recent.last_activity = Timestamp(drive::now().0 - 30_000);
    let mut old = summary(&session(2));
    old.open_turn = true;
    old.last_activity = Timestamp(drive::now().0 - 10 * 60_000);
    cx.executor().allow_parking();
    let rig = {
        cx.update(|cx| {
            lectern_ui::init_fixed(
                cx,
                lectern_i18n::Strings::of(lectern_i18n::Locale::En),
                lectern_ui::Appearance::Light,
                false,
            );
        });
        Rig::with_sessions(vec![recent, old])
    };
    let (one, two) = (session(1), session(2));
    rig.conversations.set(idle(&one, 1));
    rig.conversations.set(idle(&two, 1));
    let mut history = snapshot(&one, 2);
    history.link = Link::None;
    rig.conversations.set(history);
    let mut history_two = snapshot(&two, 2);
    history_two.link = Link::None;
    rig.conversations.set(history_two);
    let (_host, vcx) = open(cx, &rig, &one);
    let shown = tree(vcx, all);
    assert!(
        shown.contains("conversation.elsewhere Text label=[conversation-elsewhere]"),
        "{shown}"
    );
    // It is a hint: the box takes a send all the same.
    assert!(
        !line(&shown, "conversation.composer.field").contains("disabled"),
        "{shown}"
    );

    // A terminal that drives the session, or a connection, turns it off.
    rig.terminals.set(&one, RunState::Running { pid: 5 });
    rig.terminals_listed();
    frame(vcx);
    assert!(!has_node(&tree(vcx, all), "conversation.elsewhere"));
    rig.terminals.set(
        &one,
        RunState::Exited {
            code: 0,
            signaled: false,
        },
    );
    rig.terminals_listed();
    frame(vcx);
    assert!(has_node(&tree(vcx, all), "conversation.elsewhere"));
    let mut connected = idle(&one, 3);
    connected.link = Link::Idle;
    push(&rig, vcx, connected);
    assert!(!has_node(&tree(vcx, all), "conversation.elsewhere"));

    // Quiet for ten minutes: no hint.
    rig.review(&two);
    frame(vcx);
    assert!(!has_node(&tree(vcx, all), "conversation.elsewhere"));
}

// ------------------------------------------------------ permission cards

#[gpui::test]
fn permission_card_uses_kind_labels(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(waiting(
        &key,
        1,
        vec![
            request(
                1,
                CallKind::Execute,
                command("cargo test", None),
                scrambled_options(),
            ),
            // Only two kinds: only two buttons.
            request(
                2,
                CallKind::Other,
                PermissionDetail::None,
                vec![
                    option("x", OptionKind::RejectOnce),
                    option("y", OptionKind::AllowOnce),
                ],
            ),
        ],
    ));
    let (_host, vcx) = open(cx, &rig, &key);
    let shown = tree(vcx, all);
    assert!(
        !shown.contains(CANARY),
        "no option ID reaches the tree:\n{shown}"
    );
    assert!(
        shown.contains(
            "conversation.permission.item[1] Dialog label=[conversation-permission-execute] kind=execute detail=command"
        ),
        "{shown}"
    );
    let order = [
        "conversation.permission.item[1].option.item[allow-once] Button label=[conversation-permission-allow-once]",
        "conversation.permission.item[1].option.item[allow-always] Button label=[conversation-permission-allow-always]",
        "conversation.permission.item[1].option.item[reject-once] Button label=[conversation-permission-reject-once]",
        "conversation.permission.item[1].option.item[reject-always] Button label=[conversation-permission-reject-always]",
    ];
    let at: Vec<usize> = order.iter().map(|id| position(&shown, id)).collect();
    assert!(at.windows(2).all(|pair| pair[0] < pair[1]), "{shown}");
    assert!(
        shown.contains("label=[conversation-status-waiting]"),
        "{shown}"
    );
    // Only the oldest request is whole; the other is behind a line.
    assert!(
        !has_node(&shown, "conversation.permission.item[2]") || {
            // Closed, its region is not drawn.
            !line(&shown, "conversation.permission.rest").contains("expanded=true")
        }
    );
    assert!(
        shown.contains("conversation.permission.more Button label=[conversation-permission-more] expanded=false"),
        "{shown}"
    );

    // ↵ is nobody's default: no option is chosen by it.
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(rig.conversations.commands().is_empty());

    // Opening the line shows the second request, with its two buttons.
    click(vcx, "conversation.permission.more");
    frame(vcx);
    frame(vcx);
    let shown = tree(vcx, all);
    assert!(
        shown.contains(
            "conversation.permission.more Button label=[conversation-permission-more] expanded=true"
        ),
        "{shown}"
    );
    assert!(
        shown.contains(
            "conversation.permission.item[2] Dialog label=[conversation-permission-other] kind=other detail=none"
        ),
        "{shown}"
    );
    assert!(has_node(
        &shown,
        "conversation.permission.item[2].option.item[allow-once]"
    ));
    assert!(has_node(
        &shown,
        "conversation.permission.item[2].option.item[reject-once]"
    ));
    assert!(!has_node(
        &shown,
        "conversation.permission.item[2].option.item[allow-always]"
    ));
    assert!(!shown.contains(CANARY));
}

#[gpui::test]
fn permission_command_and_edits_are_shown(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    let edit = |path: &str, new_file: bool, added: u32, removed: u32, omitted: u32| EditPreview {
        path: Arc::from(Path::new(path)),
        new_file,
        added,
        removed,
        rows: Arc::from(vec![
            PreviewRow {
                kind: RowKind::Context,
                text: Arc::from("fn a() {"),
            },
            PreviewRow {
                kind: RowKind::Removed,
                text: Arc::from("    old();"),
            },
            PreviewRow {
                kind: RowKind::Added,
                text: Arc::from("    new();"),
            },
        ]),
        omitted,
    };

    // A command in the session's folder, then one somewhere else.
    rig.conversations.set(waiting(
        &key,
        1,
        vec![request(
            1,
            CallKind::Execute,
            command("make -j4\nmake test", Some("/work/app")),
            scrambled_options(),
        )],
    ));
    let (_host, vcx) = open(cx, &rig, &key);
    let shown = tree(vcx, all);
    assert!(
        shown.contains(
            "conversation.permission.item[1].command Text label=\"make -j4\\nmake test\""
        ),
        "{shown}"
    );
    assert!(
        !has_node(&shown, "conversation.permission.item[1].workdir"),
        "the folder of the session is not repeated:\n{shown}"
    );
    push(
        &rig,
        vcx,
        waiting(
            &key,
            2,
            vec![request(
                1,
                CallKind::Execute,
                command("ls", Some("/work/app/sub/dir")),
                scrambled_options(),
            )],
        ),
    );
    let shown = tree(vcx, all);
    let place = line(&shown, "conversation.permission.item[1].workdir");
    assert!(place.contains("sub/dir"), "relative to the folder: {place}");
    assert!(!place.contains("/work/app"), "{place}");
    push(
        &rig,
        vcx,
        waiting(
            &key,
            3,
            vec![request(
                1,
                CallKind::Execute,
                command("ls", Some("/elsewhere")),
                scrambled_options(),
            )],
        ),
    );
    let shown = tree(vcx, all);
    assert!(
        line(&shown, "conversation.permission.item[1].workdir").contains("/elsewhere"),
        "{shown}"
    );

    // Edits: a heading with the number of files, each file with its counts
    // and rows, and the lines that were cut.
    push(
        &rig,
        vcx,
        waiting(
            &key,
            4,
            vec![request(
                1,
                CallKind::Edit,
                PermissionDetail::Edits(Arc::from(vec![
                    edit("/work/app/src/a.rs", false, 1, 1, 7),
                    edit("/work/app/src/b.rs", true, 3, 0, 0),
                ])),
                scrambled_options(),
            )],
        ),
    );
    let shown = tree(vcx, all);
    assert!(
        shown.contains(
            "conversation.permission.item[1] Dialog label=[conversation-permission-edit] kind=edit detail=edits"
        ),
        "{shown}"
    );
    let first = line(
        &shown,
        "conversation.permission.item[1].edit.item[/work/app/src/a.rs]",
    );
    assert!(
        first.contains("label=\"src/a.rs\" added=1 removed=1 new=false rows=3 omitted=7"),
        "{first}"
    );
    let second = line(
        &shown,
        "conversation.permission.item[1].edit.item[/work/app/src/b.rs]",
    );
    assert!(second.contains("added=3 removed=0 new=true"), "{second}");
    for (row, text, kind) in [
        (0, "fn a() {", "context"),
        (1, "    old();", "removed"),
        (2, "    new();", "added"),
    ] {
        let id = format!(
            "conversation.permission.item[1].edit.item[/work/app/src/a.rs].row.item[{row}]"
        );
        let found = line(&shown, &id);
        assert!(
            found.contains(&format!("label={text:?} kind={kind}")),
            "{found}"
        );
    }

    // No details: the title and the line that says there are none.
    push(
        &rig,
        vcx,
        waiting(
            &key,
            5,
            vec![request(
                1,
                CallKind::Other,
                PermissionDetail::None,
                scrambled_options(),
            )],
        ),
    );
    let shown = tree(vcx, all);
    assert!(
        shown.contains(
            "conversation.permission.item[1].no-detail Text label=[conversation-permission-no-detail]"
        ),
        "{shown}"
    );
    assert!(
        shown.contains("label=[conversation-permission-other] kind=other detail=none"),
        "{shown}"
    );
}

#[gpui::test]
fn answer_removes_the_card(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    let both = || {
        vec![
            request(
                1,
                CallKind::Execute,
                command("first", None),
                vec![
                    option("answer-allow-1", OptionKind::AllowOnce),
                    option("answer-reject-1", OptionKind::RejectOnce),
                ],
            ),
            request(
                2,
                CallKind::Execute,
                command("second", None),
                vec![option("answer-allow-2", OptionKind::AllowOnce)],
            ),
        ]
    };
    rig.conversations.set(waiting(&key, 1, both()));
    // The provider takes the request out of the snapshot before `command`
    // returns.
    let responder_key = key.clone();
    rig.conversations.respond(move |provider, _, command| {
        if let ConversationCommand::Answer { request, .. } = command {
            let left: Vec<PermissionRequest> = both()
                .into_iter()
                .filter(|asked| asked.id != *request)
                .collect();
            let mut snap = waiting(&responder_key, 2, left);
            if snap.permissions.is_empty() {
                snap.can_prompt = Ok(());
            }
            provider.set(snap);
        }
    });
    let (_host, vcx) = open(cx, &rig, &key);
    let shown = tree(vcx, all);
    assert!(
        has_node(&shown, "conversation.permission.item[1]"),
        "{shown}"
    );
    assert!(
        shown.contains("conversation.permission.more Button label=[conversation-permission-more]"),
        "{shown}"
    );

    click(
        vcx,
        "conversation.permission.item[1].option.item[allow-once]",
    );
    assert_eq!(
        rig.conversations.commands(),
        [(
            key.clone(),
            ConversationCommand::Answer {
                request: PermissionId(1),
                option: OptionId(Arc::from("answer-allow-1")),
            }
        )]
    );
    frame(vcx);
    let shown = tree(vcx, all);
    assert!(
        !has_node(&shown, "conversation.permission.item[1]"),
        "{shown}"
    );
    // The second request is the whole card now, and the line is gone.
    assert!(
        has_node(&shown, "conversation.permission.item[2]"),
        "{shown}"
    );
    assert!(
        has_node(
            &shown,
            "conversation.permission.item[2].option.item[allow-once]"
        ),
        "{shown}"
    );
    assert!(!has_node(&shown, "conversation.permission.more"), "{shown}");

    click(
        vcx,
        "conversation.permission.item[2].option.item[allow-once]",
    );
    frame(vcx);
    let shown = tree(vcx, all);
    assert!(
        !has_node(&shown, "conversation.permission.item[2]"),
        "{shown}"
    );
    assert_eq!(rig.conversations.commands().len(), 2);
}

#[gpui::test]
fn answer_actions_answer_the_oldest_request(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(waiting(
        &key,
        1,
        vec![
            request(
                7,
                CallKind::Execute,
                command("one", None),
                vec![
                    option("allow-7", OptionKind::AllowOnce),
                    option("reject-always-7", OptionKind::RejectAlways),
                ],
            ),
            request(
                8,
                CallKind::Execute,
                command("two", None),
                vec![option("allow-8", OptionKind::AllowOnce)],
            ),
        ],
    ));
    let (host, vcx) = open(cx, &rig, &key);
    let focus = {
        let tab = tab_of(&host, vcx);
        vcx.update(|_, cx| tab.read(cx).focus_handle())
    };
    vcx.update(|window, cx| window.focus(&focus, cx));
    // A kind the oldest request does not offer is not answered.
    vcx.dispatch_action(crate::actions::AllowAlways);
    vcx.run_until_parked();
    assert!(rig.conversations.commands().is_empty());
    vcx.dispatch_action(RejectAlways);
    vcx.run_until_parked();
    vcx.dispatch_action(AllowOnce);
    vcx.run_until_parked();
    let commands = rig.conversations.commands();
    // The fake provider keeps the snapshot as it is, so both actions find
    // the request 7.
    assert_eq!(
        commands,
        [
            (
                key.clone(),
                ConversationCommand::Answer {
                    request: PermissionId(7),
                    option: OptionId(Arc::from("reject-always-7")),
                }
            ),
            (
                key.clone(),
                ConversationCommand::Answer {
                    request: PermissionId(7),
                    option: OptionId(Arc::from("allow-7")),
                }
            ),
        ]
    );
}

#[gpui::test]
fn a_request_that_arrives_while_the_page_shows_blinks_and_is_whole(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    let mut working = idle(&key, 1);
    working.link = Link::Working;
    working.can_prompt = Err(Unavailable::Busy);
    rig.conversations.set(working);
    let (_host, vcx) = open(cx, &rig, &key);
    assert!(!has_node(
        &tree(vcx, all),
        "conversation.permission.item[3]"
    ));
    push(
        &rig,
        vcx,
        waiting(
            &key,
            2,
            vec![request(
                3,
                CallKind::Execute,
                command("late", None),
                scrambled_options(),
            )],
        ),
    );
    let shown = tree(vcx, all);
    assert!(
        has_node(&shown, "conversation.permission.item[3]"),
        "{shown}"
    );
}

// ----------------------------------------------------------- the new request

#[gpui::test]
fn new_request_selects_the_tab(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let (key, other) = (session(1), session(2));
    rig.activate_main();
    rig.activate_live();
    rig.review(&key);
    let select = WorkspaceCommand::SelectInspectorTab(Name::Borrowed("conversation"));

    // Another session starts to wait: nothing for this page.
    rig.conversations.set_waiting(vec![other.clone()]);
    rig.waiting_changed();
    assert!(rig.contributions.dispatched().is_empty());

    // The session under review starts to wait: the page comes up, once.
    rig.conversations
        .set_waiting(vec![other.clone(), key.clone()]);
    rig.waiting_changed();
    assert_eq!(
        rig.contributions.dispatched(),
        std::slice::from_ref(&select)
    );
    rig.waiting_changed();
    assert_eq!(
        rig.contributions.dispatched().len(),
        1,
        "the same set is not a new request"
    );

    // It stops waiting and waits again: that is a new request.
    rig.conversations.set_waiting(vec![other.clone()]);
    rig.waiting_changed();
    assert_eq!(rig.contributions.dispatched().len(), 1);
    rig.conversations
        .set_waiting(vec![other.clone(), key.clone()]);
    rig.waiting_changed();
    assert_eq!(
        rig.contributions.dispatched(),
        [select.clone(), select.clone()]
    );

    // The screen is not the review of the session: nothing is selected.
    rig.conversations.set_waiting(vec![]);
    rig.waiting_changed();
    rig.home();
    rig.conversations.set_waiting(vec![key.clone()]);
    rig.waiting_changed();
    assert_eq!(rig.contributions.dispatched().len(), 2);
    // And the review of another session is not this one's either.
    rig.conversations.set_waiting(vec![]);
    rig.waiting_changed();
    rig.review(&other);
    rig.conversations.set_waiting(vec![key.clone()]);
    rig.waiting_changed();
    assert_eq!(rig.contributions.dispatched().len(), 2);
}

#[gpui::test]
fn sessions_already_waiting_when_the_provider_starts_are_not_new(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set_waiting(vec![key.clone()]);
    rig.activate_main();
    rig.activate_live();
    rig.review(&key);
    // The same set again: nothing changed for this page.
    rig.waiting_changed();
    assert!(rig.contributions.dispatched().is_empty());
}

// ---------------------------------------------------------------- drafts

#[gpui::test]
fn drafts_survive_hiding_the_inspector(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let (one, two) = (session(1), session(2));
    rig.conversations.set(idle(&one, 1));
    rig.conversations.set(idle(&two, 1));
    let (host, vcx) = open(cx, &rig, &one);
    focus_composer(&host, vcx);
    vcx.simulate_input("draft one");
    vcx.run_until_parked();

    // The page switch keeps the page, and with it the text.
    click(vcx, "conversation.pages.item[terminal]");
    frame(vcx);
    click(vcx, "conversation.pages.item[messages]");
    frame(vcx);
    assert_eq!(composer_text(&host, vcx), "draft one");

    // Another session: its own box, empty. The first page is gone.
    rig.review(&two);
    frame(vcx);
    assert_eq!(rig.conversations.alive(), 1);
    assert_eq!(composer_text(&host, vcx), "");
    focus_composer(&host, vcx);
    vcx.simulate_input("draft two");
    vcx.run_until_parked();

    // Leaving the review altogether, then coming back to each.
    rig.home();
    frame(vcx);
    assert!(messages_of(&host, vcx).is_none());
    rig.review(&one);
    frame(vcx);
    assert_eq!(composer_text(&host, vcx), "draft one");
    rig.review(&two);
    frame(vcx);
    assert_eq!(composer_text(&host, vcx), "draft two");

    // A sent text is not a draft: the box was emptied, nothing comes back.
    rig.conversations.respond({
        let key = two.clone();
        move |provider, _, command| {
            if matches!(command, ConversationCommand::Prompt { .. }) {
                provider.set(connecting(&key, 5));
            }
        }
    });
    focus_composer(&host, vcx);
    vcx.simulate_keystrokes("cmd-enter");
    frame(vcx);
    assert_eq!(composer_text(&host, vcx), "");
    rig.review(&one);
    frame(vcx);
    rig.review(&two);
    frame(vcx);
    assert_eq!(composer_text(&host, vcx), "");
}

#[gpui::test]
fn the_page_opens_on_the_page_the_session_was_last_on(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let (one, two) = (session(1), session(2));
    rig.conversations.set(idle(&one, 1));
    rig.conversations.set(idle(&two, 1));
    let (host, vcx) = open(cx, &rig, &one);
    click(vcx, "conversation.pages.item[terminal]");
    frame(vcx);
    let tab = tab_of(&host, vcx);
    assert_eq!(vcx.update(|_, cx| tab.read(cx).page()), Page::Terminal);

    // The other session opens on Messages; this one on the Terminal page again.
    rig.review(&two);
    frame(vcx);
    assert_eq!(vcx.update(|_, cx| tab.read(cx).page()), Page::Messages);
    rig.review(&one);
    frame(vcx);
    assert_eq!(vcx.update(|_, cx| tab.read(cx).page()), Page::Terminal);
}
