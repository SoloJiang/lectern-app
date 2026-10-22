//! Local calendar days and the 15-minute UTC buckets the facts are summed in
//! (docs/design/usage.md「取整与时间窗口」).
//!
//! Every UTC offset in use is a multiple of 15 minutes, and so is every
//! transition instant, so a bucket never straddles a local midnight: a
//! bucket belongs to the day its start falls in.

use std::ops::Range;

use lectern_services::usage::{LocalDate, Window};

use crate::LocalZone;

/// The width of a bucket: 15 minutes in milliseconds.
pub const BUCKET_MS: i64 = 15 * 60 * 1000;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// The bucket of a UTC millisecond timestamp.
pub fn bucket(at_ms: i64) -> i64 {
    at_ms.div_euclid(BUCKET_MS)
}

/// How many local days a window has, today included.
pub fn len(window: Window) -> u32 {
    match window {
        Window::D7 => 7,
        Window::D14 => 14,
        Window::D30 => 30,
    }
}

/// Every local calendar day of the window and its UTC range (milliseconds,
/// half open), oldest first, ending with today. A day with a DST transition
/// is 23 or 25 hours long.
pub fn days(window: Window, now_ms: i64, zone: &dyn LocalZone) -> Vec<(LocalDate, Range<i64>)> {
    last_days(len(window), now_ms, zone)
}

/// The last `n` local days up to today, as [`days`].
pub fn last_days(n: u32, now_ms: i64, zone: &dyn LocalZone) -> Vec<(LocalDate, Range<i64>)> {
    let today = day_number(local_date(now_ms, zone));
    let first = today - i64::from(n.saturating_sub(1));
    (first..=today)
        .map(|d| {
            let date = date_of(d);
            (date, start_of(d, zone)..start_of(d + 1, zone))
        })
        .collect()
}

/// The local date of an instant.
pub fn local_date(at_ms: i64, zone: &dyn LocalZone) -> LocalDate {
    let offset = i64::from(zone.offset_at(at_ms)) * 1000;
    date_of((at_ms + offset).div_euclid(DAY_MS))
}

/// The UTC instant a local date starts at.
pub fn start(date: LocalDate, zone: &dyn LocalZone) -> i64 {
    start_of(day_number(date), zone)
}

/// The first instant whose local date is day `d` (days since 1970-01-01).
///
/// Local midnight `m` is at `m - offset` for the offset in force then. Two
/// candidate offsets, those a day before and a day after, cover a day with
/// one transition: a candidate is consistent when the zone gives that offset
/// at the instant it yields. With two consistent instants (midnight happens
/// twice, clocks went back) the earlier wins; with none (midnight was
/// skipped, clocks went forward) the day starts at the transition, which is
/// the later candidate.
fn start_of(d: i64, zone: &dyn LocalZone) -> i64 {
    let midnight = d * DAY_MS;
    let mut consistent: Option<i64> = None;
    let mut latest = i64::MIN;
    for probe in [midnight - DAY_MS, midnight + DAY_MS] {
        let offset = i64::from(zone.offset_at(probe)) * 1000;
        let at = midnight - offset;
        latest = latest.max(at);
        if i64::from(zone.offset_at(at)) * 1000 == offset {
            consistent = Some(consistent.map_or(at, |c: i64| c.min(at)));
        }
    }
    consistent.unwrap_or(latest)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`, public domain).
pub fn day_number(date: LocalDate) -> i64 {
    let (y, m, d) = (
        i64::from(date.year),
        i64::from(date.month),
        i64::from(date.day),
    );
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date of a day number (`civil_from_days`).
pub fn date_of(z: i64) -> LocalDate {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    LocalDate {
        year: i32::try_from(year).unwrap_or(if year < 0 { i32::MIN } else { i32::MAX }),
        month: month as u8,
        day: day as u8,
    }
}

/// The next local midnight after `now_ms`: when the window snapshots move to
/// a new day.
pub fn next_midnight(now_ms: i64, zone: &dyn LocalZone) -> i64 {
    let today = day_number(local_date(now_ms, zone));
    start_of(today + 1, zone)
}
