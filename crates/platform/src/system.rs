//! Facts about the running system: the OS version for the header line of
//! every log file (「日志目录与系统版本」 in docs/design/platform.md,
//! docs/design/logging.md「文件」) and the user's preferred languages for the
//! interface language (「系统首选语言」, docs/design/app.md).

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

/// The longest language tag accepted, in bytes. Real ones are like `en-US`
/// or `zh-Hans-CN`.
#[cfg(any(target_os = "macos", test))]
const MAX_TAG_LEN: usize = 35;

/// At most this many preferred languages are returned; `Locale::negotiate`
/// only needs the first one it supports.
#[cfg(target_os = "macos")]
const MAX_LANGUAGES: usize = 16;

/// The user's preferred languages, most preferred first, as BCP 47 tags such
/// as `zh-Hans-CN`: `CFLocaleCopyPreferredLanguages` on macOS.
///
/// Tags that are not ASCII letters, digits and hyphens, start with a
/// hyphen or are longer than 35 bytes are left out; only the first 16
/// entries are looked at.
/// Empty when nothing can be read, and on every other platform (`app`, the
/// only caller, is macOS only). The system preferences are in memory: no
/// file of Lectern's is read, so `app` calls it before the first frame
/// (docs/design/app.md「启动顺序」). Any thread may call it.
pub fn preferred_languages() -> Vec<String> {
    imp::preferred_languages()
}

mod language;
pub use language::{PrefError, language_override, set_language_override, system_languages};

/// One tag as CoreFoundation writes it into a buffer (up to the first NUL)
/// turned into a `String`; None for anything that is not a tag. Compiled
/// for tests everywhere: it only checks bytes.
#[cfg(any(target_os = "macos", test))]
fn language_tag(raw: &[u8]) -> Option<String> {
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    let tag = &raw[..end];
    let valid = !tag.is_empty()
        && tag.len() <= MAX_TAG_LEN
        && tag[0].is_ascii_alphanumeric()
        && tag.iter().all(|&b| b.is_ascii_alphanumeric() || b == b'-');
    if !valid {
        return None;
    }
    // Only ASCII letters, digits and hyphens are left.
    Some(std::str::from_utf8(tag).ok()?.to_owned())
}

/// The product version as `sysctl` returns it (with or without its
/// trailing NUL) turned into `macOS <version>`; None for anything else.
/// Compiled for tests everywhere: it only checks bytes.
#[cfg(any(target_os = "macos", test))]
fn mac_os_version(raw: &[u8]) -> Option<String> {
    Some(format!("macOS {}", product_version(raw)?))
}

/// The product version as `sysctl` returns it (with or without its
/// trailing NULs) when it is one: at most 32 bytes, ASCII digits and dots,
/// starting with a digit; None for anything else. Shared with
/// `host::describe`, which reports the version without the `macOS ` prefix.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn product_version(raw: &[u8]) -> Option<&str> {
    let end = raw.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    let version = &raw[..end];
    let valid = version.len() <= MAX_VERSION_LEN
        && version.first().is_some_and(u8::is_ascii_digit)
        && version.iter().all(|&b| b.is_ascii_digit() || b == b'.');
    if !valid {
        return None;
    }
    // Only ASCII digits and dots are left.
    std::str::from_utf8(version).ok()
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{c_char, c_void};
    use std::ptr;

    use super::{MAX_LANGUAGES, MAX_TAG_LEN, MAX_VERSION_LEN, language_tag, mac_os_version};

    type CFIndex = isize;
    type CFTypeID = usize;
    type CFTypeRef = *const c_void;
    pub(super) type CFArrayRef = *const c_void;
    type CFStringRef = *const c_void;
    /// `kCFStringEncodingUTF8`.
    const UTF8: u32 = 0x0800_0100;

    // Already linked by AppKit (and by `fs_events`).
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFLocaleCopyPreferredLanguages() -> CFArrayRef;
        fn CFArrayGetCount(array: CFArrayRef) -> CFIndex;
        fn CFArrayGetValueAtIndex(array: CFArrayRef, index: CFIndex) -> *const c_void;
        fn CFStringGetCString(
            string: CFStringRef,
            buffer: *mut c_char,
            size: CFIndex,
            encoding: u32,
        ) -> u8;
        fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
        fn CFStringGetTypeID() -> CFTypeID;
        fn CFRelease(cf: CFTypeRef);
    }

    pub(super) fn preferred_languages() -> Vec<String> {
        // SAFETY: takes no arguments; returns a +1 CFArray of CFStrings or
        // null.
        let array = unsafe { CFLocaleCopyPreferredLanguages() };
        if array.is_null() {
            return Vec::new();
        }
        // SAFETY: `array` is a valid CFArray until the `CFRelease` below.
        let tags = unsafe { tags(array) };
        // SAFETY: balances the Copy rule of `CFLocaleCopyPreferredLanguages`.
        unsafe { CFRelease(array) };
        tags
    }

    /// The accepted tags among the first `MAX_LANGUAGES` entries of `array`;
    /// entries that are not CFStrings are skipped, and so is anything
    /// `language_tag` rejects. Shared with `language`, which reads the same
    /// kind of array from the preferences.
    ///
    /// # Safety
    ///
    /// `array` is a valid CFArray (any element types) for the call.
    pub(super) unsafe fn tags(array: CFArrayRef) -> Vec<String> {
        let mut tags = Vec::new();
        // SAFETY: the caller guarantees `array` is valid.
        let count = unsafe { CFArrayGetCount(array) };
        // Only the first entries can matter to `Locale::negotiate`: bound the
        // walk, not just the result.
        for i in 0..count.min(MAX_LANGUAGES as CFIndex) {
            // SAFETY: `i` is within `0..count`; the value belongs to the
            // array (Get rule: not released here).
            let string = unsafe { CFArrayGetValueAtIndex(array, i) };
            // SAFETY: `string` is non-null and a CF object owned by the
            // array; both calls only read its type.
            if string.is_null() || unsafe { CFGetTypeID(string) != CFStringGetTypeID() } {
                continue;
            }
            // Room for a longer value than any accepted one plus the NUL, so
            // that a too-long tag fails here or in `language_tag`.
            let mut buf = [0u8; MAX_TAG_LEN + 2];
            // SAFETY: `string` is a CFString (checked above); `buf` is valid
            // for writes of its length, which is passed as `size`.
            let ok = unsafe {
                CFStringGetCString(
                    string,
                    buf.as_mut_ptr().cast::<c_char>(),
                    buf.len() as CFIndex,
                    UTF8,
                )
            };
            if ok != 0
                && let Some(tag) = language_tag(&buf)
            {
                tags.push(tag);
            }
        }
        tags
    }

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

    pub(super) fn preferred_languages() -> Vec<String> {
        Vec::new()
    }
}
