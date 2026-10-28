//! The screen as the design canvas draws it (docs/design/review_view.md
//! 「界面」, #283): the title block, the cards a diff is drawn in, the turns
//! that fold into a line, the Files page, the capsule and the toolbar.

use std::path::Path;
use std::sync::Arc;

use gpui::TestAppContext;
use lectern_services::project::{ProjectKey, WorktreeKey};
use lectern_services::review::{Degraded, ReviewTarget, RowsKey, StepId, TurnRef};

use crate::items::{Item, ItemKey};
use crate::progress_tab::{percent, short_names};
use crate::render::Slice;
use crate::testing::{FIRST_ID, file_key, snapshot_fixture};
use crate::tests::screen::{attr, find, must, session};
use crate::tests::support::{Rig, ready};

fn outside() -> ReviewTarget {
    ReviewTarget::Outside {
        project: ProjectKey::Folder {
            root: Arc::from(Path::new("/fake")),
        },
        worktree: WorktreeKey(Arc::from(Path::new("/fake"))),
    }
}

#[gpui::test]
fn the_header_names_the_review_and_counts_it(cx: &mut TestAppContext) {
    let (_rig, opened, _) = ready(cx, 3, 2, 8);
    assert_eq!(
        opened.read(cx, |model| model.items[0].clone()),
        Item::Header
    );
    assert_eq!(opened.read(cx, |model| model.items[1].clone()), Item::Brief);
    let records = opened.record(cx);
    let header = must(&records, "review.header");
    assert_eq!(attr(header, "files").as_deref(), Some("3"));
    assert_eq!(attr(header, "turns").as_deref(), Some("2"));
    assert_eq!(
        attr(header, "packages"),
        None,
        "the review does not count packages yet"
    );
}

#[gpui::test]
fn the_grouped_view_has_the_header_without_turns(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let (mut snapshot, rows) = snapshot_fixture(3, 1, 6);
    snapshot.turns = None;
    snapshot.degraded = Some(Degraded::UnrecognizedSession);
    rig.review.set_all_rows(FIRST_ID, rows);
    rig.review.set_auto_prepare(true);
    let opened = rig.open(cx, session(), Some(snapshot));
    opened.draw(cx);
    rig.settle(cx);
    let records = opened.record(cx);
    let header = must(&records, "review.header");
    assert_eq!(attr(header, "files").as_deref(), Some("3"));
    assert_eq!(attr(header, "turns"), None);
}

#[gpui::test]
fn a_card_is_cut_into_its_title_and_the_items_of_its_diff(cx: &mut TestAppContext) {
    let (_rig, opened, _) = ready(cx, 2, 1, 8);
    let (step, hunk, first_row, last_row, next_head) = opened.read(cx, |model| {
        let position = |wanted: &dyn Fn(&Item) -> bool| model.items.iter().position(wanted);
        let step = position(&|item| matches!(item, Item::Step { id, .. } if *id == StepId(0)));
        let step = step.expect("the first step");
        let hunk = step + 1;
        let end = model.items[step + 1..]
            .iter()
            .position(|item| !item.is_card_body())
            .map_or(model.items.len(), |at| step + 1 + at);
        let next = position(&|item| matches!(item, Item::Step { id, .. } if *id == StepId(1)));
        (
            step,
            hunk,
            hunk + 1,
            end - 1,
            next.expect("the second step"),
        )
    });
    let slices = opened.read(cx, |model| {
        [step, hunk, first_row, last_row, next_head].map(|ix| model.slice_of(ix))
    });
    assert_eq!(
        slices[0],
        Slice {
            first: true,
            last: false,
            pad_top: false,
            pad_bottom: false
        },
        "the title opens the card"
    );
    assert_eq!(
        slices[1],
        Slice {
            first: false,
            last: false,
            pad_top: true,
            pad_bottom: false
        },
        "the first item of the diff has room above it"
    );
    assert_eq!(
        slices[2],
        Slice {
            first: false,
            last: false,
            pad_top: false,
            pad_bottom: false
        }
    );
    assert_eq!(
        slices[3],
        Slice {
            first: false,
            last: true,
            pad_top: false,
            pad_bottom: true
        },
        "the last item closes the card and has room below"
    );
    assert!(slices[4].first, "the next card starts its own");
}

#[gpui::test]
fn the_pieces_of_a_card_share_its_width(cx: &mut TestAppContext) {
    let (_rig, opened, _) = ready(cx, 2, 1, 8);
    let records = opened.record(cx);
    let head = must(&records, "review.step.item[0]");
    let hunk = must(&records, "review.diff.item[step:0].hunk.item[0:0]");
    let row = must(&records, "review.diff.item[step:0].row.item[0:0]");
    let narrative = must(&records, "review.narrative");
    let gutter = crate::render::GUTTER;
    assert_eq!(head.bounds.origin.x, narrative.bounds.origin.x + gutter);
    assert_eq!(
        head.bounds.size.width,
        narrative.bounds.size.width - gutter * 2.0,
        "inset by the column's side margin on both sides"
    );
    for piece in [hunk, row] {
        assert_eq!(piece.bounds.origin.x, head.bounds.origin.x);
        assert_eq!(piece.bounds.size.width, head.bounds.size.width);
    }
    // A diff's lines are one line high; the card's title is not a line.
    assert!(head.bounds.size.height > row.bounds.size.height);
}

#[gpui::test]
fn a_hunk_of_a_session_draws_only_the_drift_note(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let (snapshot, mut all_rows) = snapshot_fixture(2, 1, 8);
    // Step 0's hunk has lines that match no edit; step 1's does not.
    for (key, rows) in &mut all_rows {
        if matches!(key, RowsKey::Step { step, .. } if *step == StepId(0)) {
            let mut hunks = rows.hunks.to_vec();
            hunks[0].attribution.drifted = 2;
            rows.hunks = Arc::from(hunks);
        }
    }
    rig.review.set_auto_prepare(true);
    rig.review.set_all_rows(FIRST_ID, all_rows);
    let opened = rig.open(cx, session(), Some(snapshot));
    opened.draw(cx);
    rig.settle(cx);
    let records = opened.record(cx);
    let line = f32::from(crate::tests::screen::line_height(
        &records,
        "review.diff.item[step:0].row.item[0:0]",
    ));
    let drifted = must(&records, "review.diff.item[step:0].hunk.item[0:0]");
    let plain = must(&records, "review.diff.item[step:1].hunk.item[0:0]");
    assert_eq!(
        f32::from(drifted.bounds.size.height),
        line,
        "the note is a line"
    );
    assert!(
        f32::from(plain.bounds.size.height) < 1.0,
        "no attribution title and no note: a hairline"
    );
}

#[gpui::test]
fn a_hunk_of_the_grouped_view_keeps_its_attribution_title(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let (mut snapshot, rows) = snapshot_fixture(2, 1, 8);
    snapshot.turns = None;
    snapshot.degraded = Some(Degraded::UnrecognizedSession);
    rig.review.set_all_rows(FIRST_ID, rows);
    rig.review.set_auto_prepare(true);
    let opened = rig.open(cx, session(), Some(snapshot));
    opened.draw(cx);
    rig.settle(cx);
    let records = opened.record(cx);
    let line = crate::tests::screen::line_height(
        &records,
        "review.diff.item[final:src/file_000.rs].row.item[0:0]",
    );
    let hunk = must(
        &records,
        "review.diff.item[final:src/file_000.rs].hunk.item[0:0]",
    );
    assert_eq!(
        hunk.bounds.size.height, line,
        "the title of the hunk is a line"
    );
}

#[gpui::test]
fn a_turn_folds_into_one_line_and_a_jump_opens_it(cx: &mut TestAppContext) {
    let (_rig, opened, _) = ready(cx, 3, 2, 8);
    let first = TurnRef {
        session: 0,
        ordinal: 1,
    };
    opened.update(cx, |model, cx| model.toggle_turn(first, cx));
    opened.draw(cx);
    let records = opened.record(cx);
    let turn = must(&records, "review.turn.item[0:1]");
    assert_eq!(turn.semantic.state.expanded, Some(false));
    assert!(
        find(&records, "review.turn.item[0:1].prompt").is_none(),
        "the prompt goes with the rest of the turn"
    );
    assert!(find(&records, "review.step.item[0]").is_none());
    assert!(
        find(&records, "review.step.item[1]").is_some(),
        "the other turn is whole"
    );
    let second = must(&records, "review.turn.item[0:2]");
    assert_eq!(second.semantic.state.expanded, Some(true));
    // The folded line is a line, not a block.
    assert!(turn.bounds.size.height < second.bounds.size.height + gpui::px(40.0));

    // A jump to a file of the folded turn opens it again.
    opened.update(cx, |model, cx| {
        model.jump_to_brief(Some(file_key(0)), None, cx)
    });
    opened.draw(cx);
    assert!(opened.read(cx, |model| model.collapsed_turns.is_empty()));
    assert_eq!(
        opened.read(cx, |model| model.selection.clone()),
        Some(ItemKey::Step(StepId(0)))
    );
    let records = opened.record(cx);
    assert_eq!(
        must(&records, "review.turn.item[0:1]")
            .semantic
            .state
            .expanded,
        Some(true)
    );

    // A second click on the turn's title folds it again, a third opens it.
    opened.update(cx, |model, cx| model.toggle_turn(first, cx));
    assert_eq!(opened.read(cx, |model| model.collapsed_turns.len()), 1);
    opened.update(cx, |model, cx| model.toggle_turn(first, cx));
    assert!(opened.read(cx, |model| model.collapsed_turns.is_empty()));
}

#[gpui::test]
fn the_capsule_names_the_agent_and_gives_way_when_all_is_reviewed(cx: &mut TestAppContext) {
    let (_rig, opened, _) = ready(cx, 3, 1, 6);
    let records = opened.record(cx);
    let send = must(&records, "review.bar.send");
    assert!(send.semantic.state.disabled, "no notes to send yet (#22)");
    let text = send.semantic.label.as_ref().map(|l| l.text.to_string());
    assert!(
        text.as_deref()
            .is_some_and(|text| text.contains("Claude Code") && text.ends_with('\u{21A9}')),
        "{text:?}"
    );
    assert!(find(&records, "review.bar.looks-good").is_some());
    // The order of the pieces: the notes, Looks Good, then Send.
    let at = |id: &str| must(&records, id).bounds.origin.x;
    assert!(at("review.bar.comments") < at("review.bar.looks-good"));
    assert!(at("review.bar.looks-good") < at("review.bar.send"));
}

#[gpui::test]
fn the_capsule_of_a_review_outside_a_session_sends_to_nobody_by_name(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let (snapshot, rows) = snapshot_fixture(2, 1, 4);
    rig.review.set_all_rows(FIRST_ID, rows);
    let opened = rig.open(cx, outside(), Some(snapshot));
    opened.draw(cx);
    let records = opened.record(cx);
    let send = must(&records, "review.bar.send");
    let text = send.semantic.label.as_ref().map(|l| l.text.to_string());
    assert_eq!(text.as_deref(), Some("Send \u{2318}\u{21A9}"));
    // The subtitle: how many files changed.
    let strings = lectern_i18n::Strings::of(lectern_i18n::Locale::En);
    let subtitle = opened
        .read(cx, |model| model.toolbar_subtitle(&strings))
        .map(|text| text.as_str().to_owned());
    assert_eq!(subtitle.as_deref(), Some("2 files changed"));
}

#[test]
fn the_files_page_names_a_file_by_its_last_segment() {
    let paths = [
        "src/main.rs",
        "crates/a/src/lib.rs",
        "crates/b/src/lib.rs",
        "Cargo.toml",
        "docs/a/readme.md",
        "docs/readme.md",
    ]
    .map(str::to_owned);
    assert_eq!(
        short_names(&paths),
        [
            "main.rs",
            "src/lib.rs",
            "src/lib.rs",
            "Cargo.toml",
            "a/readme.md",
            "docs/readme.md"
        ]
    );
    // Names that are the same under their parents too are left as the
    // parent and the name make them: the list does not invent more.
    assert_eq!(short_names(&["a.rs".to_owned()]), ["a.rs"]);
    assert!(short_names(&[]).is_empty());
}

#[test]
fn the_files_page_says_the_percent_in_whole_numbers() {
    assert_eq!(percent(0, 0), 0);
    assert_eq!(percent(0, 6), 0);
    assert_eq!(percent(2, 6), 33);
    assert_eq!(percent(5, 6), 83);
    assert_eq!(percent(6, 6), 100);
}

#[gpui::test]
fn the_files_page_shows_the_count_the_bar_and_the_turn_of_each_file(cx: &mut TestAppContext) {
    let (rig, opened, _) = ready(cx, 4, 2, 6);
    opened.update(cx, |model, cx| model.toggle_seen(file_key(1), cx));
    rig.settle(cx);
    let tab = rig.open_tab(cx);
    let records = tab.record(cx);
    let bar = must(&records, "review.progress.bar");
    assert_eq!(attr(bar, "fraction").as_deref(), Some("0.250"));
    let list = must(&records, "review.progress");
    assert_eq!(attr(list, "seen").as_deref(), Some("1"));
    assert_eq!(attr(list, "total").as_deref(), Some("4"));
}
