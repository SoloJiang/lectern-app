//! The canary test and the verbose records (docs/design/terminal_view.md
//! 「日志与诊断」, `logs_and_nodes_hold_no_terminal_content`): the text of the
//! screen, a composition, a paste and the clipboard are carried through the
//! pane at trace level, and neither the log nor the semantic records of the
//! frame hold any of it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{ClipboardItem, EntityInputHandler, TestAppContext};
use lectern_services::terminal::{CursorShape, Input, TerminalId};

use crate::diag::FrameStats;
use crate::testing::frame;
use crate::tests::support::Rig;
use crate::tests::tree::{record, render_tree};

const CANARY: &str = "terminalcanary6e1f4a";

#[gpui::test]
fn logs_and_nodes_hold_no_terminal_content(cx: &mut TestAppContext) {
    let capture = lectern_logging::testing::capture();
    lectern_logging::testing::set_callsite_limit(1_000_000);
    let rig = Rig::new(cx);
    let lines = [format!("$ echo {CANARY}"), format!("{CANARY} 你好")];
    let opened = rig.open(
        cx,
        Some(
            frame(&[lines[0].as_str(), lines[1].as_str()])
                .cursor(2, 0, CursorShape::Block)
                .selection(1, 0, 6)
                .build(),
        ),
    );
    rig.terminals
        .set_copy_text(Some(&format!("selected {CANARY}")));
    let mut visual = opened.visual(cx);

    // A composition and its commit.
    opened.pane.update_in(&mut visual, |pane, window, cx| {
        pane.replace_and_mark_text_in_range(None, &format!("ni-{CANARY}"), None, window, cx);
    });
    opened.draw(cx);
    opened.pane.update_in(&mut visual, |pane, window, cx| {
        pane.replace_text_in_range(None, &format!("committed {CANARY}"), window, cx);
    });
    // Typing, the clipboard and a paste of it, a copy of the selection.
    visual.simulate_input(CANARY);
    cx.write_to_clipboard(ClipboardItem::new_string(format!("clipboard {CANARY}")));
    visual.simulate_keystrokes("cmd-v");
    visual.simulate_keystrokes("cmd-c");
    visual.simulate_keystrokes("enter");
    opened.draw(cx);

    // The canary did reach the terminal and the clipboard: the test is not
    // about a pane that ignored it.
    let inputs = rig.inputs();
    assert!(inputs.contains(&Input::Paste(Arc::from(format!("clipboard {CANARY}")))));
    assert!(inputs.contains(&Input::Text(Arc::from(format!("committed {CANARY}")))));
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some(format!("selected {CANARY}"))
    );

    // The verbose records: a resize and, after a second, the frame counts.
    let mut stats = FrameStats::default();
    let start = Instant::now();
    stats.record_prepaint(40, 4800, Duration::from_micros(250));
    stats.record_paint_at(TerminalId(7), Duration::from_micros(300), start);
    stats.record_paint_at(
        TerminalId(7),
        Duration::from_micros(200),
        start + Duration::from_millis(1100),
    );

    capture.assert_absent(CANARY);
    capture.assert_absent("你好");
    let lines = capture.lines().join("\n");
    assert!(lines.contains("terminal view resized"), "{lines}");
    assert!(lines.contains("terminal view frames"), "{lines}");
    assert!(lines.contains("rows_laid_out"), "{lines}");

    // The semantic records of the frame.
    let tree = render_tree(&record(&opened, cx));
    assert!(tree.contains("terminal.grid"));
    assert!(!tree.contains(CANARY), "{tree}");
    assert!(!tree.contains("你好"), "{tree}");
}
