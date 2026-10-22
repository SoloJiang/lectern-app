//! 「测试」限流.

use std::sync::atomic::Ordering;
use std::time::Duration;

use log::Level;

use super::{Fixture, records, third_party, with_msg};
use crate::engine::limit::{Decision, Table};
use crate::hooks::imp as hooks;

const WINDOW: Duration = Duration::from_secs(10);

fn flood() {
    crate::warn!("limited call site"; i = 1);
}

#[test]
fn third_party_cached_level_follows_filter_changes() {
    let fx = Fixture::new();
    let logging = fx.start(None);
    let gpui = |n: u32| {
        third_party(
            Level::Warn,
            "gpui::window",
            "src/window.rs",
            7,
            format_args!("cached {n}"),
        );
    };
    gpui(1);
    logging.set_verbose(Some(crate::Verbose {
        filter: crate::Filter::parse("gpui=error").0,
        lasts: Duration::from_secs(3600),
    }));
    gpui(2);
    logging.set_verbose(None);
    gpui(3);
    assert!(logging.flush(Duration::from_secs(5)));
    let records = records(fx.path());
    let written: Vec<&str> = records
        .iter()
        .filter(|r| r["tg"] == "gpui")
        .filter_map(|r| r["msg"].as_str())
        .collect();
    // The second record came under `gpui=error`: the cached level of the
    // first did not let it through.
    assert_eq!(written, ["cached 1", "cached 3"], "{records:?}");
}

#[test]
fn third_party_suppressed_counts_reach_the_status() {
    let fx = Fixture::new();
    let logging = fx.start(None);
    for _ in 0..30 {
        third_party(
            Level::Error,
            "gpui::window",
            "src/window.rs",
            9,
            format_args!("counted"),
        );
    }
    // 10 left out, still held by the table entry.
    assert_eq!(logging.status().suppressed, 10);
    fx.clock.advance(WINDOW);
    third_party(
        Level::Error,
        "gpui::window",
        "src/window.rs",
        9,
        format_args!("counted"),
    );
    // Now joined the process total; not counted twice.
    assert_eq!(logging.status().suppressed, 10);
}

#[test]
fn callsite_limit_counts_suppressed() {
    let fx = Fixture::new();
    let logging = fx.start(None);
    for _ in 0..10_000 {
        flood();
    }
    fx.clock.advance(WINDOW);
    flood();
    assert!(logging.flush(Duration::from_secs(5)));
    let records = records(fx.path());
    let written = with_msg(&records, "limited call site");
    assert_eq!(written.len(), 21);
    assert!(written[..20].iter().all(|r| r.get("n").is_none()));
    assert_eq!(written[20]["n"], 9_980);
    assert!(logging.status().suppressed >= 9_980);
}

#[test]
fn third_party_call_site_stays_bounded() {
    let fx = Fixture::new();
    let logging = fx.start(None);
    let tick = Duration::from_secs(1) / 120;
    for second in 0..60 {
        third_party(
            Level::Warn,
            "gpui::other",
            "src/other.rs",
            8,
            format_args!("other call site"),
        );
        for _ in 0..120 {
            third_party(
                Level::Warn,
                "gpui::window",
                "src/window.rs",
                7,
                format_args!("flood"),
            );
            fx.clock.advance(tick);
        }
        let _ = second;
    }
    fx.clock.advance(WINDOW);
    third_party(
        Level::Warn,
        "gpui::window",
        "src/window.rs",
        7,
        format_args!("flood"),
    );
    assert!(logging.flush(Duration::from_secs(5)));
    let records = records(fx.path());
    let flood = with_msg(&records, "flood");
    // Six windows of 20, and the first record of the next window.
    assert_eq!(flood.len(), 6 * 20 + 1);
    let left_out: u64 = flood.iter().filter_map(|r| r["n"].as_u64()).sum();
    assert_eq!(left_out, 60 * 120 - 6 * 20);
    assert_eq!(with_msg(&records, "other call site").len(), 60);
}

#[test]
fn third_party_total_cap() {
    let fx = Fixture::new();
    let logging = fx.start(None);
    for line in 1..=100 {
        third_party(
            Level::Warn,
            "gpui::window",
            "src/window.rs",
            line,
            format_args!("one of many"),
        );
    }
    assert!(logging.flush(Duration::from_secs(5)));
    assert_eq!(with_msg(&records(fx.path()), "one of many").len(), 50);
}

#[test]
fn third_party_errors_bypass_the_total_cap() {
    let fx = Fixture::new();
    let logging = fx.start(None);
    for line in 1..=60 {
        third_party(
            Level::Warn,
            "gpui::window",
            "src/window.rs",
            line,
            format_args!("filling"),
        );
    }
    for line in 101..=130 {
        third_party(
            Level::Error,
            "gpui::window",
            "src/window.rs",
            line,
            format_args!("an error"),
        );
    }
    for _ in 0..30 {
        third_party(
            Level::Error,
            "gpui::window",
            "src/window.rs",
            200,
            format_args!("repeated error"),
        );
    }
    fx.clock.advance(WINDOW);
    third_party(
        Level::Error,
        "gpui::window",
        "src/window.rs",
        200,
        format_args!("repeated error"),
    );
    assert!(logging.flush(Duration::from_secs(5)));
    let records = records(fx.path());
    assert_eq!(with_msg(&records, "filling").len(), 50);
    assert_eq!(with_msg(&records, "an error").len(), 30);
    let repeated = with_msg(&records, "repeated error");
    assert_eq!(repeated.len(), 21);
    assert_eq!(repeated[20]["n"], 10);
}

#[test]
fn third_party_table_overflow_shares_one_limit() {
    let table = Table::new();
    let size = table.len();
    let written = (1..=5000_u32)
        .filter(|key| {
            table.check(key.wrapping_mul(2_654_435_761) | 1, Level::Error, 0) != Decision::Limited
        })
        .count();
    assert!(table.used() <= 256);
    assert_eq!(table.len(), size, "the table does not grow");
    // Each slot writes its key's first record; the keys that found no slot
    // share the overflow entry's 20.
    assert!(written <= 256 + 20, "{written}");
    assert!(written >= 200, "{written}");
}

#[test]
fn limited_calls_do_not_encode() {
    let fx = Fixture::new();
    let logging = fx.start(None);
    for _ in 0..20 {
        flood();
    }
    for line in 1..=50 {
        third_party(
            Level::Warn,
            "gpui::window",
            "src/window.rs",
            line,
            format_args!("fill"),
        );
    }
    assert!(logging.flush(Duration::from_secs(5)));
    hooks::stall_writer(true);
    let before = hooks::ENCODED.load(Ordering::SeqCst);
    for _ in 0..1000 {
        flood();
        third_party(
            Level::Warn,
            "gpui::window",
            "src/window.rs",
            1,
            format_args!("fill"),
        );
        third_party(
            Level::Info,
            "gpui::window",
            "src/window.rs",
            500,
            format_args!("over the total"),
        );
    }
    assert_eq!(hooks::ENCODED.load(Ordering::SeqCst), before);
    hooks::stall_writer(false);
}
