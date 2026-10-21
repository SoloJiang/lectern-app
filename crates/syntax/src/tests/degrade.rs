//! `degrade_*`: plain text for binary or non-UTF-8 bytes in the file's head
//! or the window, and for windows slower than `SLOW_WINDOW` twice in a row
//! (durations given by the test, in place of a clock).

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crate::Rules;
use crate::highlight::{Budget, ChainWindow, Outcome, SlowWindows, Source, Spent};
use crate::limits::{SLOW_WINDOW, SLOW_WINDOW_WALL, WINDOW, WINDOW_MAX};
use crate::testing;
use crate::tests::health::ManualClocks;
use crate::window::{Encoding, Target, first_read, plan};

use super::lang;

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

fn cpu(parsed: u64, cpu: Duration) -> Spent {
    Spent {
        parsed,
        cpu,
        wall: Duration::ZERO,
    }
}

#[test]
fn degrade_slow_windows_twice_in_a_row() {
    let slow = cpu(WINDOW, SLOW_WINDOW + Duration::from_millis(1));
    let fast = cpu(WINDOW, Duration::from_millis(20));
    let mut windows = SlowWindows::default();
    // A fast window between two slow ones starts the count again.
    for spent in [slow, fast, slow, fast] {
        assert!(!windows.record(spent));
    }
    assert!(!windows.record(slow));
    assert!(windows.record(slow));
    // Off for the rest of this opening.
    assert!(windows.record(fast));
    assert!(windows.plain());
    // Exactly the limit is not slow; a widened window (its first attempt
    // and the wide one parsed) has that much more; a short last window has
    // the whole limit.
    let mut windows = SlowWindows::default();
    assert!(!windows.record(cpu(WINDOW, SLOW_WINDOW)));
    assert!(!windows.record(cpu(WINDOW, SLOW_WINDOW)));
    let widened = WINDOW + WINDOW_MAX;
    assert!(!windows.record(cpu(widened, 4 * SLOW_WINDOW)));
    assert!(!windows.record(cpu(widened, 4 * SLOW_WINDOW)));
    assert!(!windows.record(cpu(100, slow.cpu)));
    assert!(windows.record(cpu(100, slow.cpu)));
}

/// Wall time (no CPU clock) is held to the wider limit.
#[test]
fn degrade_slow_windows_on_wall_time() {
    let wall = |d| Spent {
        parsed: WINDOW,
        cpu: Duration::ZERO,
        wall: d,
    };
    let mut windows = SlowWindows::default();
    assert!(!windows.record(wall(SLOW_WINDOW * 5)));
    assert!(!windows.record(wall(SLOW_WINDOW * 5)));
    assert!(!windows.record(wall(SLOW_WINDOW_WALL + Duration::from_millis(1))));
    assert!(windows.record(wall(SLOW_WINDOW_WALL + Duration::from_millis(1))));
}

/// One chain window parsed on the test clocks: `tick` runs at each of
/// tree-sitter's progress callbacks and advances them.
fn window_on(clocks: &Arc<ManualClocks>, tick: &dyn Fn()) -> Spent {
    let health = clocks.health();
    let text = testing::ts(256 << 10, 0);
    let src: Arc<dyn Source> = Arc::new(text);
    let mut window = ChainWindow::new_in(health.handle(), lang("ts"), src, 0);
    let cancel = AtomicBool::new(false);
    let probe = || {
        tick();
        false
    };
    let budget = Budget {
        budget: Duration::from_secs(3600),
        cancel: &cancel,
        yield_probe: Some(&probe),
    };
    assert!(matches!(window.step(budget), Outcome::Done(_)));
    window.spent()
}

/// A batch thread pushed aside by a build: wall time grows by seconds,
/// CPU time by little. The windows keep their highlighting.
#[test]
fn degrade_starved_thread_is_not_slow() {
    let clocks = ManualClocks::new();
    let thread = clocks.new_thread();
    clocks.run_on(&thread);
    let mut windows = SlowWindows::default();
    for _ in 0..3 {
        let spent = window_on(&clocks, &|| {
            clocks.advance_wall(Duration::from_millis(100));
            thread.add_cpu(Duration::from_micros(10));
        });
        assert!(spent.cpu < SLOW_WINDOW, "{spent:?}");
        assert_eq!(spent.wall, Duration::ZERO);
        assert!(!windows.record(spent));
    }
}

/// A genuinely slow parse (the CPU time over the limit) twice in a row:
/// plain text.
#[test]
fn degrade_slow_parse_turns_plain() {
    let clocks = ManualClocks::new();
    let thread = clocks.new_thread();
    clocks.run_on(&thread);
    let mut windows = SlowWindows::default();
    let slow = || window_on(&clocks, &|| thread.add_cpu(Duration::from_millis(50)));
    let spent = slow();
    assert!(spent.cpu > SLOW_WINDOW * 4, "{spent:?}");
    assert!(!windows.record(spent));
    assert!(windows.record(slow()));
}

/// Without a CPU clock the parse's wall time is counted, against
/// `SLOW_WINDOW_WALL`.
#[test]
fn degrade_without_a_cpu_clock_uses_wall_time() {
    let clocks = ManualClocks::new();
    clocks.run_without_cpu_clock();
    let starved = window_on(&clocks, &|| {});
    assert_eq!(
        (starved.cpu, starved.wall),
        (Duration::ZERO, Duration::ZERO)
    );
    let mut windows = SlowWindows::default();
    for _ in 0..2 {
        let spent = window_on(&clocks, &|| clocks.advance_wall(Duration::from_millis(100)));
        assert_eq!(spent.cpu, Duration::ZERO);
        assert!(spent.wall > SLOW_WINDOW_WALL * 4, "{spent:?}");
        windows.record(spent);
    }
    assert!(windows.plain());
}
