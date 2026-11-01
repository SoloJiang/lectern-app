//! The plugins in a real kernel with the fake services (docs/design/conversation.md
//! 「测试」): what they register, how they deactivate, and how the page follows
//! the review on the screen.

use gpui::TestAppContext;
use lectern_kernel::Name;
use lectern_services::review::ReviewTarget;
use lectern_services::terminal::RunState;
use lectern_services::workspace::{ContributionKind, Route, Target, screens};
use lectern_ui::contrib::InspectorTab;

use crate::plugin::{LIVE_ID, TAB_ID, TERMINAL_ID};
use crate::testing::{Rig, prompt, session, snapshot, with_entries};

use super::stage::*;

/// The names, contexts and keys of a list of bindings, to compare two lists.
fn binding_names(
    bindings: &[(&'static str, Box<dyn gpui::Action>, Option<&'static str>)],
) -> Vec<(&'static str, &'static str, Option<&'static str>)> {
    bindings
        .iter()
        .map(|(keys, action, context)| (*keys, action.name(), *context))
        .collect()
}

#[gpui::test]
fn plugins_register_and_deactivate_cleanly(cx: &mut TestAppContext) {
    let rig = rig(cx);
    rig.activate_main();
    assert_eq!(
        rig.contributions.kinds(),
        [
            (ContributionKind::InspectorTab, "conversation".to_owned()),
            (ContributionKind::KeyBindings, "conversation".to_owned()),
        ]
    );
    // Staged: there was no token in `activate`.
    assert_eq!(rig.contributions.tokens_given(), [false, false]);
    // After the Files page (order 0).
    assert_eq!(rig.contributions.order(TAB_ID), Some(10));
    let tab: InspectorTab = rig.contributions.payload(TAB_ID).expect("the page");
    assert_eq!(tab.label.name(), "conversation-tab");
    // Not scoped to a screen: a scoped page would hide the Files page.
    assert_eq!(tab.screen, None);
    let bindings = cx.update(|cx| rig.contributions.key_bindings(cx));
    assert_eq!(bindings.len(), 1);
    assert_eq!(rig.contributions.released(), 0);

    // The terminal plugin adds the terminal's keys and the leave guard.
    rig.activate_terminal();
    assert_eq!(
        rig.contributions.kinds(),
        [
            (ContributionKind::InspectorTab, "conversation".to_owned()),
            (ContributionKind::KeyBindings, "conversation".to_owned()),
            (
                ContributionKind::KeyBindings,
                "conversation-terminal".to_owned()
            ),
        ]
    );
    assert_eq!(rig.contributions.tokens_given(), [false; 4]);
    assert_eq!(rig.contributions.order("conversation-terminal"), Some(0));
    let registered = rig
        .contributions
        .binding_names("conversation-terminal")
        .expect("the terminal's keys");
    let wanted = lectern_terminal_view::key_bindings();
    assert_eq!(registered, binding_names(&wanted));
    assert!(
        wanted
            .iter()
            .all(|(_, _, context)| *context == Some("terminal")),
        "the terminal's keys are in the terminal's context"
    );
    assert_eq!(rig.contributions.guards().len(), 1);
    assert_eq!(rig.contributions.guards_released(), 0);

    // Each plugin takes only its own registrations away.
    rig.deactivate_terminal();
    assert_eq!(rig.contributions.released(), 1);
    assert_eq!(rig.contributions.guards_released(), 1);
    rig.deactivate_main();
    assert_eq!(rig.contributions.released(), 3);
    assert_eq!(rig.contributions.guards_released(), 1);
}

/// "conversation-terminal" leaves and comes back on its own: its keys and its
/// guard go with it, the main plugin's registrations stay.
#[gpui::test]
fn terminal_plugin_deactivates_on_its_own(cx: &mut TestAppContext) {
    let rig = rig(cx);
    rig.activate_main();
    assert_eq!(rig.contributions.kinds().len(), 2);

    rig.activate_terminal();
    assert_eq!(rig.contributions.kinds().len(), 3);
    assert_eq!(rig.contributions.guards().len(), 1);

    rig.deactivate_terminal();
    assert_eq!(rig.contributions.released(), 1, "only the terminal's keys");
    assert_eq!(rig.contributions.guards_released(), 1);

    // It comes back and goes again without leaving anything behind.
    rig.kernel.set_enabled(TERMINAL_ID, true);
    rig.activate_terminal();
    assert_eq!(rig.contributions.kinds().len(), 4);
    assert_eq!(rig.contributions.guards().len(), 2);
    rig.deactivate_terminal();
    assert_eq!(rig.contributions.released(), 2);
    assert_eq!(rig.contributions.guards_released(), 2);
    rig.deactivate_main();
    assert_eq!(rig.contributions.released(), 4);
}

/// The reply box sees a terminal only while the plugin that brings
/// `dyn Terminals` is active.
#[gpui::test]
fn the_reply_box_sees_the_terminal_through_the_plugin(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(snapshot(&key, 1));
    rig.terminals.set(&key, RunState::Running { pid: 7 });
    rig.activate_main();
    rig.activate_live();
    rig.review(&key);
    let (_host, vcx) = window(cx, &rig);
    // The terminal plugin has not started: nobody sees the terminal.
    assert!(!on_screen(vcx, "conversation.composer.notice.action"));
    rig.activate_terminal();
    frame(vcx);
    assert!(on_screen(vcx, "conversation.composer.notice.action"));
    rig.deactivate_terminal();
    frame(vcx);
    assert!(!on_screen(vcx, "conversation.composer.notice.action"));
}

#[gpui::test]
fn live_plugin_deactivates_on_its_own(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(with_entries(
        &key,
        1,
        vec![prompt("h:0:0", "synthetic prompt")],
    ));
    let (host, vcx) = open(cx, &rig, &key);
    assert_eq!(
        rig.contributions.kinds().len(),
        3,
        "live adds no contribution"
    );
    assert!(on_screen(vcx, "conversation.list.item[h:0:0]"));
    assert_eq!(rig.conversations.alive(), 1);

    rig.deactivate_live();
    frame(vcx);
    // The page stays and says so; the interest is let go.
    assert!(!on_screen(vcx, "conversation.list.item[h:0:0]"));
    let shown = tree(vcx, all);
    assert!(
        shown.contains("conversation.notice Text label=[conversation-unavailable-service]"),
        "{shown}"
    );
    assert_eq!(rig.conversations.alive(), 0);
    assert_eq!(rig.contributions.released(), 0, "the main plugin stays");
    assert!(messages_of(&host, vcx).is_some());

    // It comes back: the page watches again and shows the history.
    rig.kernel.set_enabled(LIVE_ID, true);
    rig.activate_live();
    frame(vcx);
    assert!(on_screen(vcx, "conversation.list.item[h:0:0]"));
    assert_eq!(rig.conversations.alive(), 1);
}

#[gpui::test]
fn the_page_works_when_the_live_plugin_never_started(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.activate_main();
    rig.review(&key);
    let (_host, vcx) = window(cx, &rig);
    let shown = tree(vcx, all);
    assert!(
        shown.contains("conversation.notice Text label=[conversation-unavailable-service]"),
        "{shown}"
    );
    assert_eq!(rig.conversations.watched(), 0);
}

/// A Dock reopen keeps the navigation and emits no `ScreenChanged`: the new
/// page finds the session of the review still on the screen.
#[gpui::test]
fn a_page_built_after_the_window_closed_follows_the_review(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let one = session(1);
    rig.conversations
        .set(with_entries(&one, 1, vec![prompt("h:0:0", "in one")]));
    rig.activate_main();
    rig.activate_live();
    rig.review(&one);
    rig.kernel
        .emitter()
        .emit(lectern_services::workspace::WorkspaceEvent::WindowClosed {
            window: lectern_services::workspace::WindowId(0),
        });
    rig.kernel.barrier();

    let (host, vcx) = window(cx, &rig);
    assert_eq!(
        messages_of(&host, vcx).map(|model| vcx.update(|_, cx| model.read(cx).session().clone())),
        Some(one.clone())
    );
}

#[gpui::test]
fn tab_follows_the_review_route(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let (one, two) = (session(1), session(2));
    rig.conversations
        .set(with_entries(&one, 1, vec![prompt("h:0:0", "in one")]));
    rig.conversations
        .set(with_entries(&two, 1, vec![prompt("h:0:0", "in two")]));
    // The review was on the screen before the plugin started: its
    // `ScreenChanged` was not delivered, `current_route` has it.
    rig.contributions.set_route(Some(Rig::review_route(&one)));
    rig.activate_main();
    rig.activate_live();
    let (host, vcx) = window(cx, &rig);
    assert_eq!(
        messages_of(&host, vcx).map(|model| vcx.update(|_, cx| model.read(cx).session().clone())),
        Some(one.clone())
    );
    assert_eq!(rig.conversations.alive(), 1);

    // Another session: a new page for it, the old interest let go.
    rig.review(&two);
    frame(vcx);
    assert_eq!(
        messages_of(&host, vcx).map(|model| vcx.update(|_, cx| model.read(cx).session().clone())),
        Some(two.clone())
    );
    assert_eq!(rig.conversations.alive(), 1);
    assert_eq!(rig.conversations.watched(), 2);

    // A review that is not of a session: both pages say so.
    let key = lectern_services::project::ProjectKey::Folder {
        root: std::sync::Arc::from(std::path::Path::new("/work/app")),
    };
    rig.screen(Route {
        screen: Name::Borrowed(screens::REVIEW),
        target: Target::Review(ReviewTarget::Outside {
            project: key,
            worktree: lectern_services::project::WorktreeKey(std::sync::Arc::from(
                std::path::Path::new("/work/app"),
            )),
        }),
    });
    frame(vcx);
    assert!(messages_of(&host, vcx).is_none());
    assert_eq!(rig.conversations.alive(), 0);
    let shown = tree(vcx, all);
    assert!(
        shown.contains("conversation.empty Text label=[conversation-empty-no-session]"),
        "{shown}"
    );

    // Back on the project home, then in the review again.
    rig.home();
    frame(vcx);
    assert!(messages_of(&host, vcx).is_none());
    rig.review(&one);
    frame(vcx);
    assert!(on_screen(vcx, "conversation.list.item[h:0:0]"));
    assert_eq!(rig.conversations.alive(), 1);
}

#[gpui::test]
fn a_route_change_before_the_page_is_built_is_not_lost(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let key = session(1);
    rig.conversations.set(snapshot(&key, 1));
    rig.activate_main();
    rig.activate_live();
    // The review comes up before the inspector shows the page.
    rig.review(&key);
    let (host, vcx) = window(cx, &rig);
    assert!(messages_of(&host, vcx).is_some());
}
