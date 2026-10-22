//! `normalize_*`, `round_*`, `window_*` (docs/design/usage.md「测试」):
//! the pure functions.

use lectern_services::session::RawTokens;
use lectern_services::usage::{LocalDate, Tokens, Window};

use super::support::{DAY, HOUR, OCT_20};
use crate::LocalZone;
use crate::round::{MICRO, largest_remainder, split};
use crate::testing::TableZone;
use crate::tokens::normalize;
use crate::window::{self, BUCKET_MS};

fn d(year: i32, month: u8, day: u8) -> LocalDate {
    LocalDate { year, month, day }
}

#[test]
fn normalize_anthropic_input_excludes_the_cache() {
    let t = normalize(&RawTokens::Anthropic {
        input: 12,
        output: 30,
        cache_read: 400,
        cache_write: 50,
        cache_write_5m: 20,
        cache_write_1h: 30,
    });
    assert_eq!(
        t,
        Tokens {
            input: 12,
            output: 30,
            cache_read: 400,
            cache_write: 50
        }
    );
}

#[test]
fn normalize_anthropic_cache_write_falls_back_to_the_split() {
    let t = normalize(&RawTokens::Anthropic {
        input: 1,
        output: 2,
        cache_read: 3,
        cache_write: 0,
        cache_write_5m: 20,
        cache_write_1h: 7,
    });
    assert_eq!(t.cache_write, 27);
    assert_eq!(t.total(), 1 + 2 + 3 + 27);
}

#[test]
fn normalize_openai_input_minus_cached() {
    let t = normalize(&RawTokens::OpenAi {
        input: 1000,
        cached_input: 600,
        cache_write: 0,
        output: 80,
        reasoning_output: 50,
    });
    // Reasoning is already inside `output`; cached moves to cache read.
    assert_eq!(
        t,
        Tokens {
            input: 400,
            output: 80,
            cache_read: 600,
            cache_write: 0
        }
    );
}

#[test]
fn normalize_openai_cached_never_exceeds_input() {
    let t = normalize(&RawTokens::OpenAi {
        input: 30,
        cached_input: 40,
        cache_write: 5,
        output: 1,
        reasoning_output: 0,
    });
    assert_eq!(
        t,
        Tokens {
            input: 0,
            output: 1,
            cache_read: 30,
            cache_write: 5
        }
    );
}

#[test]
fn round_keeps_the_sum() {
    let parts = [1_250_000, 2_250_000, 333_333, 666_667, 4_500_000];
    let sum: u64 = parts.iter().sum();
    let out = largest_remainder(&parts);
    assert_eq!(out.iter().sum::<u64>(), sum / MICRO);
    // Each row moves by less than one token.
    for (p, o) in parts.iter().zip(&out) {
        assert!(o * MICRO + MICRO > *p && *o * MICRO < p + MICRO);
    }
}

#[test]
fn round_equal_remainders_go_by_index() {
    assert_eq!(
        largest_remainder(&[500_000, 500_000, 500_000, 500_000]),
        [1, 1, 0, 0]
    );
    assert_eq!(largest_remainder(&[2_000_000, 0, 1_000_000]), [2, 0, 1]);
    assert_eq!(largest_remainder(&[]), Vec::<u64>::new());
}

#[test]
fn round_larger_remainder_first() {
    assert_eq!(largest_remainder(&[300_000, 700_000]), [0, 1]);
    assert_eq!(
        largest_remainder(&[1_100_000, 1_200_000, 700_000]),
        [1, 1, 1]
    );
}

#[test]
fn round_split_adds_up_exactly() {
    let parts = split(7, &[1, 1, 1]);
    assert_eq!(parts, [2_333_334, 2_333_333, 2_333_333]);
    assert_eq!(parts.iter().sum::<u64>(), 7 * MICRO);
    let parts = split(1001, &[3, 5, 0, 9]);
    assert_eq!(parts.iter().sum::<u64>(), 1001 * MICRO);
    assert_eq!(parts[2], 0);
    // And back to whole tokens, still adding up.
    assert_eq!(largest_remainder(&parts).iter().sum::<u64>(), 1001);
}

#[test]
fn round_split_edge_cases() {
    assert_eq!(split(5, &[0, 0]), [2_500_000, 2_500_000]);
    assert_eq!(split(5, &[]), Vec::<u64>::new());
    assert_eq!(split(0, &[1, 2]), [0, 0]);
    let huge = split(u64::MAX, &[1, 1]);
    assert_eq!(
        huge.iter().sum::<u64>(),
        crate::round::MAX_SPLIT_TOKENS * MICRO
    );
}

#[test]
fn window_day_numbers_round_trip() {
    assert_eq!(window::day_number(d(1970, 1, 1)), 0);
    assert_eq!(window::day_number(d(2000, 3, 1)), 11_017);
    assert_eq!(window::day_number(d(2026, 10, 20)) * DAY, OCT_20);
    for n in [-800_000, -1, 0, 59, 60, 11_016, 20_746, 2_932_896] {
        assert_eq!(window::day_number(window::date_of(n)), n);
    }
    assert_eq!(
        window::date_of(window::day_number(d(2024, 2, 29))),
        d(2024, 2, 29)
    );
}

#[test]
fn window_days_end_today() {
    let zone = TableZone::fixed(8 * 3600);
    // 2026-10-20T20:00Z is 2026-10-21 04:00 at +08:00.
    let days = window::days(Window::D7, OCT_20 + 20 * HOUR, &zone);
    assert_eq!(days.len(), 7);
    assert_eq!(days[6].0, d(2026, 10, 21));
    assert_eq!(days[0].0, d(2026, 10, 15));
    assert_eq!(days[6].1.start, OCT_20 + 16 * HOUR);
    for w in days.windows(2) {
        assert_eq!(w[0].1.end, w[1].1.start);
    }
    assert_eq!(window::days(Window::D14, OCT_20, &zone).len(), 14);
    assert_eq!(window::days(Window::D30, OCT_20, &zone).len(), 30);
}

/// America/New_York in 2026: EDT from 2026-03-08T07:00Z, EST again from
/// 2026-11-01T06:00Z.
fn new_york() -> TableZone {
    let spring = window::day_number(d(2026, 3, 8)) * DAY + 7 * HOUR;
    let fall = window::day_number(d(2026, 11, 1)) * DAY + 6 * HOUR;
    TableZone::new(&[
        (i64::MIN, -5 * 3600),
        (spring, -4 * 3600),
        (fall, -5 * 3600),
    ])
}

#[test]
fn window_dst_days_are_23_and_25_hours() {
    let zone = new_york();
    let noon = |day: LocalDate| window::day_number(day) * DAY + 17 * HOUR;
    let days = window::last_days(3, noon(d(2026, 3, 9)), &zone);
    let lengths: Vec<(LocalDate, i64)> = days
        .iter()
        .map(|(d, r)| (*d, (r.end - r.start) / HOUR))
        .collect();
    assert_eq!(
        lengths,
        [
            (d(2026, 3, 7), 24),
            (d(2026, 3, 8), 23),
            (d(2026, 3, 9), 24)
        ]
    );
    let days = window::last_days(3, noon(d(2026, 11, 2)), &zone);
    let lengths: Vec<(LocalDate, i64)> = days
        .iter()
        .map(|(d, r)| (*d, (r.end - r.start) / HOUR))
        .collect();
    assert_eq!(
        lengths,
        [
            (d(2026, 10, 31), 24),
            (d(2026, 11, 1), 25),
            (d(2026, 11, 2), 24)
        ]
    );
    // Midnight before the spring change is still EST.
    assert_eq!(
        window::start(d(2026, 3, 8), &zone),
        window::day_number(d(2026, 3, 8)) * DAY + 5 * HOUR
    );
}

#[test]
fn window_kathmandu_quarter_hour_offset() {
    let zone = TableZone::fixed(5 * 3600 + 45 * 60);
    let start = window::start(d(2026, 10, 20), &zone);
    assert_eq!(start, OCT_20 - 5 * HOUR - 45 * 60_000);
    // Days start on bucket boundaries, so a bucket never straddles midnight.
    for (_, r) in window::days(Window::D30, OCT_20, &zone) {
        assert_eq!(r.start % BUCKET_MS, 0);
        assert_eq!(r.end % BUCKET_MS, 0);
    }
    assert_eq!(window::local_date(start - 1, &zone), d(2026, 10, 19));
    assert_eq!(window::local_date(start, &zone), d(2026, 10, 20));
}

#[test]
fn window_skipped_midnight_starts_at_the_change() {
    // Clocks jump from 00:00 to 01:00 at -03:00 -> -02:00 (as in zones that
    // used to change at midnight): 2026-10-20 starts at the change.
    let change = OCT_20 + 3 * HOUR;
    let zone = TableZone::new(&[(i64::MIN, -3 * 3600), (change, -2 * 3600)]);
    assert_eq!(window::start(d(2026, 10, 20), &zone), change);
    assert_eq!(window::local_date(change - 1, &zone), d(2026, 10, 19));
    assert_eq!(window::local_date(change, &zone), d(2026, 10, 20));
    // Midnight that happens twice (00:30 at -02:00 goes back to 23:30 at
    // -03:00): the day starts at the first one.
    let back = OCT_20 + 2 * HOUR + 30 * 60_000;
    let zone = TableZone::new(&[(i64::MIN, -2 * 3600), (back, -3 * 3600)]);
    assert_eq!(window::start(d(2026, 10, 20), &zone), OCT_20 + 2 * HOUR);
    assert_eq!(zone.offset_at(back), -3 * 3600);
}

#[test]
fn window_next_midnight() {
    let zone = TableZone::fixed(8 * 3600);
    assert_eq!(window::next_midnight(OCT_20, &zone), OCT_20 + 16 * HOUR);
    assert_eq!(
        window::next_midnight(OCT_20 + 16 * HOUR, &zone),
        OCT_20 + 40 * HOUR
    );
}
