//! A description of the machine, for the `host` of the `lectern-bench`
//! report (docs/design/platform.md 「主机信息与线程 QoS」).

use std::io;

#[cfg(test)]
mod tests;

/// The longest value accepted, in bytes.
#[cfg(any(target_os = "macos", test))]
const MAX_VALUE_LEN: usize = 64;

/// The description of the machine, written into the `host` of the bench
/// report. Every string is printable ASCII (0x20 to 0x7E), 1 to 64 bytes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HostInfo {
    /// `sysctl hw.model`, such as "Mac14,2"; on a GitHub macOS runner
    /// something like "VirtualMac2,1".
    pub model: String,
    /// `sysctl machdep.cpu.brand_string`, such as "Apple M2".
    pub chip: String,
    /// `sysctl kern.osproductversion`, such as "15.6". Without "macOS"; the
    /// character rules are those of `system::os_version`.
    pub os_version: String,
    /// `sysctl hw.logicalcpu`, greater than 0.
    pub cpus: u32,
}

/// Reads four values with `sysctlbyname`. A value that cannot be read is
/// the system's error; a value that breaks the rules of [`HostInfo`] is
/// `InvalidData`, never cut or guessed. Other platforms return
/// `ErrorKind::Unsupported`. Reads no file; any thread may call it.
pub fn describe() -> io::Result<HostInfo> {
    imp::describe()
}

/// One string value as `sysctl` returns it (with or without its trailing
/// NULs) when it is a plain value: 1 to 64 bytes of printable ASCII after
/// the NULs are removed; None for anything else, a NUL in the middle
/// included. Compiled for tests everywhere: it only checks bytes.
#[cfg(any(target_os = "macos", test))]
fn host_value(raw: &[u8]) -> Option<String> {
    let end = raw.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    let value = &raw[..end];
    let valid = !value.is_empty()
        && value.len() <= MAX_VALUE_LEN
        && value.iter().all(|b| (0x20..=0x7E).contains(b));
    if !valid {
        return None;
    }
    // Only printable ASCII is left.
    Some(std::str::from_utf8(value).ok()?.to_owned())
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::CStr;
    use std::io;

    use super::{HostInfo, MAX_VALUE_LEN, host_value};
    use crate::system::product_version;

    /// Room for a longer value than any accepted one, so that an overlong
    /// value is read whole and rejected as `InvalidData`.
    const BUF_LEN: usize = 2 * MAX_VALUE_LEN;

    pub(super) fn describe() -> io::Result<HostInfo> {
        let mut buf = [0u8; BUF_LEN];
        let model = string(c"hw.model", &mut buf, host_value)?;
        let chip = string(c"machdep.cpu.brand_string", &mut buf, host_value)?;
        let os_version = string(c"kern.osproductversion", &mut buf, |raw| {
            // The shared check; it accepts a shorter set than `host_value`.
            product_version(raw).map(str::to_owned)
        })?;
        let cpus = logical_cpus()?;
        Ok(HostInfo {
            model,
            chip,
            os_version,
            cpus,
        })
    }

    /// The string value of `name` read into `buf` and checked by `check`.
    fn string(
        name: &CStr,
        buf: &mut [u8; BUF_LEN],
        check: impl Fn(&[u8]) -> Option<String>,
    ) -> io::Result<String> {
        let mut len: libc::size_t = buf.len();
        // SAFETY: `name` is a NUL-terminated C string; `buf` is valid for
        // writes of `len` bytes and `len` is a valid, exclusive pointer for
        // the call, which writes at most `len` bytes and stores the number
        // written back into `len`. A null `newp` with `newlen` 0 only reads
        // the value.
        let rc = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                buf.as_mut_ptr().cast::<libc::c_void>(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            let err = io::Error::last_os_error();
            // The value does not fit even the larger buffer: it is not a
            // plain value either.
            return Err(if err.raw_os_error() == Some(libc::ENOMEM) {
                invalid(name)
            } else {
                err
            });
        }
        buf.get(..len).and_then(check).ok_or_else(|| invalid(name))
    }

    fn logical_cpus() -> io::Result<u32> {
        let mut value: libc::c_int = 0;
        let mut len: libc::size_t = size_of::<libc::c_int>();
        // SAFETY: the name is a NUL-terminated C string; `value` is valid
        // for writes of `len` bytes and `len` is a valid, exclusive pointer
        // for the call. A null `newp` with `newlen` 0 only reads the value.
        let rc = unsafe {
            libc::sysctlbyname(
                c"hw.logicalcpu".as_ptr(),
                (&raw mut value).cast::<libc::c_void>(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        if len != size_of::<libc::c_int>() {
            return Err(invalid(c"hw.logicalcpu"));
        }
        u32::try_from(value)
            .ok()
            .filter(|&cpus| cpus > 0)
            .ok_or_else(|| invalid(c"hw.logicalcpu"))
    }

    fn invalid(name: &CStr) -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not a plain value", name.to_string_lossy()),
        )
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::io;

    use super::HostInfo;

    pub(super) fn describe() -> io::Result<HostInfo> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "describe is macOS only",
        ))
    }
}
