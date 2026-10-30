//! Input encoding (design 「测试」: `input_*`, `paste_*`; the parts without a
//! process).
#![cfg(feature = "test-support")]

use std::sync::Arc;

use lectern_services::terminal::{
    Input, Key, KeyAction, KeyInput, Mods, MouseButton, MouseInput, MouseKind, RunState,
    ScrollInput,
};
use lectern_terminal::testing::{self, Engine};

fn engine() -> Engine {
    Engine::new(testing::size(20, 5), testing::theme())
}

fn key(key: Key, mods: Mods, text: Option<&str>) -> Input {
    Input::Key(KeyInput {
        key,
        mods,
        action: KeyAction::Press,
        text: text.map(Arc::from),
    })
}

fn sent(e: &mut Engine, input: Input) -> Vec<u8> {
    e.take_pty_output();
    e.input(input);
    e.take_pty_output()
}

fn scroll(lines: i32, shift: bool) -> Input {
    Input::Scroll(ScrollInput {
        lines,
        col: 2,
        row: 1,
        mods: Mods {
            shift,
            ..Mods::default()
        },
    })
}

fn mouse(kind: MouseKind, col: u16, row: u16, shift: bool) -> Input {
    Input::Mouse(MouseInput {
        kind,
        button: Some(MouseButton::Left),
        mods: Mods {
            shift,
            ..Mods::default()
        },
        col,
        row,
        x: f32::from(col) * 8.0 + 4.0,
        y: f32::from(row) * 16.0 + 8.0,
        clicks: 1,
    })
}

#[test]
fn input_text_and_plain_keys() {
    let mut e = engine();
    assert_eq!(
        sent(&mut e, key(Key::Char('a'), Mods::default(), Some("a"))),
        b"a"
    );
    let shift = Mods {
        shift: true,
        ..Mods::default()
    };
    assert_eq!(sent(&mut e, key(Key::Char('a'), shift, Some("A"))), b"A");
    let ctrl = Mods {
        control: true,
        ..Mods::default()
    };
    assert_eq!(sent(&mut e, key(Key::Char('c'), ctrl, None)), b"\x03");
    assert_eq!(sent(&mut e, key(Key::Enter, Mods::default(), None)), b"\r");
    assert_eq!(
        sent(&mut e, key(Key::Backspace, Mods::default(), None)),
        b"\x7f"
    );
    assert_eq!(
        sent(&mut e, Input::Text(Arc::from("中文"))),
        "中文".as_bytes()
    );
}

#[test]
fn input_arrow_keys_follow_decckm() {
    let mut e = engine();
    assert_eq!(sent(&mut e, key(Key::Up, Mods::default(), None)), b"\x1b[A");
    e.feed(b"\x1b[?1h");
    assert_eq!(sent(&mut e, key(Key::Up, Mods::default(), None)), b"\x1bOA");
    e.feed(b"\x1b[?1l");
    assert_eq!(
        sent(&mut e, key(Key::Left, Mods::default(), None)),
        b"\x1b[D"
    );
}

#[test]
fn input_kitty_keyboard_protocol() {
    let mut e = engine();
    // CSI > 1 u: push "disambiguate escape codes".
    e.feed(b"\x1b[>1u");
    assert_eq!(
        sent(&mut e, key(Key::Escape, Mods::default(), None)),
        b"\x1b[27u"
    );
    let ctrl = Mods {
        control: true,
        ..Mods::default()
    };
    assert_eq!(sent(&mut e, key(Key::Char('c'), ctrl, None)), b"\x1b[99;5u");
    // Pop it: legacy encoding again.
    e.feed(b"\x1b[<u");
    assert_eq!(
        sent(&mut e, key(Key::Escape, Mods::default(), None)),
        b"\x1b"
    );
}

#[test]
fn input_mouse_tracking_encodes_unless_shift() {
    let mut e = engine();
    e.feed(b"some text\r\n");
    // Normal tracking with SGR encoding.
    e.feed(b"\x1b[?1000h\x1b[?1006h");
    assert_eq!(
        sent(&mut e, mouse(MouseKind::Down, 2, 1, false)),
        b"\x1b[<0;3;2M"
    );
    assert_eq!(
        sent(&mut e, mouse(MouseKind::Up, 2, 1, false)),
        b"\x1b[<0;3;2m"
    );
    assert!(e.frame().modes.mouse_tracking);
    // Shift: selection instead, nothing for the program.
    assert!(sent(&mut e, mouse(MouseKind::Down, 0, 0, true)).is_empty());
    assert!(sent(&mut e, mouse(MouseKind::Drag, 3, 0, true)).is_empty());
    assert_eq!(e.copy_selection().as_deref(), Some("some"));
}

#[test]
fn input_wheel_with_mouse_tracking_is_a_wheel_event() {
    let mut e = engine();
    e.feed(b"\x1b[?1000h\x1b[?1006h");
    assert_eq!(sent(&mut e, scroll(-1, false)), b"\x1b[<64;3;2M");
    assert_eq!(
        sent(&mut e, scroll(2, false)),
        b"\x1b[<65;3;2M\x1b[<65;3;2M"
    );
}

#[test]
fn input_wheel_on_alt_screen_becomes_arrow_keys() {
    let mut e = engine();
    e.feed(b"\x1b[?1049h");
    assert_eq!(sent(&mut e, scroll(-2, false)), b"\x1b[A\x1b[A");
    e.feed(b"\x1b[?1h");
    assert_eq!(sent(&mut e, scroll(1, false)), b"\x1bOB");
}

#[test]
fn input_wheel_on_primary_screen_scrolls_the_viewport() {
    let mut e = engine();
    for i in 0..20 {
        e.feed(format!("{i}\r\n").as_bytes());
    }
    let before = e.frame().scroll;
    assert!(sent(&mut e, scroll(-3, false)).is_empty());
    assert_eq!(e.frame().scroll.offset + 3, before.offset);
}

#[test]
fn input_focus_events_only_in_mode_1004() {
    let mut e = engine();
    assert!(sent(&mut e, Input::Focus(true)).is_empty());
    e.feed(b"\x1b[?1004h");
    assert!(e.frame().modes.focus_events);
    assert_eq!(sent(&mut e, Input::Focus(true)), b"\x1b[I");
    assert_eq!(sent(&mut e, Input::Focus(false)), b"\x1b[O");
}

#[test]
fn input_after_exit_keeps_viewport_input_and_drops_program_input() {
    for state in [
        RunState::Starting,
        RunState::Exited {
            code: 0,
            signaled: false,
        },
    ] {
        let mut e = engine();
        for i in 0..20 {
            e.feed(format!("line {i}\r\n").as_bytes());
        }
        e.feed(b"\x1b[?1004h\x1b[?2004h");
        e.set_state(state);
        let before = e.frame();
        assert_eq!(before.state, state);
        for input in [
            key(Key::Char('a'), Mods::default(), Some("a")),
            Input::Text(Arc::from("x")),
            Input::Paste(Arc::from("paste")),
            Input::Focus(true),
        ] {
            assert!(sent(&mut e, input).is_empty(), "{state:?}");
        }
        e.input(scroll(-2, false));
        assert_eq!(e.frame().scroll.offset + 2, before.scroll.offset);
        e.input(Input::SelectAll);
        assert!(e.copy_selection().is_some_and(|t| t.contains("line 19")));
    }
}

#[test]
fn input_mouse_tracking_after_exit_selects_nothing_for_the_program() {
    let mut e = engine();
    e.feed(b"\x1b[?1000h");
    e.set_state(RunState::Exited {
        code: 1,
        signaled: false,
    });
    assert!(sent(&mut e, mouse(MouseKind::Down, 1, 1, false)).is_empty());
    assert!(sent(&mut e, scroll(-1, false)).is_empty());
}

#[test]
fn paste_bracketed_wraps_the_text() {
    let mut e = engine();
    e.feed(b"\x1b[?2004h");
    assert!(e.frame().modes.bracketed_paste);
    assert_eq!(
        sent(&mut e, Input::Paste(Arc::from("one\ntwo"))),
        b"\x1b[200~one\ntwo\x1b[201~"
    );
}

#[test]
fn paste_plain_turns_newlines_into_returns() {
    let mut e = engine();
    assert_eq!(
        sent(&mut e, Input::Paste(Arc::from("one\ntwo"))),
        b"one\rtwo"
    );
}

#[test]
fn paste_strips_control_characters() {
    let mut e = engine();
    e.feed(b"\x1b[?2004h");
    let text = "a\tb\u{7}c\u{1b}d\u{7f}e\u{85}f\u{9b}g\r\n";
    assert_eq!(
        sent(&mut e, Input::Paste(Arc::from(text))),
        "\x1b[200~a\tbcdefg\n\x1b[201~".as_bytes()
    );
}

#[test]
fn paste_cannot_end_bracketed_paste_early() {
    let mut e = engine();
    e.feed(b"\x1b[?2004h");
    let out = sent(&mut e, Input::Paste(Arc::from("x\u{1b}[201~rm -rf ~\n")));
    assert_eq!(out, b"\x1b[200~x[201~rm -rf ~\n\x1b[201~");
    // Only the closing sequence ends the paste.
    let ends = out.windows(6).filter(|w| w == b"\x1b[201~").count();
    assert_eq!(ends, 1);
}

#[test]
fn paste_adds_no_return() {
    let mut e = engine();
    e.feed(b"\x1b[?2004h");
    let out = sent(&mut e, Input::Paste(Arc::from("review note")));
    assert_eq!(out, b"\x1b[200~review note\x1b[201~");
    let mut e = engine();
    assert_eq!(sent(&mut e, Input::Paste(Arc::from("note"))), b"note");
}

#[test]
fn no_echo_of_program_text_in_replies() {
    let mut e = engine();
    let canary = "CANARY-7f3a";
    e.feed(
        format!("\x1b]2;{canary}\x07\x1b]0;{canary}\x07\x1b]7;file://h/{canary}\x07").as_bytes(),
    );
    e.take_pty_output();
    // Title report, icon report, ENQ, DECRQSS, XTGETTCAP, device attributes,
    // XTVERSION, size and colour queries.
    e.feed(b"\x1b[21t\x1b[20t\x05\x1bP$qm\x1b\\\x1bP+q544e\x1b\\\x1b[c\x1b[>c\x1b[>q\x1b[18t\x1b]10;?\x07");
    let out = e.take_pty_output();
    let text = String::from_utf8_lossy(&out);
    assert!(!text.contains(canary), "{text:?}");
    assert!(
        text.contains("Lectern "),
        "XTVERSION answers with the app: {text:?}"
    );
    assert!(text.contains("\x1b[8;5;20t"), "size report: {text:?}");
}

#[test]
fn input_button_released_while_not_tracking_is_not_stuck() {
    let mut e = engine();
    e.feed(b"\x1b[?1000h\x1b[?1006h");
    sent(&mut e, mouse(MouseKind::Down, 2, 1, false));
    // Tracking ends before the release: the release goes to selection.
    e.feed(b"\x1b[?1000l");
    sent(&mut e, mouse(MouseKind::Up, 2, 1, false));
    // Any-motion tracking: a move with no button held is 35, not a drag (32).
    e.feed(b"\x1b[?1003h");
    let mut moved = mouse(MouseKind::Move, 3, 1, false);
    if let Input::Mouse(m) = &mut moved {
        m.button = None;
    }
    assert_eq!(sent(&mut e, moved), b"\x1b[<35;4;2M");
}

#[test]
fn input_drag_after_resize_starts_no_selection() {
    let mut e = engine();
    e.feed(b"some text\r\n");
    e.input(mouse(MouseKind::Down, 0, 0, false));
    e.input(Input::Resize(testing::size(10, 3)));
    e.input(mouse(MouseKind::Drag, 3, 0, false));
    assert_eq!(e.copy_selection(), None);
}
