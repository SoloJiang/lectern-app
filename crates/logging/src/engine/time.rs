//! UTC dates without a date library: RFC 3339 for records and
//! `verbose.json`, `YYYYMMDDTHHMMSSZ` for file names.

#[cfg(feature = "sink")]
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

/// A UTC time broken down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Civil {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub millis: u32,
}

pub(crate) fn civil(t: SystemTime) -> Civil {
    let since = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since.as_secs() as i64;
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400) as u32;
    Civil {
        year,
        month,
        day,
        hour: rem / 3600,
        minute: rem / 60 % 60,
        second: rem % 60,
        millis: since.subsec_millis(),
    }
}

/// Days since 1970-01-01 to (year, month, day) (H. Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(feature = "sink")]
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(feature = "sink")]
fn from_civil(c: Civil) -> Option<SystemTime> {
    let valid = (1..=12).contains(&c.month)
        && (1..=31).contains(&c.day)
        && c.hour < 24
        && c.minute < 60
        && c.second < 61
        && c.year >= 1970;
    if !valid {
        return None;
    }
    let days = days_from_civil(c.year, c.month, c.day);
    let secs = days * 86_400 + i64::from(c.hour * 3600 + c.minute * 60 + c.second);
    Some(
        UNIX_EPOCH
            + Duration::from_secs(u64::try_from(secs).ok()?)
            + Duration::from_millis(u64::from(c.millis)),
    )
}

/// `2026-10-22T03:27:01.123Z`, 24 bytes.
pub(crate) fn rfc3339_ms(t: SystemTime) -> [u8; 24] {
    let c = civil(t);
    let mut out = *b"0000-00-00T00:00:00.000Z";
    put(&mut out[0..4], c.year.clamp(0, 9999) as u32);
    put(&mut out[5..7], c.month);
    put(&mut out[8..10], c.day);
    put(&mut out[11..13], c.hour);
    put(&mut out[14..16], c.minute);
    put(&mut out[17..19], c.second);
    put(&mut out[20..23], c.millis);
    out
}

/// `2026-10-23T03:27:01Z`.
#[cfg(feature = "sink")]
pub(crate) fn rfc3339_s(t: SystemTime) -> String {
    let ms = rfc3339_ms(t);
    let mut s = String::from_utf8_lossy(&ms[..19]).into_owned();
    s.push('Z');
    s
}

/// `20261022T032701Z`.
#[cfg(feature = "sink")]
pub(crate) fn file_stamp(t: SystemTime) -> String {
    let c = civil(t);
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        c.year.clamp(0, 9999),
        c.month,
        c.day,
        c.hour,
        c.minute,
        c.second
    )
}

#[cfg(feature = "sink")]
fn digits(s: &str) -> Option<u32> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok())?
}

/// Parses [`rfc3339_s`]'s form (seconds, `Z`).
#[cfg(feature = "sink")]
pub(crate) fn parse_rfc3339_s(s: &str) -> Option<SystemTime> {
    let b = s.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    from_civil(Civil {
        year: i64::from(digits(&s[0..4])?),
        month: digits(&s[5..7])?,
        day: digits(&s[8..10])?,
        hour: digits(&s[11..13])?,
        minute: digits(&s[14..16])?,
        second: digits(&s[17..19])?,
        millis: 0,
    })
}

/// Parses [`file_stamp`]'s form.
#[cfg(feature = "sink")]
pub(crate) fn parse_file_stamp(s: &str) -> Option<SystemTime> {
    let b = s.as_bytes();
    if b.len() != 16 || b[8] != b'T' || b[15] != b'Z' {
        return None;
    }
    from_civil(Civil {
        year: i64::from(digits(&s[0..4])?),
        month: digits(&s[4..6])?,
        day: digits(&s[6..8])?,
        hour: digits(&s[9..11])?,
        minute: digits(&s[11..13])?,
        second: digits(&s[13..15])?,
        millis: 0,
    })
}

fn put(out: &mut [u8], mut v: u32) {
    for b in out.iter_mut().rev() {
        *b = b'0' + (v % 10) as u8;
        v /= 10;
    }
}
