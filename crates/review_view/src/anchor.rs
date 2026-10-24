//! Finding the scroll position again after the items were rebuilt
//! (docs/design/review_view.md「滚动位置与选中项」): the item at the top of
//! the list is found by its key; when it is gone, the first item after it
//! (in the old list) that still exists; when none does, the old index
//! clamped to the new length. The offset inside the item is kept only when
//! the same item is found.

use std::collections::HashMap;

use gpui::{ListOffset, Pixels, px};

use crate::items::{Item, ItemKey};

/// The index of the item with `key`, if there is one. A diff row whose two
/// line numbers changed shape (the same removed line, now beside its partner
/// or alone, after the diff mode changed) is found by the line it has on a
/// side that is still there.
pub fn find(items: &[Item], key: &ItemKey) -> Option<usize> {
    if let Some(ix) = items.iter().position(|item| item.key() == *key) {
        return Some(ix);
    }
    let ItemKey::Row { rows, old, new } = key else {
        return None;
    };
    items.iter().position(|item| match item {
        Item::Row {
            rows: at,
            old: item_old,
            new: item_new,
            ..
        } => {
            at == rows && ((old.is_some() && item_old == old) || (new.is_some() && item_new == new))
        }
        _ => false,
    })
}

/// Where the list scrolls to once `new` replaces `old` and was at `at`.
pub fn restore(old: &[Item], at: ListOffset, new: &[Item]) -> ListOffset {
    let last = new.len().saturating_sub(1);
    let Some(anchor) = old.get(at.item_ix) else {
        return ListOffset {
            item_ix: at.item_ix.min(last),
            offset_in_item: px(0.0),
        };
    };
    let key = anchor.key();
    if let Some(ix) = find(new, &key) {
        return ListOffset {
            item_ix: ix,
            offset_in_item: at.offset_in_item,
        };
    }
    let index: HashMap<ItemKey, usize> = new
        .iter()
        .enumerate()
        .map(|(ix, item)| (item.key(), ix))
        .collect();
    for item in &old[at.item_ix + 1..] {
        if let Some(&ix) = index.get(&item.key()) {
            return ListOffset {
                item_ix: ix,
                offset_in_item: Pixels::ZERO,
            };
        }
    }
    ListOffset {
        item_ix: at.item_ix.min(last),
        offset_in_item: Pixels::ZERO,
    }
}
