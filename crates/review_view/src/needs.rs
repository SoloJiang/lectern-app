//! Which rows and texts the view asks `review` to prepare
//! (docs/design/review_view.md「可见范围与 Prepare」): the keys of the
//! visible items and of one screen of items before and after them, nearest
//! first. A pure function of the items and the visible range.

use std::collections::HashSet;
use std::ops::Range;

use lectern_services::review::{Need, ReviewSnapshot, TextKey};

use crate::items::Item;

/// The most keys one request holds. `review` keeps the first ones within its
/// own byte limit (docs/design/review.md); the nearest are first.
pub const CAP: usize = 256;

/// The text a `Prompt` item shows, once it has arrived.
pub fn prompt_key(snapshot: &ReviewSnapshot, turn: u32) -> Option<TextKey> {
    snapshot.turns.as_deref()?.get(turn as usize)?.prompt
}

/// The needs of the items in `visible` and `visible.len()` items before and
/// after it, the visible ones first, then by distance. The same key appears
/// once. Items that are not diffs or texts ask for nothing.
pub fn compute(
    items: &[Item],
    snapshot: &ReviewSnapshot,
    visible: &Range<usize>,
    cap: usize,
) -> Vec<Need> {
    let count = visible.len().max(1);
    let lo = visible.start.saturating_sub(count);
    let hi = visible.end.saturating_add(count).min(items.len());
    let mut found: Vec<(usize, usize, Need)> = Vec::new();
    for (ix, item) in items.iter().enumerate().take(hi).skip(lo) {
        let need = match item {
            Item::Placeholder { rows, .. }
            | Item::Hunk { rows, .. }
            | Item::Row { rows, .. }
            | Item::Fold { rows, .. } => Some(Need::Rows(rows.clone())),
            Item::Text { key, .. } | Item::Injected { key, .. } => Some(Need::Text(*key)),
            Item::Prompt { ix: turn, .. } => prompt_key(snapshot, *turn).map(Need::Text),
            _ => None,
        };
        if let Some(need) = need {
            let distance = if ix < visible.start {
                visible.start - ix
            } else if ix >= visible.end {
                ix - visible.end + 1
            } else {
                0
            };
            found.push((distance, ix, need));
        }
    }
    found.sort_by_key(|(distance, ix, _)| (*distance, *ix));
    let mut seen = HashSet::new();
    let mut needs = Vec::new();
    for (_, _, need) in found {
        if needs.len() >= cap {
            break;
        }
        if seen.insert(need.clone()) {
            needs.push(need);
        }
    }
    needs
}
