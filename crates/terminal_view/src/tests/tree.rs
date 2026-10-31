//! `semantic_tree` (docs/design/terminal_view.md「界面」「语义节点」): what
//! the probe sees of the pane: the `terminal.grid` node with its sizes and
//! states, and nothing of the screen.

use std::fmt::Write as _;

use gpui::TestAppContext;
use lectern_services::terminal::{FailReason, Modes, RunState, Terminals as _};
use lectern_ui::semantic::{Record, Recorder};

use crate::testing::frame;
use crate::tests::support::{ID, Opened, Rig};

pub const SCREEN_TEXT: &str = "text-on-the-screen";

/// One line per node: ID, role, label with its message key, state flags and
/// attributes. No bounds (they depend on the fonts).
pub fn render_tree(records: &[Record]) -> String {
    let mut out = String::new();
    for record in records {
        let semantic = &record.semantic;
        let _ = write!(out, "{} {:?}", semantic.id.as_str(), semantic.role);
        if let Some(label) = &semantic.label {
            match label.key {
                Some(key) => {
                    let _ = write!(out, " <{key}> {:?}", label.text.as_ref());
                }
                None => {
                    let _ = write!(out, " {:?}", label.text.as_ref());
                }
            }
        }
        if semantic.state.focused {
            out.push_str(" [focused]");
        }
        for (key, value) in &semantic.attrs {
            let _ = write!(out, " {key}={value}");
        }
        out.push('\n');
    }
    out
}

/// One frame with the recorder armed.
pub fn record(opened: &Opened, cx: &mut TestAppContext) -> Vec<Record> {
    opened.draw(cx);
    let mut visual = opened.visual(cx);
    let id = visual.update(|window, _| window.window_handle().window_id());
    Recorder::begin(id);
    visual.update(|window, cx| window.draw(cx).clear(cx));
    Recorder::finish()
}

#[gpui::test]
fn semantic_tree(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(frame(&[SCREEN_TEXT]).size(80, 24).build()));
    let mut snapshot = String::new();

    snapshot.push_str("running, focused\n");
    snapshot.push_str(&render_tree(&record(&opened, cx)));

    rig.terminals.set_frame(
        ID,
        frame(&[SCREEN_TEXT])
            .size(100, 30)
            .generation(2)
            .modes(Modes {
                alt_screen: true,
                ..Modes::default()
            })
            .scroll(500, 100)
            .build(),
    );
    rig.terminals.set_state(
        ID,
        RunState::Exited {
            code: 2,
            signaled: false,
        },
    );
    opened.refresh(cx);
    snapshot.push_str("\nexited, alternate screen, scrolled\n");
    snapshot.push_str(&render_tree(&record(&opened, cx)));

    rig.terminals
        .set_state(ID, RunState::Failed(FailReason::CwdMissing));
    opened.refresh(cx);
    snapshot.push_str("\nfailed\n");
    snapshot.push_str(&render_tree(&record(&opened, cx)));

    rig.terminals.set_state(ID, RunState::Starting);
    opened.refresh(cx);
    snapshot.push_str("\nstarting\n");
    snapshot.push_str(&render_tree(&record(&opened, cx)));

    let mut visual = opened.visual(cx);
    visual.update(|window, cx| window.blur(cx));
    snapshot.push_str("\nstarting, no focus\n");
    snapshot.push_str(&render_tree(&record(&opened, cx)));

    assert!(
        !snapshot.contains(SCREEN_TEXT),
        "the screen is not in the tree"
    );
    insta::assert_snapshot!(snapshot);
}

/// The state of a closed terminal; `closed` is also what an unknown ID is.
#[gpui::test]
fn semantic_tree_closed(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(frame(&[SCREEN_TEXT]).build()));
    rig.terminals.close(ID);
    opened.refresh(cx);
    let tree = render_tree(&record(&opened, cx));
    assert!(tree.contains("state=closed"), "{tree}");
    assert!(!tree.contains(SCREEN_TEXT));
}

/// The node is declared once, under the id the host nests `conversation.terminal` over.
#[gpui::test]
fn semantic_tree_is_one_group_node(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(frame(&["x"]).build()));
    let records = record(&opened, cx);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].semantic.id.as_str(), "terminal.grid");
    assert_eq!(records[0].parent, None);
    assert!(records[0].visible);
}
