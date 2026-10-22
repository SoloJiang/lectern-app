//! Facts about the running system (「日志目录与系统版本」 in
//! docs/design/platform.md). Only the OS version so far, for the header line
//! of every log file (docs/design/logging.md「文件」).

#[cfg(test)]
mod tests;

/// The longest product version accepted, in bytes. Real ones are like
/// `15.1` or `26.0.1`.
#[cfg(any(target_os = "macos", test))]
const MAX_VERSION_LEN: usize = 32;

/// The OS name and version, such as `macOS 15.1`: `sysctl
/// kern.osproductversion` on macOS.
///
/// None when the value cannot be read or is not a version (anything but
/// ASCII digits and dots, starting with a digit, at most 32 bytes), and on
/// every other platform: there the build exists only for CI, and `app`, the
/// only caller, is macOS only. One system call, no file IO and no
/// allocation besides the returned string; any thread may call it.
pub fn os_version() -> Option<String> {
    imp::os_version()
}

/// The product version as `sysctl` returns it (with or without its
/// trailing NUL) turned into `macOS <version>`; None for anything else.
/// Compiled for tests everywhere: it only checks bytes.
#[cfg(any(target_os = "macos", test))]
fn mac_os_version(raw: &[u8]) -> Option<String> {
    let end = raw.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    let version = &raw[..end];
    let valid = version.len() <= MAX_VERSION_LEN
        && version.first().is_some_and(u8::is_ascii_digit)
        && version.iter().all(|&b| b.is_ascii_digit() || b == b'.');
    if !valid {
        return None;
    }
    // Only ASCII digits and dots are left.
    let version = std::str::from_utf8(version).ok()?;
    Some(format!("macOS {version}"))
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ptr;

    use super::{MAX_VERSION_LEN, mac_os_version};

    pub(super) fn os_version() -> Option<String> {
        // Room for a longer value than any accepted one, so that an
        // overlong value is read whole and then rejected rather than failing
        // with `ENOMEM`.
        let mut buf = [0u8; 2 * MAX_VERSION_LEN];
        let mut len: libc::size_t = buf.len();
        // SAFETY: the name is a NUL-terminated C string literal; `buf` is
        // valid for writes of `len` bytes and `len` is a valid, exclusive
        // pointer for the call, which writes at most `len` bytes and stores
        // the number written back into `len`. A null `newp` with `newlen`
        // 0 only reads the value.
        let rc = unsafe {
            libc::sysctlbyname(
                c"kern.osproductversion".as_ptr(),
                buf.as_mut_ptr().cast::<libc::c_void>(),
                &mut len,
                ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return None;
        }
        mac_os_version(buf.get(..len)?)
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub(super) fn os_version() -> Option<String> {
        None
    }
}
