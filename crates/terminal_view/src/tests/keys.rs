//! `keys_*`, `escape_reaches_the_terminal` and `cmd_keys_bubble`
//! (docs/design/terminal_view.md「键盘」): every row of the key table, the
//! modifiers, the printable characters that go through the input method,
//! the key repeat and release, and the priority of the shortcuts.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use gpui::{KeyDownEvent, KeyUpEvent, Keystroke, TestAppContext};
use lectern_services::terminal::{Input, Key, KeyAction, KeyInput, Mods, ScrollTarget};

use crate::testing::frame;
use crate::tests::support::Rig;

fn key(key: Key, mods: Mods) -> Input {
    Input::Key(KeyInput {
        key,
        mods,
        action: KeyAction::Press,
        text: None,
    })
}

const NONE: Mods = Mods {
    shift: false,
    control: false,
    option: false,
    command: false,
};

fn ctrl() -> Mods {
    Mods {
        control: true,
        ..NONE
    }
}

fn shift() -> Mods {
    Mods {
        shift: true,
        ..NONE
    }
}

fn option() -> Mods {
    Mods {
        option: true,
        ..NONE
    }
}

/// A rig with a pane on a one-line frame, the inputs of opening cleared.
fn rig(cx: &mut TestAppContext) -> (Rig, crate::tests::support::Opened) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(frame(&["$ "]).build()));
    rig.inputs();
    (rig, opened)
}

#[gpui::test]
fn keys_named(cx: &mut TestAppContext) {
    let (rig, opened) = rig(cx);
    let mut visual = opened.visual(cx);
    let mut table = vec![
        ("enter", Key::Enter),
        ("tab", Key::Tab),
        ("backspace", Key::Backspace),
        ("escape", Key::Escape),
        ("up", Key::Up),
        ("down", Key::Down),
        ("left", Key::Left),
        ("right", Key::Right),
        ("home", Key::Home),
        ("end", Key::End),
        ("pageup", Key::PageUp),
        ("pagedown", Key::PageDown),
        ("insert", Key::Insert),
        ("delete", Key::Delete),
    ];
    let functions: Vec<(String, Key)> = (1..=20).map(|n| (format!("f{n}"), Key::F(n))).collect();
    for (name, expected) in &functions {
        table.push((name.as_str(), *expected));
    }
    for (stroke, expected) in table {
        visual.simulate_keystrokes(stroke);
        assert_eq!(rig.inputs(), vec![key(expected, NONE)], "{stroke}");
    }
}

#[gpui::test]
fn keys_modifiers(cx: &mut TestAppContext) {
    let (rig, opened) = rig(cx);
    let mut visual = opened.visual(cx);
    let table = [
        ("shift-tab", Key::Tab, shift()),
        ("ctrl-up", Key::Up, ctrl()),
        ("alt-left", Key::Left, option()),
        ("alt-backspace", Key::Backspace, option()),
        ("shift-enter", Key::Enter, shift()),
        (
            "ctrl-shift-up",
            Key::Up,
            Mods {
                control: true,
                shift: true,
                ..NONE
            },
        ),
        (
            "ctrl-alt-delete",
            Key::Delete,
            Mods {
                control: true,
                option: true,
                ..NONE
            },
        ),
        (
            "ctrl-shift-pageup",
            Key::PageUp,
            Mods {
                control: true,
                shift: true,
                ..NONE
            },
        ),
    ];
    for (stroke, expected, mods) in table {
        visual.simulate_keystrokes(stroke);
        assert_eq!(rig.inputs(), vec![key(expected, mods)], "{stroke}");
    }
}

#[gpui::test]
fn keys_control_characters(cx: &mut TestAppContext) {
    let (rig, opened) = rig(cx);
    let mut visual = opened.visual(cx);
    let table = [
        ("ctrl-c", Key::Char('c'), ctrl()),
        ("ctrl-d", Key::Char('d'), ctrl()),
        ("ctrl-space", Key::Space, ctrl()),
        ("ctrl-[", Key::Char('['), ctrl()),
        (
            "ctrl-alt-x",
            Key::Char('x'),
            Mods {
                control: true,
                option: true,
                ..NONE
            },
        ),
    ];
    for (stroke, expected, mods) in table {
        visual.simulate_keystrokes(stroke);
        // `text` is None: the key is the whole message.
        assert_eq!(rig.inputs(), vec![key(expected, mods)], "{stroke}");
    }
}

#[gpui::test]
fn keys_scroll_to(cx: &mut TestAppContext) {
    let (rig, opened) = rig(cx);
    let mut visual = opened.visual(cx);
    for (stroke, target) in [
        ("shift-pageup", ScrollTarget::PageUp),
        ("shift-pagedown", ScrollTarget::PageDown),
        ("shift-home", ScrollTarget::Top),
        ("shift-end", ScrollTarget::Bottom),
    ] {
        visual.simulate_keystrokes(stroke);
        assert_eq!(rig.inputs(), vec![Input::ScrollTo(target)], "{stroke}");
    }
}

/// A printable character is not a `Key`: it reaches the terminal as the text
/// the input method commits.
#[gpui::test]
fn keys_printable_characters_are_text(cx: &mut TestAppContext) {
    let (rig, opened) = rig(cx);
    let mut visual = opened.visual(cx);
    visual.simulate_input("hi");
    assert_eq!(
        rig.inputs(),
        vec![Input::Text(Arc::from("h")), Input::Text(Arc::from("i"))]
    );
    visual.simulate_keystrokes("shift-a");
    assert_eq!(rig.inputs(), vec![Input::Text(Arc::from("A"))]);
    visual.simulate_keystrokes("space");
    assert_eq!(rig.inputs(), vec![Input::Text(Arc::from(" "))]);
}

/// ⌥ with a character is for the input method (⌥e e is é): the view sends
/// no key.
#[gpui::test]
fn keys_option_character_is_not_a_key(cx: &mut TestAppContext) {
    let (rig, opened) = rig(cx);
    let mut visual = opened.visual(cx);
    visual.simulate_keystrokes("alt-e");
    assert_eq!(rig.inputs(), Vec::<Input>::new());
}

#[gpui::test]
fn keys_command_is_not_handled(cx: &mut TestAppContext) {
    let (rig, opened) = rig(cx);
    let mut visual = opened.visual(cx);
    for stroke in ["cmd-j", "cmd-left", "cmd-shift-pageup", "cmd-enter"] {
        visual.simulate_keystrokes(stroke);
        assert_eq!(rig.inputs(), Vec::<Input>::new(), "{stroke}");
    }
}

#[gpui::test]
fn keys_repeat_and_release(cx: &mut TestAppContext) {
    let (rig, opened) = rig(cx);
    let mut visual = opened.visual(cx);
    let up = || Keystroke::parse("up").unwrap();
    visual.simulate_event(KeyDownEvent {
        keystroke: up(),
        is_held: false,
        prefer_character_input: false,
    });
    visual.simulate_event(KeyDownEvent {
        keystroke: up(),
        is_held: true,
        prefer_character_input: false,
    });
    visual.simulate_event(KeyUpEvent { keystroke: up() });
    let release = |action| {
        Input::Key(KeyInput {
            key: Key::Up,
            mods: NONE,
            action,
            text: None,
        })
    };
    assert_eq!(
        rig.inputs(),
        vec![
            release(KeyAction::Press),
            release(KeyAction::Repeat),
            release(KeyAction::Release)
        ]
    );
    // A key whose press the view did not handle has no release either.
    visual.simulate_event(KeyUpEvent {
        keystroke: Keystroke::parse("a").unwrap(),
    });
    visual.simulate_event(KeyUpEvent { keystroke: up() });
    assert_eq!(rig.inputs(), Vec::<Input>::new());
}

#[gpui::test]
fn keys_return_the_viewport_to_the_bottom_first(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(
        cx,
        Some(frame(&["history"]).size(40, 1).scroll(100, 10).build()),
    );
    rig.inputs();
    let mut visual = opened.visual(cx);
    visual.simulate_keystrokes("enter");
    visual.simulate_keystrokes("a");
    assert_eq!(
        rig.inputs(),
        vec![
            Input::ScrollTo(ScrollTarget::Bottom),
            key(Key::Enter, NONE),
            Input::Text(Arc::from("a")),
        ],
        "one ScrollTo until a newer frame comes"
    );
    // The frame at the bottom: nothing is sent first.
    rig.terminals.set_frame(
        crate::tests::support::ID,
        frame(&["now"]).size(40, 1).generation(2).build(),
    );
    opened.refresh(cx);
    visual.simulate_keystrokes("enter");
    assert_eq!(rig.inputs(), vec![key(Key::Enter, NONE)]);
}

/// Esc is the terminal's even though `workspace` binds it: `key_bindings()`
/// binds Esc to `NoAction` in the deeper `terminal` context.
#[gpui::test]
fn escape_reaches_the_terminal(cx: &mut TestAppContext) {
    let (rig, opened) = rig(cx);
    let mut visual = opened.visual(cx);
    visual.simulate_keystrokes("escape");
    assert_eq!(rig.inputs(), vec![key(Key::Escape, NONE)]);
    assert_eq!(opened.escapes.load(Ordering::SeqCst), 0);
}

#[gpui::test]
fn cmd_keys_bubble(cx: &mut TestAppContext) {
    let (rig, opened) = rig(cx);
    let mut visual = opened.visual(cx);
    visual.simulate_keystrokes("cmd-w");
    assert_eq!(opened.closes.load(Ordering::SeqCst), 1);
    assert_eq!(rig.inputs(), Vec::<Input>::new());
}
