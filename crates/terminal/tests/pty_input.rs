//! `input_*` through a real PTY (docs/design/terminal.md「测试」 输入编码,
//! the parts that need a process): what the program reads, window size
//! changes, and the input a finished process still takes. The program is the
//! fake program.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Kit, shell, size};
use lectern_services::terminal::{
    GridSize, Input, Key, KeyAction, KeyInput, Mods, RunState, ScrollInput, TerminalId,
};
use lectern_terminal::testing::Harness;

const WAIT: Duration = Duration::from_secs(20);

fn key(key: Key) -> Input {
    Input::Key(KeyInput {
        key,
        mods: Mods::default(),
        action: KeyAction::Press,
        text: None,
    })
}

/// Waits until the file holds `want` and returns what it holds.
fn wait_file(harness: &Harness, path: &std::path::Path, want: &[u8]) -> Vec<u8> {
    let got = harness.wait_for(WAIT, || {
        let bytes = std::fs::read(path).ok()?;
        (bytes.len() >= want.len()).then_some(bytes)
    });
    got.unwrap_or_else(|| panic!("{path:?} never held {want:?}"))
}

/// A terminal that reads its input into a file; returns once it reads.
fn reading(kit: &Kit, harness: &Harness, setup: &str) -> (TerminalId, std::path::PathBuf) {
    let received = kit.root.join("received.bin");
    kit.plan(&format!(
        "raw,{setup}say:READY,nl,input:{}",
        received.display()
    ));
    let id = harness.open(shell(&kit.project), size()).unwrap();
    // Kept for the test: the watch keeps frames coming.
    std::mem::forget(harness.terminals.watch(id).unwrap());
    harness.wait_frame(id, |text| text.contains("READY"));
    (id, received)
}

#[test]
fn input_keys_text_and_paste_reach_the_program_in_order() {
    let kit = Kit::new();
    let harness = kit.harness();
    let (id, received) = reading(&kit, &harness, "");
    harness.terminals.input(id, key(Key::Up));
    harness.terminals.input(id, Input::Text(Arc::from("x")));
    // No bracketed paste: newlines become carriage returns, nothing is added.
    harness.terminals.input(id, Input::Paste(Arc::from("a\nb")));
    harness.terminals.input(id, key(Key::Enter));
    let want = b"\x1b[Ax" as &[u8];
    let all = [want, b"a\rb", b"\r"].concat();
    assert_eq!(wait_file(&harness, &received, &all), all);
}

#[test]
fn input_paste_is_bracketed_once_the_program_asks_and_stripped_of_controls() {
    let kit = Kit::new();
    let on = kit.root.join("on.vt");
    std::fs::write(&on, b"\x1b[?2004h").unwrap();
    let harness = kit.harness();
    let (id, received) = reading(&kit, &harness, &format!("out:{},", on.display()));
    let frame = harness.wait_for(WAIT, || {
        harness
            .terminals
            .frame(id)
            .filter(|f| f.modes.bracketed_paste)
    });
    assert!(frame.is_some(), "the view sees bracketed paste is on");
    // ESC and a lookalike of the closing sequence cannot end the paste early.
    harness
        .terminals
        .input(id, Input::Paste(Arc::from("hi\x1b[201~there\t.")));
    let want = b"\x1b[200~hi[201~there\t.\x1b[201~";
    assert_eq!(wait_file(&harness, &received, want), want);
}

#[test]
fn input_resize_changes_the_window_size_the_program_sees() {
    let kit = Kit::new();
    kit.plan("size,winch");
    let harness = kit.harness();
    let id = harness.open(shell(&kit.project), size()).unwrap();
    let _watch = harness.terminals.watch(id).unwrap();
    harness.wait_frame(id, |text| text.contains("SIZE:80x24"));
    harness.wait_frame(id, |text| text.contains("WATCHING"));
    // Several resizes in a row: the program ends up with the last.
    for (cols, rows) in [(90, 30), (100, 31), (110, 32)] {
        harness.terminals.input(
            id,
            Input::Resize(GridSize {
                cols,
                rows,
                cell_width: 8,
                cell_height: 16,
            }),
        );
    }
    harness.wait_frame(id, |text| text.contains("SIZE:110x32"));
    let frame = harness.terminals.frame(id).unwrap();
    assert_eq!((frame.size.cols, frame.size.rows), (110, 32));
}

#[test]
fn input_focus_is_not_sent_until_the_program_asks() {
    let kit = Kit::new();
    let harness = kit.harness();
    let (id, received) = reading(&kit, &harness, "");
    harness.terminals.input(id, Input::Focus(true));
    harness.terminals.input(id, Input::Focus(false));
    // The marker arrives after the focus events would have.
    harness.terminals.input(id, Input::Text(Arc::from("z")));
    assert_eq!(wait_file(&harness, &received, b"z"), b"z");
}

#[test]
fn input_focus_is_sent_only_when_the_program_asks() {
    let kit = Kit::new();
    let on = kit.root.join("on.vt");
    std::fs::write(&on, b"\x1b[?1004h").unwrap();
    let harness = kit.harness();
    let (id, received) = reading(&kit, &harness, &format!("out:{},", on.display()));
    harness.wait_for(WAIT, || {
        harness.terminals.frame(id).filter(|f| f.modes.focus_events)
    });
    harness.terminals.input(id, Input::Focus(true));
    harness.terminals.input(id, Input::Focus(false));
    let want = b"\x1b[I\x1b[O";
    assert_eq!(wait_file(&harness, &received, want), want);
}

#[test]
fn input_after_exit_scrolls_selects_and_copies_but_drops_keys_and_paste() {
    let kit = Kit::new();
    let output = kit.root.join("output.vt");
    let lines: String = (0..100).map(|n| format!("line {n}\r\n")).collect();
    std::fs::write(&output, lines).unwrap();
    kit.plan(&format!("out:{},exit:0", output.display()));
    let harness = kit.harness();
    let id = harness.open(shell(&kit.project), size()).unwrap();
    let _watch = harness.terminals.watch(id).unwrap();
    let info = harness.wait_state(id, |s| matches!(s, RunState::Exited { .. }));
    assert_eq!(
        info.state,
        RunState::Exited {
            code: 0,
            signaled: false
        }
    );
    let frame = harness.wait_frame(id, |text| text.contains("line 99"));
    assert!(matches!(frame.state, RunState::Exited { .. }));
    let bottom = frame.scroll.offset;
    assert!(bottom > 0 && frame.scroll.total > u32::from(frame.size.rows));

    // The viewport scrolls and the selection works after the exit.
    harness.terminals.input(
        id,
        Input::Scroll(ScrollInput {
            lines: -10,
            col: 0,
            row: 0,
            mods: Mods::default(),
        }),
    );
    let scrolled = harness
        .wait_for(WAIT, || {
            harness
                .terminals
                .frame(id)
                .filter(|f| f.scroll.offset + 10 == bottom)
        })
        .expect("the viewport scrolled up");
    assert!(scrolled.rows.iter().any(|r| r.text.contains("line 7")));
    harness.terminals.input(id, Input::SelectAll);
    let selected = harness
        .wait_for(WAIT, || {
            harness
                .terminals
                .frame(id)
                .filter(|f| !f.selection.is_empty())
        })
        .expect("everything is selected");
    assert!(!selected.selection.is_empty());

    // Keys, text, paste and focus go nowhere: no crash, no state change.
    harness.terminals.input(id, key(Key::Enter));
    harness
        .terminals
        .input(id, Input::Text(Arc::from("ignored")));
    harness
        .terminals
        .input(id, Input::Paste(Arc::from("ignored")));
    harness.terminals.input(id, Input::Focus(true));
    harness.terminals.input(
        id,
        Input::ScrollTo(lectern_services::terminal::ScrollTarget::Bottom),
    );
    harness.wait_frame(id, |text| text.contains("line 99"));
    let info = harness.terminals.info(id).unwrap();
    assert_eq!(
        info.state,
        RunState::Exited {
            code: 0,
            signaled: false
        }
    );
    assert_eq!(info.dropped_inputs, 0);
    assert_eq!(kit.records(), 1, "one program was started");
}

#[test]
fn input_mouse_selection_works_without_mouse_tracking() {
    let kit = Kit::new();
    let output = kit.root.join("output.vt");
    std::fs::write(&output, b"hello world\r\n").unwrap();
    kit.plan(&format!("out:{},hold", output.display()));
    let harness = kit.harness();
    let id = harness.open(shell(&kit.project), size()).unwrap();
    harness.wait_state(id, |s| matches!(s, RunState::Running { .. }));
    let _watch = harness.terminals.watch(id).unwrap();
    harness.wait_frame(id, |text| text.contains("hello world"));
    // Double click on a word selects it.
    harness.terminals.input(
        id,
        Input::Mouse(lectern_services::terminal::MouseInput {
            kind: lectern_services::terminal::MouseKind::Down,
            button: Some(lectern_services::terminal::MouseButton::Left),
            mods: Mods::default(),
            col: 1,
            row: 0,
            x: 12.0,
            y: 4.0,
            clicks: 2,
        }),
    );
    let (tx, rx) = std::sync::mpsc::channel();
    harness.wait_for(WAIT, || {
        harness
            .terminals
            .frame(id)
            .filter(|f| !f.selection.is_empty())
    });
    harness.terminals.copy_selection(
        id,
        harness.token.clone(),
        Box::new(move |text| {
            let _ = tx.send(text);
        }),
    );
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5)),
        Ok(Some("hello".to_owned()))
    );
}
