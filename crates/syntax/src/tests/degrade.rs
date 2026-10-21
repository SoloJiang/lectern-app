//! `degrade_*`: plain text for binary or non-UTF-8 bytes in the file's head
//! or the window, and for windows slower than `SLOW_WINDOW` twice in a row
//! (durations given by the test, in place of a clock).

use std::time::Duration;

use crate::Rules;
use crate::highlight::SlowWindows;
use crate::limits::{SLOW_WINDOW, WINDOW, WINDOW_MAX};
use crate::testing;
use crate::window::{Encoding, Target, first_read, plan};

/// The first screen of `text` at `target`, read as the first read would.
fn first_screen(text: &[u8], target: u64) -> crate::window::Plan {
    let size = text.len() as u64;
    let around = first_read(size, target).pop().unwrap();
    plan(
        Rules::Code,
        &text[..(size.min(8 << 10)) as usize],
        &text[around.start as usize..around.end as usize],
        around.start,
        size,
        Target::Byte(target),
        60,
    )
}

#[test]
fn degrade_nul_in_the_head() {
    let mut text = testing::ts(2 << 20, 0);
    text[100] = 0;
    let plan = first_screen(&text, 1 << 20);
    assert_eq!(plan.encoding, Encoding::NotText);
    assert_eq!(plan.window, None);
}

#[test]
fn degrade_nul_or_invalid_utf8_in_the_window() {
    let target = 1 << 20;
    for bad in [&[0u8][..], &[0xFF], &[0xC3, 0x28]] {
        let mut text = testing::ts(2 << 20, 0);
        let at = target as usize - 2000;
        text[at..at + bad.len()].copy_from_slice(bad);
        let plan = first_screen(&text, target);
        assert_eq!(plan.encoding, Encoding::NotText, "{bad:?}");
        assert_eq!(plan.window, None);
        assert!(!plan.visible.is_empty(), "the text is still shown");
    }
}

/// Only the head and the window are looked at: a NUL elsewhere is not
/// seen on the first screen (the full read finds it later).
#[test]
fn degrade_only_the_head_and_the_window() {
    let mut text = testing::ts(2 << 20, 0);
    text[(1 << 20) - (40 << 10)] = 0;
    let plan = first_screen(&text, 1 << 20);
    assert_eq!(plan.encoding, Encoding::Utf8);
    assert!(plan.window.is_some());
}

#[test]
fn degrade_slow_windows_twice_in_a_row() {
    let slow = SLOW_WINDOW + Duration::from_millis(1);
    let fast = Duration::from_millis(20);
    let mut windows = SlowWindows::default();
    // A fast window between two slow ones starts the count again.
    for spent in [slow, fast, slow, fast] {
        assert!(!windows.record(WINDOW, spent));
    }
    assert!(!windows.record(WINDOW, slow));
    assert!(windows.record(WINDOW, slow));
    // Off for the rest of this opening.
    assert!(windows.record(WINDOW, fast));
    assert!(windows.plain());
    // Exactly the limit is not slow; a widened window has four times the
    // time; a short last window has the whole of it.
    let mut windows = SlowWindows::default();
    assert!(!windows.record(WINDOW, SLOW_WINDOW));
    assert!(!windows.record(WINDOW, SLOW_WINDOW));
    assert!(!windows.record(WINDOW_MAX, 3 * SLOW_WINDOW));
    assert!(!windows.record(WINDOW_MAX, 3 * SLOW_WINDOW));
    assert!(!windows.record(100, slow));
    assert!(windows.record(100, slow));
}
