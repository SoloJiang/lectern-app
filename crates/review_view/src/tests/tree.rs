//! The semantic trees (`review_tree_*`): what the probe sees of the screen
//! and of the Progress page, as text. No bounds (they depend on the fonts),
//! the call sites left out.

use std::fmt::Write as _;
use std::sync::Arc;

use gpui::TestAppContext;
use lectern_services::review::{
    Degraded, LoadState, Progress, ReviewError, ReviewTarget, SeenState,
};
use lectern_services::session::Timestamp;
use lectern_ui::semantic::Record;

use crate::testing::{FIRST_ID, session_key, snapshot_fixture};
use crate::tests::support::{Rig, ready};

/// One line per node, indented by depth.
pub fn render_tree(records: &[Record]) -> String {
    let depth = |mut ix: usize| {
        let mut depth = 0;
        while let Some(parent) = records[ix].parent {
            depth += 1;
            ix = parent;
        }
        depth
    };
    let mut out = String::new();
    for (ix, record) in records.iter().enumerate() {
        let semantic = &record.semantic;
        let _ = write!(
            out,
            "{}{} {:?}",
            "  ".repeat(depth(ix)),
            semantic.id.as_str(),
            semantic.role
        );
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
        let state = &semantic.state;
        let mut flags = Vec::new();
        if state.selected {
            flags.push("selected".to_owned());
        }
        if state.focused {
            flags.push("focused".to_owned());
        }
        if state.disabled {
            flags.push("disabled".to_owned());
        }
        if let Some(expanded) = state.expanded {
            flags.push(format!("expanded={expanded}"));
        }
        if let Some(checked) = state.checked {
            flags.push(format!("checked={checked}"));
        }
        if !flags.is_empty() {
            let _ = write!(out, " [{}]", flags.join(" "));
        }
        for (key, value) in &semantic.attrs {
            let _ = write!(out, " {key}={value}");
        }
        if !record.visible {
            out.push_str(" (hidden)");
        }
        out.push('\n');
    }
    out
}

fn session() -> ReviewTarget {
    ReviewTarget::Session(session_key())
}

#[gpui::test]
fn review_tree_session(cx: &mut TestAppContext) {
    // File 1 is seen, file 2 changed after it was seen: the review opens
    // with those marks, so that the progress bar is at rest.
    let rig = Rig::new(cx);
    let (mut snapshot, rows) = snapshot_fixture(3, 2, 6);
    let mut seen = snapshot.seen.to_vec();
    seen[1] = SeenState::Seen;
    seen[2] = SeenState::ChangedSinceSeen {
        since: Timestamp(1_700_000_000_000),
    };
    snapshot.seen = Arc::from(seen);
    snapshot.progress = Progress {
        total: 3,
        seen: 1,
        changed_since_seen: 1,
    };
    rig.review.set_all_rows(FIRST_ID, rows);
    rig.review.set_auto_prepare(true);
    for turn in 0..2 {
        rig.review.set_text(
            FIRST_ID,
            crate::testing::prompt_key(turn),
            "Make the change.",
        );
        rig.review.set_text(
            FIRST_ID,
            crate::testing::agent_key(turn),
            "Done: the files are changed.",
        );
    }
    let opened = rig.open(cx, session(), Some(snapshot));
    opened.draw(cx);
    rig.settle(cx);
    opened.draw(cx);
    // Select the first hunk, as J would.
    let mut visual = opened.visual(cx);
    visual.simulate_keystrokes("j");
    let records = opened.record(cx);
    insta::assert_snapshot!("review_tree_session", render_tree(&records));
}

#[gpui::test]
fn review_tree_groups(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let (mut snapshot, rows) = snapshot_fixture(2, 1, 5);
    snapshot.turns = None;
    snapshot.degraded = Some(Degraded::UnrecognizedSession);
    rig.review.set_all_rows(FIRST_ID, rows);
    rig.review.set_auto_prepare(true);
    let opened = rig.open(cx, session(), Some(snapshot));
    opened.draw(cx);
    rig.settle(cx);
    let records = opened.record(cx);
    insta::assert_snapshot!("review_tree_groups", render_tree(&records));
}

#[gpui::test]
fn review_tree_progress(cx: &mut TestAppContext) {
    let (rig, opened, _) = ready(cx, 3, 2, 4);
    let mut next = (*opened.read(cx, |model| model.snapshot.clone())).clone();
    next.generation += 1;
    let mut seen = next.seen.to_vec();
    seen[0] = SeenState::Seen;
    seen[1] = SeenState::ChangedSinceSeen {
        since: Timestamp(1_700_000_000_000),
    };
    next.seen = Arc::from(seen);
    next.progress = Progress {
        total: 3,
        seen: 1,
        changed_since_seen: 1,
    };
    rig.review.set_snapshot(FIRST_ID, next);
    rig.review.announce(FIRST_ID);
    rig.settle(cx);
    let tab = rig.open_tab(cx);
    let records = tab.record(cx);
    let text = render_tree(&records)
        .lines()
        .map(|line| {
            // The aid text of a changed file carries a relative time.
            line.to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!("review_tree_progress", text);
}

#[gpui::test]
fn review_tree_failed(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let (mut snapshot, _) = snapshot_fixture(1, 1, 4);
    snapshot.state = LoadState::Failed(ReviewError::NotInOpenProject);
    let opened = rig.open(cx, session(), Some(snapshot));
    let records = opened.record(cx);
    insta::assert_snapshot!("review_tree_failed", render_tree(&records));
}
