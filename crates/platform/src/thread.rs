//! The quality of service of the calling thread, for the benchmark threads
//! of `lectern-bench` (docs/design/platform.md 「主机信息与线程 QoS」).

use std::io;

#[cfg(test)]
mod tests;

/// Sets the QoS of the calling thread to user-interactive
/// (`pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0)`), so that
/// a benchmark thread runs on the performance cores first. A failure is the
/// system's error. Other platforms return `ErrorKind::Unsupported`. Does not
/// block.
pub fn set_user_interactive() -> io::Result<()> {
    imp::set_user_interactive()
}

#[cfg(target_os = "macos")]
mod imp {
    use std::io;

    pub(super) fn set_user_interactive() -> io::Result<()> {
        // SAFETY: changes the QoS of the calling thread only; takes no
        // pointers. Unlike most calls it returns the error number itself
        // and leaves `errno` alone.
        let rc = unsafe {
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0)
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(rc))
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::io;

    pub(super) fn set_user_interactive() -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "set_user_interactive is macOS only",
        ))
    }
}
