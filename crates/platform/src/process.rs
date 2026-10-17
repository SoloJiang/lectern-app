//! Measurements of a process, for `lectern-bench` (docs/design/platform.md
//! 「进程」). Only `phys_footprint` so far; `start_time` and `children`
//! follow with bench step 2.

use std::io;

#[cfg(test)]
mod tests;

/// The physical footprint of process `pid`: what Activity Monitor shows as
/// "Memory" (`proc_pid_rusage`, `ri_phys_footprint`). macOS only; other
/// platforms return `ErrorKind::Unsupported`.
pub fn phys_footprint(pid: u32) -> io::Result<u64> {
    imp::phys_footprint(pid)
}

#[cfg(target_os = "macos")]
mod imp {
    use std::io;
    use std::mem::MaybeUninit;

    pub(super) fn phys_footprint(pid: u32) -> io::Result<u64> {
        let pid =
            libc::c_int::try_from(pid).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let mut info = MaybeUninit::<libc::rusage_info_v2>::zeroed();
        // SAFETY: with `RUSAGE_INFO_V2`, `proc_pid_rusage` writes one
        // `rusage_info_v2` to the buffer, which is exactly that type.
        let rc = unsafe {
            libc::proc_pid_rusage(
                pid,
                libc::RUSAGE_INFO_V2,
                info.as_mut_ptr().cast::<libc::rusage_info_t>(),
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: all zeros is a valid `rusage_info_v2` (plain integers),
        // and the call succeeded, so it holds the kernel's values.
        Ok(unsafe { info.assume_init() }.ri_phys_footprint)
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::io;

    pub(super) fn phys_footprint(_pid: u32) -> io::Result<u64> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "phys_footprint is macOS only",
        ))
    }
}
