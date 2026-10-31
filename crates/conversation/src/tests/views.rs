//! The Messages page in GPUI's test windows (docs/design/conversation.md「测试」
//! 的 `#[gpui::test]`): what the snapshots show, how the list keeps its place,
//! the commands the page sends. No test opens a real window.

use std::sync::Arc;

use gpui::{ListOffset, TestAppContext, px};
use lectern_services::conversation::{
    CallKind, CallState, ContextUsage, ConversationCommand, ConversationSnapshot, Entry,
    HistoryError, Link, LinkError, Load, OptionId, OptionKind, PermissionDetail, PermissionId,
    PermissionOption, PermissionRequest, PlanStep, StepState,
};
use lectern_services::session::Timestamp;
use lectern_ui::LoadingPhase;
use lectern_ui::motion::{LOADING_PROGRESS_AT, LOADING_SKELETON_AT};

use crate::items::{ItemKey, Splice};
use crate::testing::{Rig, agent_text, entry_key, prompt, session, snapshot, tool, with_entries};
use crate::view::Page;

use super::stage::*;

/// `n` entries of a conversation: a prompt, then the agent's long answer,
/// again and again. Tall enough that the list scrolls in the inspector.
fn long_conversation(n: usize) -> Vec<Entry> {
    let answer = "an answer of\nseveral lines\nthat takes room\nin the list";
    (0..n)
        .map(|at| {
            if at % 2 == 0 {
                prompt(&format!("h:{}:0", at / 2), &format!("prompt number {at}"))
            } else {
                agent_text(&format!("h:{}:1", at / 2), answer, false)
            }
        })
        .collect()
}

/// Sets a snapshot in the provider, tells the page and lets it settle.
fn push(rig: &Rig, vcx: &mut gpui::VisualTestContext, snap: ConversationSnapshot) {
    let (key, generation) = (snap.session.clone(), snap.generation);
    rig.conversations.set(snap);
    rig.changed(&key, generation);
    frame(vcx);
}

fn position(shown: &str, needle: &str) -> usize {
    shown
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} is not in the tree:\n{shown}"))
}

#[gpui::test]
fn history_renders_in_order_with_loading_phases(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    // The provider has no snapshot yet: the page waits.
    let (host, vcx) = open(cx, &rig, &key);
    let model = messages_of(&host, vcx).expect("a page for the session");
    assert_eq!(rig.conversations.watched(), 1, "the page watches at once");
    let phase = |vcx: &mut gpui::VisualTestContext| vcx.update(|_, cx| model.read(cx).phase(cx));
    assert_eq!(phase(vcx), LoadingPhase::Nothing);
    assert!(!tree(vcx, all).contains("conversation.empty"));

    // After 100 ms the skeleton; nothing with a node is drawn for it.
    vcx.executor().advance_clock(LOADING_SKELETON_AT);
    frame(vcx);
    assert_eq!(phase(vcx), LoadingPhase::Skeleton);
    assert!(!tree(vcx, all).contains("conversation.empty"));

    // After a second the message.
    vcx.executor()
        .advance_clock(LOADING_PROGRESS_AT - LOADING_SKELETON_AT);
    frame(vcx);
    assert_eq!(phase(vcx), LoadingPhase::Progress);
    let shown = tree(vcx, all);
    assert!(
        shown.contains("conversation.empty Text label=[conversation-loading]"),
        "{shown}"
    );

    // `Load::Loading` in a snapshot is the same wait.
    let mut loading = snapshot(&key, 1);
    loading.load = Load::Loading;
    push(&rig, vcx, loading);
    assert!(tree(vcx, all).contains("conversation-loading"));

    // The history arrives: the items are in the snapshot's order.
    let snap = with_entries(
        &key,
        2,
        vec![
            prompt("h:0:0", "first prompt"),
            agent_text("h:0:1", "first answer", false),
            tool("h:0:2", CallKind::Edit, "a.rs", CallState::Completed),
            prompt("h:1:0", "second prompt"),
        ],
    );
    push(&rig, vcx, snap);
    let shown = tree(vcx, all);
    assert!(!shown.contains("conversation-loading"), "{shown}");
    let order = [
        "conversation.list.item[h:0:0] ListItem kind=prompt",
        "conversation.list.item[h:0:1] ListItem kind=agent",
        "conversation.list.item[h:0:2] ListItem kind=tools",
        "conversation.list.item[h:1:0] ListItem kind=prompt",
    ];
    let at: Vec<usize> = order.iter().map(|id| position(&shown, id)).collect();
    assert!(at.windows(2).all(|pair| pair[0] < pair[1]), "{shown}");
    assert!(
        shown.contains("conversation.list List entries=4 earlier=0"),
        "{shown}"
    );
}

#[gpui::test]
fn history_failures_and_an_empty_history_say_so(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(snapshot(&key, 1));
    let (_host, vcx) = open(cx, &rig, &key);
    let shown = tree(vcx, all);
    assert!(
        shown.contains("label=[conversation-empty-turns]"),
        "{shown}"
    );
    let mut generation = 1;
    for (load, label) in [
        (
            Load::Failed(HistoryError::NotFound),
            "conversation-empty-missing",
        ),
        (Load::Failed(HistoryError::Io), "conversation-error-history"),
        (
            Load::Failed(HistoryError::Unrecognized),
            "conversation-unrecognized",
        ),
    ] {
        generation += 1;
        let mut snap = snapshot(&key, generation);
        snap.load = load;
        push(&rig, vcx, snap);
        let shown = tree(vcx, all);
        assert!(
            shown.contains(&format!("conversation.empty Text label=[{label}]")),
            "{label}: {shown}"
        );
        assert!(!shown.contains("conversation.list "), "{shown}");
    }
}

#[gpui::test]
fn earlier_turns_load_on_request(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    let mut snap = with_entries(&key, 1, long_conversation(40));
    snap.earlier = 5;
    rig.conversations.set(snap);
    // The provider reads further back when it gets the command: five turns
    // become three, and two more are in front.
    let responder_key = key.clone();
    rig.conversations
        .respond(move |provider, session, command| {
            if matches!(command, ConversationCommand::LoadEarlier) {
                let mut entries = vec![
                    prompt("h:-2:0", "older prompt"),
                    agent_text("h:-2:1", "older answer", false),
                ];
                entries.extend(long_conversation(40));
                let mut snap = with_entries(&responder_key, 2, entries);
                snap.earlier = 3;
                assert_eq!(*session, responder_key);
                provider.set(snap);
            }
        });
    let (host, vcx) = open(cx, &rig, &key);
    let model = messages_of(&host, vcx).expect("a page");
    let shown = tree(vcx, all);
    assert!(
        shown.contains("conversation.list List entries=40 earlier=5"),
        "{shown}"
    );

    // The reader is at the top, where the button is, and reads the first
    // entry below it.
    vcx.update(|_, cx| {
        model.read(cx).list().scroll_to(ListOffset {
            item_ix: 0,
            offset_in_item: px(0.0),
        });
    });
    frame(vcx);
    assert!(on_screen(vcx, "conversation.earlier"));
    click(vcx, "conversation.earlier");
    frame(vcx);

    assert_eq!(
        rig.conversations.commands(),
        [(key.clone(), ConversationCommand::LoadEarlier)]
    );
    // The turns came in above the entry the reader was on, which stays at
    // the top of the list.
    let (top, count) = vcx.update(|_, cx| {
        let model = model.read(cx);
        (
            model.item_key(model.list().logical_scroll_top().item_ix),
            model.item_count(),
        )
    });
    assert_eq!(top, Some(ItemKey::Entry(entry_key("h:0:0"))));
    assert_eq!(count, 43, "the button, two older entries and forty");
    assert!(on_screen(vcx, "conversation.list.item[h:0:0]"));
}

#[gpui::test]
fn streaming_follows_the_bottom_only_when_at_the_bottom(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    let mut entries = long_conversation(30);
    entries.push(agent_text("l:15:1", "streaming from the start", true));
    let mut snap = with_entries(&key, 1, entries.clone());
    snap.link = Link::Working;
    rig.conversations.set(snap);
    let (host, vcx) = open(cx, &rig, &key);
    let model = messages_of(&host, vcx).expect("a page");
    let last = "conversation.list.item[l:15:1]";

    // The page opens at the end of the conversation.
    assert!(vcx.update(|_, cx| model.read(cx).list().is_following_tail()));
    assert!(on_screen(vcx, last));

    // At the bottom the list follows the streaming text as it grows.
    let grown = |n: usize, generation: u64| {
        let mut entries = entries.clone();
        *entries.last_mut().unwrap() =
            agent_text("l:15:1", &"more streaming text\n".repeat(n), true);
        let mut snap = with_entries(&key, generation, entries);
        snap.link = Link::Working;
        snap
    };
    push(&rig, vcx, grown(8, 2));
    assert!(on_screen(vcx, last), "still at the end");
    assert!(vcx.update(|_, cx| model.read(cx).list().is_following_tail()));

    // Scrolled up, the reader stays where they are however long it grows.
    vcx.update(|_, cx| model.read(cx).list().scroll_by(px(-900.0)));
    frame(vcx);
    assert!(!vcx.update(|_, cx| model.read(cx).list().is_following_tail()));
    let before = vcx.update(|_, cx| model.read(cx).list().logical_scroll_top());
    assert!(!on_screen(vcx, last));
    push(&rig, vcx, grown(16, 3));
    push(&rig, vcx, grown(24, 4));
    let after = vcx.update(|_, cx| model.read(cx).list().logical_scroll_top());
    assert_eq!(
        (before.item_ix, before.offset_in_item),
        (after.item_ix, after.offset_in_item),
        "the list did not move"
    );
    assert!(!on_screen(vcx, last));

    // Back at the bottom it follows again.
    vcx.update(|_, cx| model.read(cx).list().scroll_to_end());
    frame(vcx);
    push(&rig, vcx, grown(32, 5));
    assert!(on_screen(vcx, last));
}

#[gpui::test]
fn streaming_relayouts_only_the_last_item(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    let mut entries = long_conversation(20);
    entries.push(agent_text("l:10:1", "", true));
    let build = |bytes: usize, generation: u64| {
        let mut entries = entries.clone();
        *entries.last_mut().unwrap() = agent_text("l:10:1", &"x".repeat(bytes), true);
        let mut snap = with_entries(&key, generation, entries);
        snap.link = Link::Working;
        snap
    };
    rig.conversations.set(build(0, 1));
    let (host, vcx) = open(cx, &rig, &key);
    let model = messages_of(&host, vcx).expect("a page");
    let (before, _) = vcx.update(|_, cx| model.read(cx).spliced());

    for n in 1..=200u64 {
        push(&rig, vcx, build(200 * usize::try_from(n).unwrap(), n + 1));
        let (spliced, last) = vcx.update(|_, cx| model.read(cx).spliced());
        assert_eq!(spliced - before, usize::try_from(n).unwrap());
        assert_eq!(
            last,
            Some(Splice {
                start: 20,
                removed: 1,
                added: 1
            }),
            "update {n} changed the last item only"
        );
    }
}

#[gpui::test]
fn context_is_shown_only_when_given(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(snapshot(&key, 1));
    let (_host, vcx) = open(cx, &rig, &key);
    let shown = tree(vcx, all);
    assert!(
        shown.contains(
            "conversation.status Text label=[conversation-status-history] link=none context=none agent=claude-code"
        ),
        "{shown}"
    );

    let mut snap = snapshot(&key, 2);
    snap.link = Link::Idle;
    snap.context = Some(ContextUsage {
        used: 72_000,
        size: 100_000,
    });
    push(&rig, vcx, snap);
    let shown = tree(vcx, all);
    assert!(
        shown.contains(
            "conversation.status Text label=[conversation-status-idle] link=idle context=72 agent=claude-code"
        ),
        "{shown}"
    );

    // The agent stopped reporting: the figure is gone, nothing is guessed.
    let mut snap = snapshot(&key, 3);
    snap.link = Link::Idle;
    push(&rig, vcx, snap);
    assert!(tree(vcx, all).contains("link=idle context=none"));
}

#[gpui::test]
fn the_status_line_follows_the_link(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(snapshot(&key, 1));
    let (_host, vcx) = open(cx, &rig, &key);
    for (n, (link, name, label)) in [
        (
            Link::Connecting,
            "connecting",
            "conversation-status-connecting",
        ),
        (Link::Working, "working", "conversation-status-working"),
        (Link::Stopping, "stopping", "conversation-status-stopping"),
        (
            Link::Failed(LinkError::Protocol),
            "failed",
            "conversation-status-failed",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut snap = snapshot(&key, 2 + n as u64);
        snap.link = link;
        push(&rig, vcx, snap);
        let shown = tree(vcx, all);
        assert!(
            shown.contains(&format!("label=[{label}] link={name}")),
            "{shown}"
        );
    }

    // A request waiting for the user is what the line says (its card is
    // the reply step's).
    let mut waiting = snapshot(&key, 10);
    waiting.link = Link::Working;
    waiting.permissions = Arc::from(vec![PermissionRequest {
        id: PermissionId(1),
        kind: CallKind::Execute,
        title: Arc::from("synthetic request"),
        detail: PermissionDetail::None,
        options: Arc::from(vec![PermissionOption {
            id: OptionId(Arc::from("allow")),
            kind: OptionKind::AllowOnce,
        }]),
        asked_at: Timestamp(1),
    }]);
    push(&rig, vcx, waiting);
    let shown = tree(vcx, all);
    assert!(
        shown.contains("label=[conversation-status-waiting] link=working"),
        "{shown}"
    );
}

#[gpui::test]
fn failed_link_shows_the_reason_and_dismisses(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(with_entries(
        &key,
        1,
        vec![prompt("h:0:0", "a prompt that was sent")],
    ));
    // Dismissing clears the failure, as the provider does.
    let responder_key = key.clone();
    rig.conversations.respond(move |provider, _, command| {
        if matches!(command, ConversationCommand::Dismiss) {
            provider.set(with_entries(
                &responder_key,
                100,
                vec![prompt("h:0:0", "a prompt that was sent")],
            ));
        }
    });
    let (_host, vcx) = open(cx, &rig, &key);
    assert!(!tree(vcx, all).contains("conversation.notice"));

    let cases = [
        (LinkError::Spawn, "conversation-error-spawn"),
        (LinkError::Unsupported, "conversation-error-unsupported"),
        (LinkError::LoadFailed, "conversation-error-load"),
        (LinkError::Protocol, "conversation-error-protocol"),
        (
            LinkError::Exited { code: Some(1) },
            "conversation-error-exited",
        ),
        (LinkError::AuthRequired, "conversation-error-auth"),
        (
            LinkError::PromptFailed {
                message: Arc::from("the agent's own words"),
            },
            "conversation-error-prompt",
        ),
    ];
    for (n, (error, label)) in cases.into_iter().enumerate() {
        let mut snap = with_entries(
            &key,
            2 + n as u64,
            vec![prompt("h:0:0", "a prompt that was sent")],
        );
        snap.link = Link::Failed(error.clone());
        push(&rig, vcx, snap);
        let shown = tree(vcx, all);
        assert!(
            shown.contains(&format!("conversation.notice Text label=[{label}]")),
            "{label}: {shown}"
        );
        assert!(
            shown.contains("conversation.notice.action Button label=[conversation-dismiss]"),
            "{shown}"
        );
        // The agent's words, as it wrote them, only for a failed prompt.
        assert_eq!(
            shown.contains("conversation.notice.detail Text label=\"the agent's own words\""),
            matches!(error, LinkError::PromptFailed { .. }),
            "{shown}"
        );
    }

    click(vcx, "conversation.notice.action");
    assert_eq!(
        rig.conversations.commands(),
        [(key.clone(), ConversationCommand::Dismiss)]
    );
    frame(vcx);
    let shown = tree(vcx, all);
    assert!(!shown.contains("conversation.notice"), "{shown}");
    assert!(shown.contains("link=none"), "{shown}");
}

#[gpui::test]
fn stale_generation_is_dropped(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations
        .set(with_entries(&key, 5, vec![prompt("h:0:0", "at five")]));
    let (host, vcx) = open(cx, &rig, &key);
    let model = messages_of(&host, vcx).expect("a page");
    let shown = |vcx: &mut gpui::VisualTestContext| {
        vcx.update(|_, cx| model.read(cx).snapshot().map(|snap| snap.generation))
    };
    assert_eq!(shown(vcx), Some(5));

    // An event older than what is shown is not even read.
    rig.conversations.set(with_entries(
        &key,
        3,
        vec![prompt("h:0:0", "at five"), prompt("h:1:0", "at three")],
    ));
    rig.changed(&key, 3);
    frame(vcx);
    assert_eq!(shown(vcx), Some(5));
    assert!(!on_screen(vcx, "conversation.list.item[h:1:0]"));

    // A newer event that finds an older snapshot (the provider swapped it
    // back) applies nothing.
    rig.changed(&key, 7);
    frame(vcx);
    assert_eq!(shown(vcx), Some(5));

    // A newer snapshot applies.
    rig.conversations.set(with_entries(
        &key,
        8,
        vec![prompt("h:0:0", "at five"), prompt("h:1:0", "at eight")],
    ));
    rig.changed(&key, 8);
    frame(vcx);
    assert_eq!(shown(vcx), Some(8));
    assert!(on_screen(vcx, "conversation.list.item[h:1:0]"));
}

#[gpui::test]
fn several_events_are_one_refresh_with_the_newest_generation(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations
        .set(with_entries(&key, 1, vec![prompt("h:0:0", "one")]));
    let (host, vcx) = open(cx, &rig, &key);
    let model = messages_of(&host, vcx).expect("a page");
    let (before, _) = vcx.update(|_, cx| model.read(cx).spliced());
    // Three events before the page runs: the provider's snapshot is the
    // latest, and the page reads it once.
    rig.conversations.set(with_entries(
        &key,
        4,
        vec![prompt("h:0:0", "one"), prompt("h:1:0", "two")],
    ));
    rig.changed(&key, 2);
    rig.changed(&key, 4);
    rig.changed(&key, 3);
    frame(vcx);
    let (after, last) = vcx.update(|_, cx| model.read(cx).spliced());
    assert_eq!(after - before, 1);
    assert_eq!(
        last,
        Some(Splice {
            start: 1,
            removed: 0,
            added: 1
        })
    );
}

#[gpui::test]
fn stop_sends_cancel(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    let mut working = with_entries(&key, 1, vec![prompt("h:0:0", "go")]);
    working.link = Link::Working;
    rig.conversations.set(working);
    // Cancelling stops the turn, as the provider does.
    let responder_key = key.clone();
    rig.conversations.respond(move |provider, _, command| {
        if matches!(command, ConversationCommand::Cancel) {
            let mut snap = with_entries(&responder_key, 2, vec![prompt("h:0:0", "go")]);
            snap.link = Link::Stopping;
            provider.set(snap);
        }
    });
    let (host, vcx) = open(cx, &rig, &key);
    let focus = {
        let tab = tab_of(&host, vcx);
        vcx.update(|_, cx| tab.read(cx).focus_handle())
    };
    vcx.update(|window, cx| window.focus(&focus, cx));
    vcx.simulate_keystrokes("cmd-.");
    assert_eq!(
        rig.conversations.commands(),
        [(key.clone(), ConversationCommand::Cancel)]
    );
    frame(vcx);
    assert!(tree(vcx, all).contains("label=[conversation-status-stopping] link=stopping"));
    // Stopping already: nothing more to stop.
    vcx.simulate_keystrokes("cmd-.");
    assert_eq!(rig.conversations.commands().len(), 1);
}

#[gpui::test]
fn the_plan_opens_and_closes(cx: &mut TestAppContext) {
    // Reduced motion, so that the steps are open in the first frame.
    let rig = rig_with(cx, true);
    let key = session(1);
    let mut snap = with_entries(&key, 1, vec![prompt("h:0:0", "go")]);
    snap.link = Link::Working;
    snap.plan = Some(Arc::from(vec![
        PlanStep {
            text: Arc::from("read the code"),
            state: StepState::Completed,
        },
        PlanStep {
            text: Arc::from("change it"),
            state: StepState::InProgress,
        },
        PlanStep {
            text: Arc::from("run the tests"),
            state: StepState::Pending,
        },
    ]));
    rig.conversations.set(snap);
    let (_host, vcx) = open(cx, &rig, &key);
    let shown = tree(vcx, all);
    assert!(
        shown.contains("conversation.plan Group expanded=false done=1 total=3"),
        "{shown}"
    );
    click(vcx, "conversation.plan.toggle");
    frame(vcx);
    frame(vcx);
    let shown = tree(vcx, all);
    assert!(
        shown.contains("conversation.plan Group expanded=true done=1 total=3"),
        "{shown}"
    );
    click(vcx, "conversation.plan.toggle");
    frame(vcx);
    assert!(tree(vcx, all).contains("conversation.plan Group expanded=false"));
}

#[gpui::test]
fn the_page_switch_shows_the_terminal_page_without_a_terminal(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations
        .set(with_entries(&key, 1, vec![prompt("h:0:0", "go")]));
    let (host, vcx) = open(cx, &rig, &key);
    let shown = tree(vcx, all);
    assert!(
        shown.contains("conversation.page Group page=messages"),
        "{shown}"
    );
    assert!(
        shown.contains(
            "conversation.pages.item[messages] Tab label=[conversation-page-messages] selected"
        ),
        "{shown}"
    );
    click(vcx, "conversation.pages.item[terminal]");
    frame(vcx);
    let shown = tree(vcx, all);
    assert!(
        shown.contains("conversation.page Group page=terminal"),
        "{shown}"
    );
    let tab = tab_of(&host, vcx);
    assert_eq!(vcx.update(|_, cx| tab.read(cx).page()), Page::Terminal);
    assert!(
        shown.contains("conversation.notice Text label=[conversation-terminal-unavailable]"),
        "{shown}"
    );
    assert!(!shown.contains("conversation.list "), "{shown}");
    // The Messages page keeps its watch while the Terminal page shows.
    assert_eq!(rig.conversations.alive(), 1);
    click(vcx, "conversation.pages.item[messages]");
    frame(vcx);
    assert!(tree(vcx, all).contains("conversation.list.item[h:0:0]"));
}
