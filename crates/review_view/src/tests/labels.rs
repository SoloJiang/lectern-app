//! The attribution labels of a hunk's title and the visible range after a
//! splice (docs/design/review_view.md「归因、Drifted 与 Pending」).

use std::sync::Arc;

use lectern_i18n::{Locale, Strings};
use lectern_services::review::{HunkAttribution, TurnRef};
use lectern_services::session::{SessionKey, SourceId};

use crate::model::shift_visible;
use crate::render::hunk_labels;

fn en() -> Strings {
    Strings::of(Locale::En)
}

fn turn(ordinal: u32) -> TurnRef {
    TurnRef {
        session: 0,
        ordinal,
    }
}

fn session(id: &str) -> SessionKey {
    SessionKey {
        source: SourceId(Arc::from("fake")),
        id: Arc::from(id),
    }
}

fn attribution(turns: &[u32], others: &[&str], drifted: u32, lines: bool) -> HunkAttribution {
    HunkAttribution {
        lines,
        turns: turns.iter().copied().map(turn).collect(),
        others: others.iter().map(|id| session(id)).collect(),
        drifted,
    }
}

fn texts(labels: Vec<lectern_ui::Text>) -> Vec<String> {
    labels.iter().map(|t| t.as_str().to_owned()).collect()
}

#[test]
fn labels_a_hunk_shows_its_turns_and_other_sessions() {
    let info = attribution(&[2, 3], &["a", "b"], 0, true);
    assert_eq!(
        texts(hunk_labels(Some(&info), false, false, &en())),
        ["Turn 2", "Turn 3", "2 other sessions"]
    );
    let one = attribution(&[1], &["a"], 0, true);
    assert_eq!(
        texts(hunk_labels(Some(&one), false, false, &en())),
        ["Turn 1", "1 other session"]
    );
}

#[test]
fn labels_a_hunk_without_attribution_is_drifted() {
    let info = attribution(&[], &[], 4, true);
    assert_eq!(
        texts(hunk_labels(Some(&info), false, false, &en())),
        ["Drifted"]
    );
}

#[test]
fn labels_a_drifted_hunk_with_attribution_counts_the_lines() {
    let info = attribution(&[2], &[], 3, true);
    assert_eq!(
        texts(hunk_labels(Some(&info), false, false, &en())),
        ["Turn 2", "3 drifted lines"]
    );
    let one = attribution(&[2], &[], 1, true);
    assert_eq!(
        texts(hunk_labels(Some(&one), false, false, &en())),
        ["Turn 2", "1 drifted line"]
    );
}

#[test]
fn labels_pending_attribution_shows_none() {
    let info = attribution(&[2], &[], 3, true);
    assert!(hunk_labels(Some(&info), true, false, &en()).is_empty());
    assert!(hunk_labels(None, true, false, &en()).is_empty());
}

#[test]
fn labels_file_level_attribution_puts_nothing_on_the_hunks() {
    let info = attribution(&[], &[], 0, false);
    assert!(hunk_labels(Some(&info), false, false, &en()).is_empty());
}

#[test]
fn labels_an_unaligned_step_is_drifted_in_every_hunk() {
    let info = attribution(&[2], &[], 0, true);
    assert_eq!(
        texts(hunk_labels(Some(&info), false, true, &en())),
        ["Drifted"]
    );
    assert_eq!(texts(hunk_labels(None, false, true, &en())), ["Drifted"]);
}

#[test]
fn labels_follow_the_language() {
    let zh = Strings::of(Locale::ZhCn);
    let info = attribution(&[2], &[], 0, true);
    assert_eq!(
        texts(hunk_labels(Some(&info), false, false, &zh)),
        ["第 2 轮"]
    );
}

#[test]
fn visible_range_follows_a_splice() {
    // Items 10..20 are visible; item 4 grows from 1 to 11 items.
    assert_eq!(shift_visible(&(10..20), 4..5, 11), 20..30);
    // A splice after the range leaves it alone.
    assert_eq!(shift_visible(&(10..20), 30..31, 11), 10..20);
    // A placeholder in the range grows: the whole new segment counts.
    assert_eq!(shift_visible(&(10..20), 12..13, 11), 10..30);
    // A segment that shrinks (rows dropped for a placeholder).
    assert_eq!(shift_visible(&(0..5), 10..40, 1), 0..5);
    assert_eq!(shift_visible(&(50..60), 10..40, 1), 21..31);
    // The range ends inside the segment.
    assert_eq!(shift_visible(&(8..14), 10..40, 1), 8..11);
    // The range starts inside the segment.
    assert_eq!(shift_visible(&(12..50), 10..40, 1), 10..21);
}
