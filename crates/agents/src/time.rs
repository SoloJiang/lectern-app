//! RFC 3339 时间戳解析到 UTC 毫秒（docs/design/agents.md「依赖」：不引入 `chrono` / `time`）。

use lectern_services::session::Timestamp;

/// 解析 `2026-10-17T02:46:01.123Z` 一类的时间戳。小数部分可有可无、位数不限（超过毫秒的截断），
/// 时区是 `Z` 或 `±HH:MM`。格式不对时返回 None。
pub(crate) fn parse(s: &str) -> Option<Timestamp> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    if b[13] != b':' || b[16] != b':' {
        return None;
    }
    let year = digits(&b[0..4])?;
    let month = digits(&b[5..7])?;
    let day = digits(&b[8..10])?;
    let hour = digits(&b[11..13])?;
    let minute = digits(&b[14..16])?;
    let second = digits(&b[17..19])?;
    if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let mut i = 19;
    let mut millis = 0i64;
    if b[i] == b'.' {
        i += 1;
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            if i - start < 3 {
                millis = millis * 10 + i64::from(b[i] - b'0');
            }
            i += 1;
        }
        let n = i - start;
        if n == 0 {
            return None;
        }
        for _ in n..3 {
            millis *= 10;
        }
    }
    let offset_minutes = match b.get(i)? {
        b'Z' | b'z' if i + 1 == b.len() => 0,
        sign @ (b'+' | b'-') if i + 6 == b.len() && b[i + 3] == b':' => {
            let h = digits(&b[i + 1..i + 3])?;
            let m = digits(&b[i + 4..i + 6])?;
            if h > 23 || m > 59 {
                return None;
            }
            let total = h * 60 + m;
            if *sign == b'+' { total } else { -total }
        }
        _ => return None,
    };
    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second - offset_minutes * 60;
    Some(Timestamp(secs * 1000 + millis))
}

fn digits(b: &[u8]) -> Option<i64> {
    b.iter().try_fold(0i64, |acc, &c| {
        c.is_ascii_digit().then(|| acc * 10 + i64::from(c - b'0'))
    })
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// 公历日期到 1970-01-01 起的天数（Howard Hinnant 的 `days_from_civil`）。
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_parses_utc_millis() {
        assert_eq!(parse("1970-01-01T00:00:00Z"), Some(Timestamp(0)));
        assert_eq!(parse("1970-01-01T00:00:00.001Z"), Some(Timestamp(1)));
        assert_eq!(
            parse("2026-10-17T02:46:01.123Z"),
            Some(Timestamp(1_792_205_161_123))
        );
        // 位数不足补零、超过截断。
        assert_eq!(parse("1970-01-01T00:00:01.5Z"), Some(Timestamp(1500)));
        assert_eq!(
            parse("1970-01-01T00:00:01.123456789Z"),
            Some(Timestamp(1123))
        );
        assert_eq!(
            parse("2000-02-29T12:00:00+02:00"),
            parse("2000-02-29T10:00:00Z")
        );
        assert_eq!(parse("1969-12-31T23:59:59Z"), Some(Timestamp(-1000)));
    }

    #[test]
    fn time_rejects_malformed() {
        for s in [
            "",
            "2026-10-17",
            "2026-10-17T02:46:01",
            "2026-13-01T00:00:00Z",
            "2026-02-30T00:00:00Z",
            "2026-10-17T24:00:00Z",
            "2026-10-17T02:46:01.Z",
            "2026-10-17T02:46:01Zjunk",
            "2026-10-17T02:46:01+0200",
            "２026-10-17T02:46:01Z",
        ] {
            assert_eq!(parse(s), None, "{s}");
        }
    }
}
