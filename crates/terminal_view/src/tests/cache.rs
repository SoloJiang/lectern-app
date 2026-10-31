//! The row cache (docs/design/terminal_view.md「行缓存」): two generations,
//! so it never holds more than two frames of rows however far the scrollback
//! has been scrolled.

use std::cell::Cell;

use gpui::px;

use crate::layout::{RowCache, RowLayout};
use crate::testing::grid_row;

fn row(text: &str) -> lectern_services::terminal::GridRow {
    grid_row(text, 20, &[], false)
}

#[test]
fn rows_of_the_last_two_frames_are_held() {
    let mut cache = RowCache::default();
    let made = Cell::new(0);
    let layout = || {
        made.set(made.get() + 1);
        RowLayout::default()
    };
    let (a, b, c) = (row("a"), row("b"), row("c"));
    let width = px(7.2);

    // Frame 1: a and b are laid out.
    cache.get(&a, width, layout);
    cache.get(&b, width, layout);
    cache.end_frame();
    assert_eq!(made.get(), 2);
    assert_eq!(cache.take_laid_out(), 2);

    // Frame 2 has b again and a new c: b comes from the last frame.
    cache.get(&b, width, layout);
    cache.get(&c, width, layout);
    cache.end_frame();
    assert_eq!(made.get(), 3);
    assert_eq!(cache.len(), 2, "a is gone; b and c are held");

    // Frame 3 is a: it was dropped, so it is laid out again.
    cache.get(&a, width, layout);
    assert_eq!(made.get(), 4);
}

#[test]
fn a_row_used_twice_in_a_frame_is_laid_out_once() {
    let mut cache = RowCache::default();
    let made = Cell::new(0);
    let layout = || {
        made.set(made.get() + 1);
        RowLayout::default()
    };
    let blank = row("x");
    let width = px(7.2);
    let first = cache.get(&blank, width, layout);
    let second = cache.get(&blank, width, layout);
    assert!(std::sync::Arc::ptr_eq(&first, &second));
    assert_eq!(made.get(), 1);
}

/// A scrollback of ten thousand rows scrolled through leaves two frames' rows.
#[test]
fn the_cache_does_not_grow_with_the_scrollback() {
    let mut cache = RowCache::default();
    let width = px(7.2);
    for top in 0..10_000 {
        for line in 0..40 {
            let text = format!("line {}", top + line);
            cache.get(&row(&text), width, RowLayout::default);
        }
        cache.end_frame();
    }
    assert!(cache.len() <= 80, "{}", cache.len());
}

#[test]
fn a_new_cell_width_empties_the_cache() {
    let mut cache = RowCache::default();
    let made = Cell::new(0);
    let layout = || {
        made.set(made.get() + 1);
        RowLayout::default()
    };
    let a = row("a");
    cache.get(&a, px(7.2), layout);
    cache.end_frame();
    cache.get(&a, px(8.4), layout);
    assert_eq!(made.get(), 2);
}

#[test]
fn rows_with_the_same_content_share_a_key() {
    assert_eq!(row("same").key, row("same").key);
    assert_ne!(row("same").key, row("other").key);
    let styled = grid_row(
        "same",
        20,
        &[(0, 2, {
            let mut style = crate::testing::plain();
            style.fg.r ^= 1;
            style
        })],
        false,
    );
    assert_ne!(row("same").key, styled.key, "the style is part of the key");
}
