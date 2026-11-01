//! The Terminal page and the leave guard in GPUI's test windows
//! (docs/design/conversation.md「测试」, step 3 of 「验收」). The terminal
//! service is a fake that records `open`, `watch`, `frame`, `input` and
//! `close`; no process runs and no test opens a real window.

use std::sync::{Arc, Mutex};

use gpui::{TestAppContext, VisualTestContext};
use lectern_services::conversation::{ConversationCommand, ConversationSnapshot, Link};
use lectern_services::session::SessionKey;
use lectern_services::terminal::{
    AgentCommand, FailReason, Launch, LaunchError, RunState, TerminalEvent, Terminals,
};
use lectern_services::workspace::{LeaveGuard, LeaveScope};
use lectern_terminal_view::testing::frame as grid;

use crate::testing::{Rig, folder, prompt, session, snapshot, summary, with_entries};

use super::stage::*;

// ----------------------------------------------------------------- helpers

fn has_node(shown: &str, id: &str) -> bool {
    shown
        .lines()
        .map(str::trim_start)
        .any(|line| line.starts_with(&format!("{id} ")) || line == id)
}

fn line<'a>(shown: &'a str, id: &str) -> &'a str {
    shown
        .lines()
        .map(str::trim_start)
        .find(|line| line.starts_with(&format!("{id} ")) || *line == id)
        .unwrap_or_else(|| panic!("{id} is not in the tree:\n{shown}"))
}

/// The page's nodes and the terminal grid's.
fn terminal_nodes(id: &str) -> bool {
    all(id) || id.starts_with("terminal.")
}

fn shown(vcx: &mut VisualTestContext) -> String {
    tree(vcx, terminal_nodes)
}

/// The three plugins on, the review of `key` up, the Terminal page open.
fn open_terminal<'a>(
    cx: &'a mut TestAppContext,
    rig: &Rig,
    key: &SessionKey,
) -> (gpui::Entity<Host>, &'a mut VisualTestContext) {
    let (host, vcx) = open(cx, rig, key);
    click(vcx, "conversation.pages.item[terminal]");
    frame(vcx);
    (host, vcx)
}

/// A snapshot whose link is `link`.
fn linked(key: &SessionKey, generation: u64, link: Link) -> ConversationSnapshot {
    let mut snap = with_entries(key, generation, vec![prompt("h:0:0", "an earlier prompt")]);
    snap.link = link;
    snap
}

/// The terminal answers `Listed` with a new state, and the page settles.
fn state_is(rig: &Rig, vcx: &mut VisualTestContext, key: &SessionKey, state: RunState) {
    rig.terminals.set(key, state);
    rig.terminals_listed();
    frame(vcx);
}

fn done_into(slot: &Arc<Mutex<Option<bool>>>) -> Box<dyn FnOnce(bool) + Send> {
    let slot = slot.clone();
    Box::new(move |allowed| {
        *slot.lock().unwrap() = Some(allowed);
    })
}

// ------------------------------------------------------- resume and embed

#[gpui::test]
fn terminal_page_resumes_and_embeds_the_pane(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(snapshot(&key, 1));
    let (host, vcx) = open_terminal(cx, &rig, &key);

    // No terminal: the explanation and the resume button, no pane.
    let before = shown(vcx);
    assert!(
        line(&before, "conversation.terminal").contains("state=none"),
        "{before}"
    );
    assert!(
        has_node(&before, "conversation.terminal.explain"),
        "{before}"
    );
    assert!(
        !line(&before, "conversation.terminal.resume").contains("disabled"),
        "{before}"
    );
    assert!(!has_node(&before, "terminal.grid"), "{before}");
    let page = terminal_of(&host, vcx).expect("the Terminal page");
    let area = vcx
        .update(|_, cx| page.read(cx).area())
        .expect("the area of the grid is known after a frame");
    assert!(area.width.as_f32() > 100.0 && area.height.as_f32() > 100.0);

    // Resume: the terminal opens with the grid of that area and the colours
    // of the window.
    click(vcx, "conversation.terminal.resume");
    let opens = rig.terminals.opens();
    assert_eq!(opens.len(), 1);
    assert_eq!(opens[0].launch, Launch::Resume(key.clone()));
    let (size, theme) = vcx.update(|window, cx| {
        (
            lectern_terminal_view::grid_size(area, window, cx),
            lectern_terminal_view::theme(window, cx),
        )
    });
    assert_eq!(opens[0].size, Some(size));
    assert_eq!(opens[0].theme, theme);
    frame(vcx);

    // The pane is there, in the starting state, and has the keyboard focus;
    // the resume button is gone and Close is there.
    let id = rig
        .terminals
        .id_of(&key)
        .expect("the terminal is registered");
    let page = terminal_of(&host, vcx).expect("page");
    let pane = vcx
        .update(|_, cx| page.read(cx).pane().cloned())
        .expect("a pane");
    let focus = vcx.update(|_, cx| pane.read(cx).focus_handle());
    assert!(vcx.update(|window, _| focus.is_focused(window)));
    let now = shown(vcx);
    assert!(
        line(&now, "conversation.terminal").contains("state=starting"),
        "{now}"
    );
    assert!(
        line(&now, "conversation.terminal.status")
            .contains("label=[conversation-terminal-starting]"),
        "{now}"
    );
    assert!(has_node(&now, "conversation.terminal.close"), "{now}");
    assert!(!has_node(&now, "conversation.terminal.resume"), "{now}");
    assert!(has_node(&now, "terminal.grid"), "{now}");

    // The pane got the token of the activation of "conversation-terminal":
    // a copy hands it to the terminal service.
    rig.terminals.set_frame(
        id,
        grid(&["synthetic terminal text"])
            .selection(0, 0, 9)
            .build(),
    );
    state_is(&rig, vcx, &key, RunState::Running { pid: 5 });
    vcx.dispatch_action(lectern_terminal_view::Copy);
    vcx.run_until_parked();
    let token = rig.shared.terminal_token().expect("the plugin is live");
    assert_eq!(rig.terminals.copy_tokens(), [token.id()]);

    // The state follows the terminal.
    let now = shown(vcx);
    assert!(
        line(&now, "conversation.terminal.status")
            .contains("label=[conversation-terminal-running]"),
        "{now}"
    );
    state_is(
        &rig,
        vcx,
        &key,
        RunState::Exited {
            code: 3,
            signaled: false,
        },
    );
    let now = shown(vcx);
    assert!(
        line(&now, "conversation.terminal.status").contains("label=[conversation-terminal-exited]"),
        "{now}"
    );
    // The last picture stays and the resume button is back.
    assert!(has_node(&now, "terminal.grid"), "{now}");
    assert!(has_node(&now, "conversation.terminal.resume"), "{now}");
    state_is(
        &rig,
        vcx,
        &key,
        RunState::Exited {
            code: 0,
            signaled: true,
        },
    );
    let now = shown(vcx);
    assert!(
        line(&now, "conversation.terminal.status")
            .contains("label=[conversation-terminal-signaled]"),
        "{now}"
    );

    // Resuming again replaces the ended terminal with a new one.
    click(vcx, "conversation.terminal.resume");
    frame(vcx);
    let again = rig.terminals.id_of(&key).expect("a terminal");
    assert_ne!(again, id);
    assert_eq!(rig.terminals.opens().len(), 2);
    let now = shown(vcx);
    assert!(
        line(&now, "conversation.terminal").contains("state=starting"),
        "{now}"
    );

    // Close: the terminal is closed, the page is back to the explanation.
    click(vcx, "conversation.terminal.close");
    frame(vcx);
    assert_eq!(rig.terminals.closed().last(), Some(&again));
    let now = shown(vcx);
    assert!(
        line(&now, "conversation.terminal").contains("state=none"),
        "{now}"
    );
    assert!(has_node(&now, "conversation.terminal.explain"), "{now}");
    assert!(!has_node(&now, "terminal.grid"), "{now}");
}

#[gpui::test]
fn a_terminal_that_runs_is_taken_when_the_page_is_built(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(snapshot(&key, 1));
    rig.terminals.set(&key, RunState::Running { pid: 9 });
    let (host, vcx) = open_terminal(cx, &rig, &key);
    let now = shown(vcx);
    assert!(
        line(&now, "conversation.terminal").contains("state=running"),
        "{now}"
    );
    assert!(has_node(&now, "terminal.grid"), "{now}");
    assert!(!has_node(&now, "conversation.terminal.resume"), "{now}");
    assert!(rig.terminals.opens().is_empty(), "nothing was opened");
    assert_eq!(rig.terminals.alive(), 1, "the page is shown: it watches");
    assert!(terminal_of(&host, vcx).is_some());
}

#[gpui::test]
fn the_terminal_that_already_runs_is_adopted_without_a_reason(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(snapshot(&key, 1));
    let (_host, vcx) = open_terminal(cx, &rig, &key);
    // Another opener started it a moment ago; the page has not heard yet.
    rig.terminals.set(&key, RunState::Running { pid: 2 });
    click(vcx, "conversation.terminal.resume");
    frame(vcx);
    let now = shown(vcx);
    assert!(has_node(&now, "terminal.grid"), "{now}");
    assert!(!has_node(&now, "conversation.terminal.warning"), "{now}");
    assert!(
        line(&now, "conversation.terminal").contains("state=running"),
        "{now}"
    );
}

// --------------------------------------------------------------- refusals

/// `open` refuses with `error`: the reason under the button, and whether the
/// button can be pressed again.
fn refused(cx: &mut TestAppContext, error: LaunchError, label: &str, disabled: bool) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(snapshot(&key, 1));
    let (_host, vcx) = open_terminal(cx, &rig, &key);
    rig.terminals.refuse_next(error);
    click(vcx, "conversation.terminal.resume");
    frame(vcx);
    let now = shown(vcx);
    assert!(
        line(&now, "conversation.terminal.warning").contains(&format!("label=[{label}]")),
        "{now}"
    );
    assert_eq!(
        line(&now, "conversation.terminal.resume").contains("disabled"),
        disabled,
        "{now}"
    );
    assert!(!has_node(&now, "terminal.grid"), "{now}");
}

#[gpui::test]
fn a_session_that_cannot_be_resumed_says_so(cx: &mut TestAppContext) {
    refused(
        cx,
        LaunchError::NotResumable,
        "conversation-terminal-not-resumable",
        true,
    );
}

#[gpui::test]
fn a_folder_outside_the_projects_says_so(cx: &mut TestAppContext) {
    refused(
        cx,
        LaunchError::OutsideProject,
        "conversation-terminal-outside-project",
        true,
    );
}

#[gpui::test]
fn too_many_terminals_leaves_the_button_on(cx: &mut TestAppContext) {
    refused(
        cx,
        LaunchError::TooMany,
        "conversation-terminal-too-many",
        false,
    );
}

#[gpui::test]
fn a_refusal_that_can_pass_is_tried_again(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(snapshot(&key, 1));
    let (_host, vcx) = open_terminal(cx, &rig, &key);
    rig.terminals.refuse_next(LaunchError::TooMany);
    click(vcx, "conversation.terminal.resume");
    frame(vcx);
    click(vcx, "conversation.terminal.resume");
    frame(vcx);
    let now = shown(vcx);
    assert!(!has_node(&now, "conversation.terminal.warning"), "{now}");
    assert!(has_node(&now, "terminal.grid"), "{now}");
    assert_eq!(rig.terminals.opens().len(), 2);
}

#[gpui::test]
fn terminal_failure_arrives_after_open(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(snapshot(&key, 1));
    let (_host, vcx) = open_terminal(cx, &rig, &key);
    click(vcx, "conversation.terminal.resume");
    frame(vcx);
    let first = rig.terminals.id_of(&key).expect("registered at once");

    for (reason, label) in [
        (
            FailReason::NotInstalled(AgentCommand::ClaudeCode),
            "conversation-terminal-not-installed",
        ),
        (FailReason::CwdMissing, "conversation-terminal-cwd-missing"),
        (
            FailReason::OutsideProject,
            "conversation-terminal-outside-project",
        ),
        (
            FailReason::Spawn { errno: Some(2) },
            "conversation-terminal-failed",
        ),
        (FailReason::NoShell, "conversation-terminal-failed"),
        (FailReason::Internal, "conversation-terminal-failed"),
    ] {
        state_is(&rig, vcx, &key, RunState::Failed(reason));
        let now = shown(vcx);
        assert!(
            line(&now, "conversation.terminal.status").contains(&format!("label=[{label}]")),
            "{reason:?}: {now}"
        );
        assert!(
            line(&now, "conversation.terminal").contains("state=failed"),
            "{now}"
        );
        // The button can try again, and the picture stays.
        assert!(
            !line(&now, "conversation.terminal.resume").contains("disabled"),
            "{now}"
        );
        assert!(has_node(&now, "terminal.grid"), "{now}");
    }

    // Trying again replaces the failed terminal.
    click(vcx, "conversation.terminal.resume");
    frame(vcx);
    let second = rig.terminals.id_of(&key).expect("a new terminal");
    assert_ne!(first, second);
    let now = shown(vcx);
    assert!(
        line(&now, "conversation.terminal").contains("state=starting"),
        "{now}"
    );
}

#[gpui::test]
fn a_terminal_closed_elsewhere_gives_the_button_back(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(snapshot(&key, 1));
    rig.terminals.set(&key, RunState::Running { pid: 3 });
    let (_host, vcx) = open_terminal(cx, &rig, &key);
    let id = rig.terminals.id_of(&key).expect("a terminal");
    rig.terminals.close(id);
    rig.terminals_listed();
    frame(vcx);
    let now = shown(vcx);
    assert!(has_node(&now, "conversation.terminal.resume"), "{now}");
    assert!(!has_node(&now, "terminal.grid"), "{now}");
    assert_eq!(rig.terminals.alive(), 0);
}

// ------------------------------------------------------------ the frames

#[gpui::test]
fn terminal_frames_only_while_visible(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let (one, two) = (session(1), session(2));
    rig.conversations.set(snapshot(&one, 1));
    rig.conversations.set(snapshot(&two, 1));
    rig.terminals.set(&one, RunState::Running { pid: 1 });
    let other = rig
        .terminals
        .set_shell(folder("/work/app"), RunState::Running { pid: 2 });
    let (host, vcx) = open(cx, &rig, &one);
    // On the Messages page nobody watches: no page was built either.
    assert_eq!(rig.terminals.alive(), 0);
    assert!(terminal_of(&host, vcx).is_none());

    click(vcx, "conversation.pages.item[terminal]");
    frame(vcx);
    assert_eq!(rig.terminals.alive(), 1);
    let id = rig.terminals.id_of(&one).expect("terminal");

    // A frame of the terminal on the page refreshes the pane once, however
    // many arrive together; a frame of another terminal does not.
    let reads = rig.terminals.frame_reads(id);
    for generation in 1..=3 {
        rig.kernel
            .emitter()
            .emit(TerminalEvent::Frame { id, generation });
    }
    rig.kernel.barrier();
    frame(vcx);
    assert_eq!(rig.terminals.frame_reads(id), reads + 1);
    rig.terminal_frame(other, 9);
    frame(vcx);
    assert_eq!(rig.terminals.frame_reads(id), reads + 1);
    assert_eq!(rig.terminals.frame_reads(other), 0);

    // Any `Listed` refreshes it once too, together with the frames of the
    // same batch.
    let reads = rig.terminals.frame_reads(id);
    rig.kernel
        .emitter()
        .emit(TerminalEvent::Frame { id, generation: 4 });
    rig.kernel
        .emitter()
        .emit(TerminalEvent::Listed { generation: 2 });
    rig.kernel.barrier();
    frame(vcx);
    assert_eq!(rig.terminals.frame_reads(id), reads + 1);

    // The Messages page: the interest is let go; coming back watches again
    // and catches the pane up.
    click(vcx, "conversation.pages.item[messages]");
    frame(vcx);
    assert_eq!(rig.terminals.alive(), 0);
    let reads = rig.terminals.frame_reads(id);
    click(vcx, "conversation.pages.item[terminal]");
    frame(vcx);
    assert_eq!(rig.terminals.alive(), 1);
    assert_eq!(rig.terminals.watches(), 2);
    assert!(rig.terminals.frame_reads(id) > reads);

    // Another session: the page and its interest are gone.
    rig.review(&two);
    frame(vcx);
    assert_eq!(rig.terminals.alive(), 0);
    rig.review(&one);
    frame(vcx);
    // The session was last on the Terminal page: it opens there and watches.
    assert_eq!(rig.terminals.alive(), 1);

    // The inspector hides: the page is dropped with its interest.
    hide_inspector(&host, vcx);
    assert_eq!(rig.terminals.alive(), 0);
    show_inspector(&host, &rig, vcx);
    assert_eq!(rig.terminals.alive(), 1);
}

#[gpui::test]
fn the_terminal_page_follows_the_service_coming_and_going(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(snapshot(&key, 1));
    rig.terminals.set(&key, RunState::Running { pid: 1 });
    let (_host, vcx) = open_terminal(cx, &rig, &key);
    assert!(has_node(&shown(vcx), "terminal.grid"));
    assert_eq!(rig.terminals.alive(), 1);

    rig.deactivate_terminal();
    frame(vcx);
    let now = shown(vcx);
    assert!(
        line(&now, "conversation.notice").contains("label=[conversation-terminal-unavailable]"),
        "{now}"
    );
    assert!(!has_node(&now, "terminal.grid"), "{now}");
    assert_eq!(rig.terminals.alive(), 0, "the interest is let go");

    rig.kernel.set_enabled(crate::plugin::TERMINAL_ID, true);
    rig.activate_terminal();
    frame(vcx);
    let now = shown(vcx);
    assert!(has_node(&now, "terminal.grid"), "{now}");
    assert_eq!(rig.terminals.alive(), 1);
}

#[gpui::test]
fn the_page_says_when_the_terminal_plugin_is_not_there(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(snapshot(&key, 1));
    rig.activate_main();
    rig.activate_live();
    rig.review(&key);
    let (_host, vcx) = window(cx, &rig);
    click(vcx, "conversation.pages.item[terminal]");
    frame(vcx);
    let now = shown(vcx);
    assert!(
        line(&now, "conversation.notice").contains("label=[conversation-terminal-unavailable]"),
        "{now}"
    );
    assert!(!has_node(&now, "conversation.terminal.resume"), "{now}");
    // The Messages page works as before.
    click(vcx, "conversation.pages.item[messages]");
    frame(vcx);
    assert!(has_node(&shown(vcx), "conversation.composer"));
}

// ------------------------------------------------------- the ACP link

/// A provider that drops the connection when told to.
fn disconnects(rig: &Rig, key: &SessionKey) {
    let key = key.clone();
    rig.conversations.respond(move |provider, _, command| {
        if matches!(command, ConversationCommand::Disconnect) {
            provider.set(linked(&key, 50, Link::None));
        }
    });
}

#[gpui::test]
fn terminal_resume_respects_the_link(cx: &mut TestAppContext) {
    // An idle link is disconnected first.
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(linked(&key, 1, Link::Idle));
    disconnects(&rig, &key);
    let (_host, vcx) = open_terminal(cx, &rig, &key);
    let now = shown(vcx);
    assert!(
        line(&now, "conversation.terminal.resume")
            .contains("label=[conversation-terminal-disconnect-resume]"),
        "{now}"
    );
    click(vcx, "conversation.terminal.resume");
    frame(vcx);
    assert_eq!(
        rig.conversations.commands(),
        [(key.clone(), ConversationCommand::Disconnect)]
    );
    assert_eq!(rig.terminals.opens().len(), 1, "then the terminal opens");
    assert!(has_node(&shown(vcx), "terminal.grid"));
}

#[gpui::test]
fn a_failed_link_is_disconnected_first_too(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(linked(
        &key,
        1,
        Link::Failed(lectern_services::conversation::LinkError::Protocol),
    ));
    disconnects(&rig, &key);
    let (_host, vcx) = open_terminal(cx, &rig, &key);
    click(vcx, "conversation.terminal.resume");
    frame(vcx);
    assert_eq!(rig.conversations.commands().len(), 1);
    assert_eq!(rig.terminals.opens().len(), 1);
}

#[gpui::test]
fn a_provider_that_keeps_the_link_keeps_the_terminal_from_opening(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(linked(&key, 1, Link::Idle));
    // No responder: the snapshot still says `Idle` after `Disconnect`.
    let (_host, vcx) = open_terminal(cx, &rig, &key);
    click(vcx, "conversation.terminal.resume");
    frame(vcx);
    assert_eq!(rig.conversations.commands().len(), 1);
    assert!(rig.terminals.opens().is_empty());
}

#[gpui::test]
fn a_busy_link_turns_the_button_off(cx: &mut TestAppContext) {
    for link in [Link::Connecting, Link::Working, Link::Stopping] {
        let rig = rig(cx);
        let key = session(1);
        rig.conversations.set(linked(&key, 1, link.clone()));
        let (_host, vcx) = open_terminal(cx, &rig, &key);
        let now = shown(vcx);
        assert!(
            line(&now, "conversation.terminal.resume").contains("disabled"),
            "{link:?}: {now}"
        );
        assert!(
            line(&now, "conversation.terminal.warning")
                .contains("label=[conversation-terminal-linked]"),
            "{link:?}: {now}"
        );
        click(vcx, "conversation.terminal.resume");
        assert!(rig.terminals.opens().is_empty(), "{link:?}");
        assert!(rig.conversations.commands().is_empty(), "{link:?}");
    }
}

#[gpui::test]
fn the_terminal_page_hints_that_the_session_may_run_elsewhere(cx: &mut TestAppContext) {
    let key = session(1);
    let mut busy = summary(&key);
    busy.open_turn = true;
    busy.last_activity = crate::drive::now();
    let rig_one = {
        cx.executor().allow_parking();
        cx.update(|cx| {
            lectern_ui::init_fixed(
                cx,
                lectern_i18n::Strings::of(lectern_i18n::Locale::En),
                lectern_ui::Appearance::Light,
                false,
            );
        });
        Rig::with_sessions(vec![busy])
    };
    rig_one.conversations.set(snapshot(&key, 1));
    let (_host, vcx) = open_terminal(cx, &rig_one, &key);
    assert!(has_node(&shown(vcx), "conversation.elsewhere"));

    // A terminal that drives the session: the hint and the button are gone.
    state_is(&rig_one, vcx, &key, RunState::Running { pid: 1 });
    let now = shown(vcx);
    assert!(!has_node(&now, "conversation.elsewhere"), "{now}");
    assert!(!has_node(&now, "conversation.terminal.resume"), "{now}");
}

// ---------------------------------------------------------- leave guard

/// A rig with terminals in two projects: running in `/work/app` and starting in
/// `/work/other`, one exited in `/work/app`, and the page built (so that the
/// guard has a window to ask in).
fn guarded(
    cx: &mut TestAppContext,
) -> (
    Rig,
    gpui::Entity<Host>,
    &mut VisualTestContext,
    Arc<dyn LeaveGuard>,
) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(snapshot(&key, 1));
    rig.terminals.set_in(
        &session(1),
        folder("/work/app"),
        RunState::Running { pid: 1 },
    );
    rig.terminals
        .set_in(&session(2), folder("/work/other"), RunState::Starting);
    rig.terminals.set_in(
        &session(3),
        folder("/work/app"),
        RunState::Exited {
            code: 0,
            signaled: false,
        },
    );
    let (host, vcx) = open(cx, &rig, &key);
    let guard = rig.contributions.guards().remove(0);
    (rig, host, vcx, guard)
}

#[gpui::test]
fn leave_guard_project_counts_the_projects_running_terminals(cx: &mut TestAppContext) {
    let (_rig, _host, vcx, guard) = guarded(cx);
    let slot = Arc::new(Mutex::new(None));
    guard.ask(LeaveScope::Project(folder("/work/app")), done_into(&slot));
    vcx.run_until_parked();
    // One runs in this project (the exited one does not count).
    let (title, detail) = vcx.pending_prompt().expect("a confirmation");
    assert_eq!(title, "1 terminal still running");
    assert_eq!(detail, "Their sessions stop when the terminals close.");
    assert_eq!(*slot.lock().unwrap(), None, "waits for the answer");
    vcx.simulate_prompt_answer("End Terminals");
    vcx.run_until_parked();
    assert_eq!(*slot.lock().unwrap(), Some(true));

    // Cancel stays.
    let slot = Arc::new(Mutex::new(None));
    guard.ask(LeaveScope::Project(folder("/work/app")), done_into(&slot));
    vcx.run_until_parked();
    vcx.simulate_prompt_answer("Cancel");
    vcx.run_until_parked();
    assert_eq!(*slot.lock().unwrap(), Some(false));

    // A project without running terminals goes on without asking.
    let slot = Arc::new(Mutex::new(None));
    guard.ask(
        LeaveScope::Project(folder("/work/nothing")),
        done_into(&slot),
    );
    assert_eq!(*slot.lock().unwrap(), Some(true));
    vcx.run_until_parked();
    assert!(!vcx.has_pending_prompt());
}

#[gpui::test]
fn leave_guard_app_counts_all_running_terminals(cx: &mut TestAppContext) {
    let (_rig, _host, vcx, guard) = guarded(cx);
    let slot = Arc::new(Mutex::new(None));
    guard.ask(LeaveScope::App, done_into(&slot));
    vcx.run_until_parked();
    let (title, _) = vcx.pending_prompt().expect("a confirmation");
    assert_eq!(title, "2 terminals still running");
    vcx.simulate_prompt_answer("Cancel");
    vcx.run_until_parked();
    assert_eq!(*slot.lock().unwrap(), Some(false));
}

#[gpui::test]
fn leave_guard_lets_the_other_scopes_go(cx: &mut TestAppContext) {
    let (_rig, _host, vcx, guard) = guarded(cx);
    for scope in [
        LeaveScope::View,
        LeaveScope::Window,
        LeaveScope::Worktree(folder("/work/app")),
    ] {
        let slot = Arc::new(Mutex::new(None));
        guard.ask(scope.clone(), done_into(&slot));
        assert_eq!(*slot.lock().unwrap(), Some(true), "{scope:?}");
    }
    vcx.run_until_parked();
    assert!(!vcx.has_pending_prompt());
}

#[gpui::test]
fn leave_guard_goes_on_when_the_terminal_service_is_gone(cx: &mut TestAppContext) {
    let (rig, _host, vcx, guard) = guarded(cx);
    // The provider of `dyn Terminals` stops: the reference answers None (and
    // the plugin that holds the guard goes with it).
    rig.kernel.set_enabled("mock-terminals", false);
    rig.kernel.barrier();
    let slot = Arc::new(Mutex::new(None));
    guard.ask(LeaveScope::App, done_into(&slot));
    assert_eq!(*slot.lock().unwrap(), Some(true));
    vcx.run_until_parked();
    assert!(!vcx.has_pending_prompt());
}

#[gpui::test]
fn leave_guard_without_a_window_goes_on(cx: &mut TestAppContext) {
    let rig = rig(cx);
    rig.activate_main();
    rig.activate_terminal();
    rig.terminals.set_in(
        &session(1),
        folder("/work/app"),
        RunState::Running { pid: 1 },
    );
    let capture = lectern_logging::testing::capture();
    let guard = rig.contributions.guards().remove(0);
    let slot = Arc::new(Mutex::new(None));
    // No page was ever built: there is no window to ask in.
    guard.ask(LeaveScope::App, done_into(&slot));
    assert_eq!(*slot.lock().unwrap(), Some(true));
    let lines = capture.lines();
    let warned = lines
        .iter()
        .find(|line| line.contains("\"msg\":\"terminal leave not asked\""))
        .unwrap_or_else(|| panic!("a warning says so: {lines:#?}"));
    for field in [
        "\"lv\":\"warn\"",
        "\"scope\":\"app\"",
        "\"running\":1",
        "\"reason\":\"no_window\"",
    ] {
        assert!(warned.contains(field), "{field} in {warned}");
    }
}

/// The window closes while a terminal keeps running (a terminal outlives its
/// window): nothing can be asked until a page is built in the window that a
/// Dock reopen makes, and the leave goes on with a warning meanwhile.
#[gpui::test]
fn leave_guard_goes_on_after_the_window_closed_until_a_page_is_built_again(
    cx: &mut TestAppContext,
) {
    let capture = lectern_logging::testing::capture();
    let (rig, host, vcx, guard) = guarded(cx);
    rig.kernel
        .emitter()
        .emit(lectern_services::workspace::WorkspaceEvent::WindowClosed {
            window: lectern_services::workspace::WindowId(0),
        });
    rig.kernel.barrier();
    vcx.run_until_parked();

    let slot = Arc::new(Mutex::new(None));
    guard.ask(LeaveScope::App, done_into(&slot));
    assert_eq!(*slot.lock().unwrap(), Some(true), "no one to ask");
    assert!(!vcx.has_pending_prompt());
    let lines = capture.lines();
    assert!(
        lines.iter().any(|line| {
            line.contains("\"msg\":\"terminal leave not asked\"")
                && line.contains("\"reason\":\"no_window\"")
                && line.contains("\"running\":2")
        }),
        "{lines:#?}"
    );

    // A page built again (the inspector shows in the new window) asks again.
    show_inspector(&host, &rig, vcx);
    let slot = Arc::new(Mutex::new(None));
    guard.ask(LeaveScope::App, done_into(&slot));
    vcx.run_until_parked();
    assert!(vcx.has_pending_prompt());
    vcx.simulate_prompt_answer("Cancel");
    vcx.run_until_parked();
    assert_eq!(*slot.lock().unwrap(), Some(false));
}

/// The task that asks lives as long as its window: a page built again
/// replaces it, a terminal plugin that comes back is asked through the same
/// one.
#[gpui::test]
fn leave_guard_keeps_asking_after_the_terminal_plugin_comes_back(cx: &mut TestAppContext) {
    let (rig, _host, vcx, _guard) = guarded(cx);
    rig.deactivate_terminal();
    rig.kernel.set_enabled(crate::plugin::TERMINAL_ID, true);
    rig.activate_terminal();
    frame(vcx);
    let guard = rig.contributions.guards().remove(1);
    let slot = Arc::new(Mutex::new(None));
    guard.ask(LeaveScope::App, done_into(&slot));
    vcx.run_until_parked();
    assert!(vcx.has_pending_prompt());
    vcx.simulate_prompt_answer("End Terminals");
    vcx.run_until_parked();
    assert_eq!(*slot.lock().unwrap(), Some(true));
}

#[gpui::test]
fn leave_guard_questions_wait_for_each_other(cx: &mut TestAppContext) {
    let (_rig, _host, vcx, guard) = guarded(cx);
    let (first, second) = (Arc::new(Mutex::new(None)), Arc::new(Mutex::new(None)));
    guard.ask(LeaveScope::App, done_into(&first));
    guard.ask(LeaveScope::App, done_into(&second));
    vcx.run_until_parked();
    vcx.simulate_prompt_answer("End Terminals");
    vcx.run_until_parked();
    assert_eq!(*first.lock().unwrap(), Some(true));
    assert_eq!(*second.lock().unwrap(), None);
    vcx.simulate_prompt_answer("Cancel");
    vcx.run_until_parked();
    assert_eq!(*second.lock().unwrap(), Some(false));
}
