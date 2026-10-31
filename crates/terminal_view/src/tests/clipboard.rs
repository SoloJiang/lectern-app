//! `copy_*` and `paste_*` (docs/design/terminal_view.md「复制与粘贴」): ⌘C
//! asks the terminal for the selection and writes the reply to the
//! clipboard; ⌘V sends the clipboard's text as `Paste`.

use std::sync::Arc;

use gpui::{ClipboardEntry, ClipboardItem, EntityInputHandler, Image, ImageFormat, TestAppContext};
use lectern_services::terminal::{Input, ScrollTarget};

use crate::testing::frame;
use crate::tests::support::Rig;

fn clipboard_text(cx: &mut TestAppContext) -> Option<String> {
    cx.read_from_clipboard().and_then(|item| item.text())
}

#[gpui::test]
fn copy_with_a_selection_writes_the_clipboard(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(frame(&["hello world"]).selection(0, 0, 5).build()));
    rig.terminals.set_copy_text(Some("hello"));
    let mut visual = opened.visual(cx);
    visual.simulate_keystrokes("cmd-c");
    assert_eq!(rig.terminals.copy_calls(), 1);
    assert_eq!(clipboard_text(cx), Some("hello".to_owned()));
}

#[gpui::test]
fn copy_without_a_selection_does_nothing(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(frame(&["hello world"]).build()));
    rig.terminals.set_copy_text(Some("hello"));
    cx.write_to_clipboard(ClipboardItem::new_string("before".to_owned()));
    let mut visual = opened.visual(cx);
    visual.simulate_keystrokes("cmd-c");
    assert_eq!(rig.terminals.copy_calls(), 0);
    assert_eq!(clipboard_text(cx), Some("before".to_owned()));
}

/// The terminal has no text for the selection (it was cleared in between):
/// the clipboard keeps its content.
#[gpui::test]
fn copy_reply_without_text_keeps_the_clipboard(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(frame(&["hello world"]).selection(0, 0, 5).build()));
    rig.terminals.set_copy_text(None);
    cx.write_to_clipboard(ClipboardItem::new_string("before".to_owned()));
    let mut visual = opened.visual(cx);
    visual.simulate_keystrokes("cmd-c");
    assert_eq!(rig.terminals.copy_calls(), 1);
    assert_eq!(clipboard_text(cx), Some("before".to_owned()));
}

/// The host plugin was deactivated: the reply is dropped, not called.
#[gpui::test]
fn copy_reply_dropped_writes_nothing(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(frame(&["hello world"]).selection(0, 0, 5).build()));
    rig.terminals.drop_copy_replies();
    cx.write_to_clipboard(ClipboardItem::new_string("before".to_owned()));
    let mut visual = opened.visual(cx);
    visual.simulate_keystrokes("cmd-c");
    assert_eq!(rig.terminals.copy_calls(), 1);
    assert_eq!(clipboard_text(cx), Some("before".to_owned()));
}

#[gpui::test]
fn paste_sends_the_clipboard_text(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(frame(&["$"]).build()));
    rig.inputs();
    cx.write_to_clipboard(ClipboardItem::new_string("one\ntwo".to_owned()));
    let mut visual = opened.visual(cx);
    visual.simulate_keystrokes("cmd-v");
    assert_eq!(rig.inputs(), vec![Input::Paste(Arc::from("one\ntwo"))]);
}

#[gpui::test]
fn paste_with_nothing_on_the_clipboard_does_nothing(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(frame(&["$"]).build()));
    rig.inputs();
    let mut visual = opened.visual(cx);
    visual.simulate_keystrokes("cmd-v");
    assert_eq!(rig.inputs(), Vec::<Input>::new());
}

/// A picture alone: ⌘V sends nothing (an agent's own image paste is the
/// ordinary ⌃V key).
#[gpui::test]
fn paste_of_an_image_does_nothing(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(frame(&["$"]).build()));
    rig.inputs();
    cx.write_to_clipboard(ClipboardItem {
        entries: vec![ClipboardEntry::Image(Image::from_bytes(
            ImageFormat::Png,
            vec![0, 1, 2],
        ))],
    });
    let mut visual = opened.visual(cx);
    visual.simulate_keystrokes("cmd-v");
    assert_eq!(rig.inputs(), Vec::<Input>::new());
}

#[gpui::test]
fn paste_returns_the_viewport_to_the_bottom_first(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(
        cx,
        Some(frame(&["history"]).size(40, 1).scroll(100, 10).build()),
    );
    rig.inputs();
    cx.write_to_clipboard(ClipboardItem::new_string("text".to_owned()));
    let mut visual = opened.visual(cx);
    visual.simulate_keystrokes("cmd-v");
    assert_eq!(
        rig.inputs(),
        vec![
            Input::ScrollTo(ScrollTarget::Bottom),
            Input::Paste(Arc::from("text"))
        ]
    );
}

/// A paste the platform starts (the Edit menu) goes the way ⌘V goes.
#[gpui::test]
fn paste_from_the_platform_is_a_paste(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(frame(&["$"]).build()));
    rig.inputs();
    let mut visual = opened.visual(cx);
    opened.pane.update_in(&mut visual, |pane, window, cx| {
        pane.paste(
            ClipboardItem::new_string("from the menu".to_owned()),
            window,
            cx,
        );
    });
    assert_eq!(rig.inputs(), vec![Input::Paste(Arc::from("from the menu"))]);
}

#[gpui::test]
fn select_all_and_clear_scrollback_are_inputs(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(frame(&["$"]).build()));
    rig.inputs();
    let mut visual = opened.visual(cx);
    visual.simulate_keystrokes("cmd-a");
    visual.dispatch_action(crate::ClearScrollback);
    assert_eq!(rig.inputs(), vec![Input::SelectAll, Input::ClearScrollback]);
}
