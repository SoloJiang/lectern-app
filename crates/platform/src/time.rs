//! The local time zone (「本地时区的偏移」 in docs/design/platform.md):
//! `usage`'s `LocalZone` splits time windows into the user's calendar days
//! with it (docs/design/usage.md, prerequisite B).

#[cfg(test)]
mod tests;

/// The offset of local time from UTC at the instant `unix_secs` (seconds
/// since the Unix epoch), in seconds, east positive, daylight saving time
/// included: the `tm_gmtoff` that `localtime_r` gives for that instant, so
/// each side of a DST transition gets its own offset.
///
/// None when `localtime_r` fails (the year does not fit in a `struct tm`,
/// such as `i64::MAX`; every millisecond timestamp divided by 1000 fits),
/// when the offset is outside the range a TZif file can hold (RFC 8536:
/// −89 999 to 93 599 s), and on platforms other than macOS and Linux.
///
/// The time zone is the C library's: `TZ`, or the system zone when `TZ`
/// is unset. The C library loads the zone's rules on first use and keeps
/// them; nothing else is cached. Any thread may call it; it takes the C
/// library's time zone lock and does not block otherwise.
pub fn utc_offset(unix_secs: i64) -> Option<i32> {
    imp::utc_offset(unix_secs)
}

/// The longest offset east of UTC a TZif file can hold (RFC 8536 §3.2:
/// `utoff` up to 25:59:59).
#[cfg(any(target_os = "linux", target_os = "macos", test))]
const MAX_EAST: i64 = 93_599;
/// The longest offset west of UTC a TZif file can hold (−24:59:59).
#[cfg(any(target_os = "linux", target_os = "macos", test))]
const MAX_WEST: i64 = -89_999;

/// `tm_gmtoff` as the offset [`utc_offset`] returns: None outside the TZif
/// range. Compiled for tests everywhere: it only compares integers.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn offset_from_gmtoff(gmtoff: i64) -> Option<i32> {
    if (MAX_WEST..=MAX_EAST).contains(&gmtoff) {
        i32::try_from(gmtoff).ok()
    } else {
        None
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod imp {
    use std::mem::MaybeUninit;

    use super::offset_from_gmtoff;

    pub(super) fn utc_offset(unix_secs: i64) -> Option<i32> {
        // `time_t` is 64 bits on every target Lectern builds for; this only
        // fails where it is 32 bits.
        #[allow(
            clippy::useless_conversion,
            reason = "time_t is i64 on 64-bit targets but i32 on 32-bit Linux"
        )]
        let time = libc::time_t::try_from(unix_secs).ok()?;
        let mut tm = MaybeUninit::<libc::tm>::zeroed();
        // SAFETY: `time` and `tm` are valid, live locals for the call:
        // `localtime_r` reads one `time_t` through the first pointer and
        // writes one `struct tm` through the second, and returns either that
        // second pointer or null. It is the reentrant form: it keeps no
        // static result buffer, and both C libraries guard their time zone
        // state with a lock of their own.
        let result = unsafe { libc::localtime_r(&time, tm.as_mut_ptr()) };
        if result.is_null() {
            return None;
        }
        // SAFETY: all-zero bytes are a valid `struct tm` (integers and a
        // nullable `tm_zone` pointer), and `localtime_r` succeeded, so every
        // field it sets is set.
        let tm = unsafe { tm.assume_init() };
        #[allow(
            clippy::useless_conversion,
            reason = "tm_gmtoff is a C long: i64 on 64-bit targets, i32 on 32-bit Linux"
        )]
        let gmtoff = i64::from(tm.tm_gmtoff);
        offset_from_gmtoff(gmtoff)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod imp {
    pub(super) fn utc_offset(_unix_secs: i64) -> Option<i32> {
        None
    }
}
