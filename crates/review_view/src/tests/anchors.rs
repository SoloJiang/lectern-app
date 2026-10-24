//! Finding the scroll position again after a rebuild
//! (docs/design/review_view.md「滚动位置与选中项」, `anchor_*`).

use gpui::{ListOffset, px};

use crate::anchor::{find, restore};
use crate::items::{Item, Notice};
use crate::testing::snapshot_fixture;
use crate::tests::items::{build, rows_map};

fn offset(ix: usize, px_in: f32) -> ListOffset {
    ListOffset {
        item_ix: ix,
        offset_in_item: px(px_in),
    }
}

#[test]
fn anchor_key_still_there_keeps_its_offset() {
    let (snapshot, rows) = snapshot_fixture(3, 2, 12);
    let old = build(&snapshot, &rows_map(rows.clone()), true, false);
    // The same items with two notices in front: everything moves down by 2.
    let mut new = vec![
        old[0].clone(),
        Item::Notice(Notice::Folder),
        Item::Notice(Notice::Unrecognized),
    ];
    new.extend(old[1..].iter().cloned());
    let at = offset(9, 5.0);
    let restored = restore(&old, at, &new);
    assert_eq!(restored.item_ix, 11);
    assert_eq!(restored.offset_in_item, px(5.0));
    assert_eq!(new[restored.item_ix].key(), old[9].key());
}

#[test]
fn anchor_key_gone_takes_the_first_later_one_that_exists() {
    let (snapshot, rows) = snapshot_fixture(3, 2, 12);
    let old = build(&snapshot, &rows_map(rows), true, false);
    // The anchor and the two items after it are gone.
    let mut new = old.clone();
    new.drain(9..12);
    let restored = restore(&old, offset(9, 7.0), &new);
    assert_eq!(restored.item_ix, 9, "the item that followed them");
    assert_eq!(new[9].key(), old[12].key());
    assert_eq!(
        restored.offset_in_item,
        px(0.0),
        "the offset is of another item"
    );
}

#[test]
fn anchor_nothing_left_clamps_to_the_new_length() {
    let (snapshot, rows) = snapshot_fixture(3, 2, 12);
    let old = build(&snapshot, &rows_map(rows), true, false);
    let new = vec![
        Item::Notice(Notice::Folder),
        Item::Notice(Notice::Unrecognized),
    ];
    let restored = restore(&old, offset(30, 3.0), &new);
    assert_eq!(restored.item_ix, 1);
    assert_eq!(restored.offset_in_item, px(0.0));
}

#[test]
fn anchor_in_an_empty_old_list_starts_at_the_top() {
    let (snapshot, _) = snapshot_fixture(1, 1, 4);
    let new = build(&snapshot, &Default::default(), true, false);
    let restored = restore(&[], offset(0, 0.0), &new);
    assert_eq!(restored.item_ix, 0);
}

#[test]
fn anchor_find_by_key() {
    let (snapshot, rows) = snapshot_fixture(2, 1, 6);
    let items = build(&snapshot, &rows_map(rows), true, false);
    let key = items[7].key();
    assert_eq!(find(&items, &key), Some(7));
    assert_eq!(find(&items[..7], &key), None);
}
