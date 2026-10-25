//! Measurements of a process, for `lectern-bench` (docs/design/platform.md
//! 「进程」): the physical footprint, the creation time and the children.

use std::io;
use std::time::SystemTime;

#[cfg(test)]
mod tests;

/// The physical footprint of process `pid`: what Activity Monitor shows as
/// "Memory" (`proc_pid_rusage`, `ri_phys_footprint`). macOS only; other
/// platforms return `ErrorKind::Unsupported`.
pub fn phys_footprint(pid: u32) -> io::Result<u64> {
    imp::phys_footprint(pid)
}

/// The time the kernel recorded when it created process `pid`, the start of
/// the cold start measurement (ADR 0017). macOS: `proc_pidinfo` with
/// `PROC_PIDTBSDINFO`, `pbi_start_tvsec` and `pbi_start_tvusec`
/// (microseconds); `exec` does not change it. A missing process, or one that
/// may not be inspected, gives the system's error. Other platforms return
/// `ErrorKind::Unsupported`. No allocation, no thread; any thread may call
/// it.
pub fn start_time(pid: u32) -> io::Result<SystemTime> {
    imp::start_time(pid)
}

/// The direct children of process `pid` at the time of the call, in
/// ascending pid order (`proc_listchildpids` on macOS). Empty when there are
/// none. Children that start or exit during the call may or may not be in
/// the result. Other platforms return `ErrorKind::Unsupported`. Any thread
/// may call it.
pub fn children(pid: u32) -> io::Result<Vec<u32>> {
    imp::children(pid)
}

#[cfg(target_os = "macos")]
mod imp {
    use std::io;
    use std::mem::{MaybeUninit, size_of};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    /// Room for children that start between the two calls of `children`.
    const CHILDREN_SLACK: usize = 16;

    fn pid_arg(pid: u32) -> io::Result<libc::pid_t> {
        libc::pid_t::try_from(pid).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
    }

    pub(super) fn phys_footprint(pid: u32) -> io::Result<u64> {
        let pid = pid_arg(pid)?;
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

    pub(super) fn start_time(pid: u32) -> io::Result<SystemTime> {
        let pid = pid_arg(pid)?;
        let size = size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let mut info = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
        // SAFETY: with `PROC_PIDTBSDINFO`, `proc_pidinfo` writes at most
        // `size` bytes, one `proc_bsdinfo`, to the buffer, which is valid
        // for writes of that size.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast::<libc::c_void>(),
                size,
            )
        };
        if written <= 0 {
            // For a process that does not exist `proc_pidinfo` returns 0;
            // `errno` is not always set then.
            let err = io::Error::last_os_error();
            return Err(match err.raw_os_error() {
                Some(code) if code != 0 => err,
                _ => io::Error::from_raw_os_error(libc::ESRCH),
            });
        }
        if written != size {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        // SAFETY: all zeros is a valid `proc_bsdinfo` (plain integers and
        // character arrays), and the call filled all `size` bytes.
        let info = unsafe { info.assume_init() };
        if info.pbi_start_tvusec >= 1_000_000 {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        let since_epoch = Duration::from_secs(info.pbi_start_tvsec)
            + Duration::from_micros(info.pbi_start_tvusec);
        UNIX_EPOCH
            .checked_add(since_epoch)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))
    }

    pub(super) fn children(pid: u32) -> io::Result<Vec<u32>> {
        let ppid = pid_arg(pid)?;
        // Both calls return a number of pids, not bytes. The first, with a
        // null buffer, is an upper estimate (measured: about the number of
        // processes on the system, far more than the number of children);
        // the second is the number written, in the order the kernel keeps
        // them (not ascending).
        //
        // SAFETY: a null buffer of size 0 only asks for the estimate.
        let wanted = unsafe { libc::proc_listchildpids(ppid, std::ptr::null_mut(), 0) };
        let wanted = usize::try_from(wanted).unwrap_or(0);
        let capacity = wanted + CHILDREN_SLACK;
        let mut pids: Vec<libc::pid_t> = vec![0; capacity];
        let bytes = (capacity * size_of::<libc::pid_t>()) as libc::c_int;
        // SAFETY: `pids` is valid for writes of `bytes` bytes, which is
        // passed as the buffer size.
        let listed = unsafe {
            libc::proc_listchildpids(ppid, pids.as_mut_ptr().cast::<libc::c_void>(), bytes)
        };
        pids.truncate(usize::try_from(listed).unwrap_or(0).min(capacity));
        let mut pids: Vec<u32> = pids
            .into_iter()
            .filter_map(|p| u32::try_from(p).ok())
            .collect();
        pids.sort_unstable();
        Ok(pids)
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::io;
    use std::time::SystemTime;

    pub(super) fn phys_footprint(_pid: u32) -> io::Result<u64> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "phys_footprint is macOS only",
        ))
    }

    pub(super) fn start_time(_pid: u32) -> io::Result<SystemTime> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "start_time is macOS only",
        ))
    }

    pub(super) fn children(_pid: u32) -> io::Result<Vec<u32>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "children is macOS only",
        ))
    }
}
