/// The compact form of a count, used for token counts: `950`, `12.3K`, `4.5M`,
/// `1.2B`. Both languages write it the same way (no 万).
///
/// Below 1000 the number is printed as is. Above, it is rounded half up to
/// one decimal of the largest unit that keeps it under 1000 after rounding;
/// a trailing `.0` is dropped.
///
/// ```
/// use lectern_i18n::count_compact;
///
/// assert_eq!(count_compact(950), "950");
/// assert_eq!(count_compact(12_345), "12.3K");
/// assert_eq!(count_compact(999_949), "999.9K");
/// assert_eq!(count_compact(999_950), "1M");
/// assert_eq!(count_compact(4_500_000), "4.5M");
/// ```
pub fn count_compact(n: u64) -> String {
    const UNITS: [(u128, &str); 3] = [(1_000, "K"), (1_000_000, "M"), (1_000_000_000, "B")];
    if n < 1000 {
        return n.to_string();
    }
    let n = u128::from(n);
    let mut unit = UNITS
        .iter()
        .rposition(|&(size, _)| n >= size)
        .expect("n >= 1000");
    loop {
        let (size, suffix) = UNITS[unit];
        let tenths = (n * 10 + size / 2) / size;
        if tenths >= 10_000 && unit + 1 < UNITS.len() {
            unit += 1;
            continue;
        }
        return match tenths % 10 {
            0 => format!("{}{suffix}", tenths / 10),
            decimal => format!("{}.{decimal}{suffix}", tenths / 10),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundaries() {
        let text = [
            0,
            1,
            999,
            1000,
            1049,
            1050,
            1950,
            12_345,
            99_999,
            999_949,
            999_950,
            1_000_000,
            1_049_999,
            4_500_000,
            999_949_999,
            999_950_000,
            1_000_000_000,
            1_234_567_890_123,
            u64::MAX,
        ]
        .iter()
        .map(|&n| format!("{n:>20} {}", count_compact(n)))
        .collect::<Vec<_>>()
        .join("\n");
        insta::assert_snapshot!(text);
    }
}
