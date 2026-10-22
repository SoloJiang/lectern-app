//! Tests of the local UTC offset. The zone table runs in child processes of
//! this test binary with `TZ` set on the child only: the tests never change
//! this process's environment (changing it is `unsafe` in edition 2024, and
//! `cargo test` runs tests on several threads of one process).

use std::collections::BTreeMap;
use std::process::Command;

use super::{MAX_EAST, MAX_WEST, offset_from_gmtoff, utc_offset};

/// Set by the parent on the child process: the child prints the offsets of
/// the table rows for its `TZ` and does nothing without it.
const CHILD: &str = "LECTERN_UTC_OFFSET_CHILD";
/// Marks the child's result lines in its output.
const MARK: &str = "utc_offset_row";

/// (`TZ`, [(instant, offset)]). IANA zones from the system tzdata, and POSIX
/// rule strings that need no zone file.
const TABLE: &[(&str, &[(i64, i32)])] = &[
    (
        "UTC",
        &[(0, 0), (-1, 0), (1_700_000_000, 0), (1_730_613_600, 0)],
    ),
    (
        "Asia/Shanghai",
        &[(1_700_000_000, 8 * 3600), (1_719_792_000, 8 * 3600)],
    ),
    (
        "America/New_York",
        &[
            // 2024-03-10 02:00 EST becomes 03:00 EDT at 07:00 UTC.
            (1_710_053_999, -5 * 3600),
            (1_710_054_000, -4 * 3600),
            // 2024-11-03 02:00 EDT becomes 01:00 EST at 06:00 UTC.
            (1_730_613_599, -4 * 3600),
            (1_730_613_600, -5 * 3600),
            // Before the epoch: 1969-12-31 18:59:59 EST.
            (-1, -5 * 3600),
        ],
    ),
    (
        "Asia/Kathmandu",
        &[
            (1_700_000_000, 5 * 3600 + 45 * 60),
            // +05:30 until 1986-01-01 00:00 local, then +05:45.
            (504_901_799, 5 * 3600 + 30 * 60),
            (504_901_800, 5 * 3600 + 45 * 60),
        ],
    ),
    (
        "EST5EDT,M3.2.0,M11.1.0",
        &[
            (1_710_053_999, -5 * 3600),
            (1_710_054_000, -4 * 3600),
            (1_730_613_599, -4 * 3600),
            (1_730_613_600, -5 * 3600),
        ],
    ),
    ("<+0545>-5:45", &[(1_700_000_000, 5 * 3600 + 45 * 60)]),
];

/// The child side of `utc_offset_follows_the_zone_table`: prints
/// `utc_offset_row <instant> <offset or none>` for each row of its `TZ`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn utc_offset_child_prints_the_rows_of_its_zone() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    let tz = std::env::var("TZ").expect("the parent sets TZ");
    let (_, rows) = TABLE
        .iter()
        .find(|(zone, _)| *zone == tz)
        .expect("TZ is a table zone");
    for &(instant, _) in *rows {
        match utc_offset(instant) {
            Some(offset) => println!("{MARK} {instant} {offset}"),
            None => println!("{MARK} {instant} none"),
        }
    }
}

/// Runs the child test with `TZ=tz` and returns its rows.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn rows_in_child(tz: &str) -> BTreeMap<i64, Option<i32>> {
    // libtest names tests by their path without the crate name.
    let module = module_path!().split_once("::").unwrap().1;
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            &format!("{module}::utc_offset_child_prints_the_rows_of_its_zone"),
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .env("TZ", tz)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "TZ={tz}: {:?}\n{stdout}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
        .lines()
        // libtest's `test … ` may precede the first row on its line.
        .filter_map(|line| line.split_once(MARK).map(|(_, row)| row))
        .map(|row| {
            let (instant, offset) = row.trim().split_once(' ').unwrap();
            let offset = (offset != "none").then(|| offset.parse().unwrap());
            (instant.parse().unwrap(), offset)
        })
        .collect()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn utc_offset_follows_the_zone_table() {
    for &(tz, rows) in TABLE {
        let expected: BTreeMap<i64, Option<i32>> = rows
            .iter()
            .map(|&(instant, offset)| (instant, Some(offset)))
            .collect();
        // A zone file missing from the system tzdata makes the C library
        // fall back to UTC, which shows up here as a wrong offset.
        assert_eq!(rows_in_child(tz), expected, "TZ={tz}");
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn utc_offset_out_of_range_is_none() {
    // Years that do not fit in `struct tm`'s `int tm_year`, whatever the
    // zone of this process.
    assert_eq!(utc_offset(i64::MAX), None);
    assert_eq!(utc_offset(i64::MIN), None);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn utc_offset_covers_every_millisecond_timestamp() {
    // `LocalZone::offset_at` takes milliseconds: every one of them, turned
    // into seconds, has an offset in this process's zone, whatever it is.
    for unix_secs in [
        i64::MIN.div_euclid(1000),
        -(1 << 40),
        i64::from(i32::MIN),
        -1,
        0,
        1_700_000_000,
        i64::from(i32::MAX) + 1,
        1 << 40,
        i64::MAX.div_euclid(1000),
    ] {
        let offset = utc_offset(unix_secs).unwrap_or_else(|| panic!("{unix_secs}"));
        assert!(
            (MAX_WEST..=MAX_EAST).contains(&i64::from(offset)),
            "{unix_secs}: {offset}"
        );
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn utc_offset_from_many_threads_agrees() {
    let instants: Vec<i64> = (0..2000).map(|i| 1_700_000_000 + i * 86_399 * 7).collect();
    let expected: Vec<Option<i32>> = instants.iter().map(|&t| utc_offset(t)).collect();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                for _ in 0..10 {
                    let got: Vec<Option<i32>> = instants.iter().map(|&t| utc_offset(t)).collect();
                    assert_eq!(got, expected);
                }
            });
        }
    });
}

#[test]
fn utc_offset_gmtoff_range() {
    for (gmtoff, expected) in [
        (0, Some(0)),
        (5 * 3600 + 45 * 60, Some(20_700)),
        (-5 * 3600, Some(-18_000)),
        // Local mean time offsets are not whole minutes, e.g. Kathmandu's
        // +05:41:16 before 1920.
        (20_476, Some(20_476)),
        (MAX_EAST, Some(93_599)),
        (MAX_WEST, Some(-89_999)),
        (MAX_EAST + 1, None),
        (MAX_WEST - 1, None),
        (i64::MAX, None),
        (i64::MIN, None),
    ] {
        assert_eq!(offset_from_gmtoff(gmtoff), expected, "{gmtoff}");
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[test]
fn utc_offset_is_none_elsewhere() {
    assert_eq!(utc_offset(0), None);
}
