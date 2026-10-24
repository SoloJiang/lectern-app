//! The interface language preference (「语言偏好」 in
//! docs/design/platform.md, docs/design/i18n.md「数据流与线程」): the system's
//! preferred languages without the app's own override, whether the app
//! domain holds an `AppleLanguages` override, and the serial queue that
//! writes it.

#[cfg(test)]
mod tests;

/// Why a language override was not written.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PrefError {
    /// A newer request replaced this one before it was written. Not a
    /// failure: the newer request carries the choice that counts.
    Superseded,
    /// The write was refused or could not be synchronized, or the tag is not
    /// a language tag (ASCII letters, digits and hyphens, at most 35
    /// bytes).
    WriteFailed,
}

impl PrefError {
    /// The name for the log field `kind`: `superseded` or `write_failed`.
    pub fn as_str(self) -> &'static str {
        match self {
            PrefError::Superseded => "superseded",
            PrefError::WriteFailed => "write_failed",
        }
    }
}

impl std::fmt::Display for PrefError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PrefError::Superseded => "the request was replaced by a newer one",
            PrefError::WriteFailed => "the language preference was not written",
        })
    }
}

impl std::error::Error for PrefError {}

/// The system's preferred languages, most preferred first, as BCP 47 tags:
/// the global domain (`NSGlobalDomain`) only, so the app's own override does
/// not show. `preferred_languages` includes the override and keeps the old
/// list in a running process after the override is removed; this does not.
///
/// The same rules as `preferred_languages`: only the first 16 entries are
/// looked at, tags that are not ASCII letters, digits and hyphens, start
/// with a hyphen or are longer than 35 bytes are left out. Empty when
/// nothing can be read, and on every other platform. The preferences are in
/// memory: no file of Lectern's is read. Any thread may call it.
pub fn system_languages() -> Vec<String> {
    imp::system_languages()
}

/// The first tag of the `AppleLanguages` override in the app's own domain;
/// None when the domain holds no override (the key is missing or not a list
/// of tags), and on every other platform. The global domain is not
/// consulted, so the answer tells "follow the system" from an override that
/// happens to equal the system's list. Preferences in memory: no file is
/// read. Any thread may call it.
pub fn language_override() -> Option<String> {
    imp::language_override()
}

/// Writes the `AppleLanguages` override in the app's own domain: `Some(tag)`
/// stores a one-element list, None removes the key.
///
/// Returns at once. The write goes to a serial queue, in the order of the
/// calls; a request that has not started when a newer one arrives is dropped
/// and its `done` gets `Err(PrefError::Superseded)`, so the last choice is
/// always the last write. `done` runs once on the queue's thread after the
/// request was written and synchronized (`Err(WriteFailed)` when it was not,
/// which is also logged as a warn); do not block in it. A panic in `done` is
/// contained.
///
/// On every other platform nothing is written and `done(Ok(()))` runs on
/// the calling thread.
pub fn set_language_override(
    tag: Option<&str>,
    done: Box<dyn FnOnce(Result<(), PrefError>) + Send>,
) {
    imp::set_language_override(tag, done);
}

type Done = Box<dyn FnOnce(Result<(), PrefError>) + Send>;

#[cfg(any(target_os = "macos", test))]
mod queue;
#[cfg(any(target_os = "macos", test))]
use queue::Queue;

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;
    use std::ptr;
    use std::sync::{Arc, OnceLock};

    use lectern_logging::warn;

    use super::{Done, PrefError, Queue};
    use crate::system::language_tag;

    type CFIndex = isize;
    type CFTypeRef = *const c_void;
    type CFStringRef = *const c_void;
    type CFArrayRef = *const c_void;
    /// `kCFStringEncodingUTF8`.
    const UTF8: u32 = 0x0800_0100;

    /// `CFArrayCallBacks`; `kCFTypeArrayCallBacks` is only ever passed by
    /// address. The layout is the one `fs_events` declares, so that the two
    /// declarations of `CFArrayCreate` agree.
    #[repr(C)]
    struct CFArrayCallBacks {
        version: CFIndex,
        retain: *const c_void,
        release: *const c_void,
        copy_description: *const c_void,
        equal: *const c_void,
    }

    // Already linked by AppKit (and by `fs_events`).
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFPreferencesAnyApplication: CFStringRef;
        static kCFPreferencesCurrentApplication: CFStringRef;
        static kCFPreferencesCurrentUser: CFStringRef;
        static kCFPreferencesAnyHost: CFStringRef;
        static kCFTypeArrayCallBacks: CFArrayCallBacks;

        fn CFPreferencesCopyValue(
            key: CFStringRef,
            application: CFStringRef,
            user: CFStringRef,
            host: CFStringRef,
        ) -> CFTypeRef;
        fn CFPreferencesSetValue(
            key: CFStringRef,
            value: CFTypeRef,
            application: CFStringRef,
            user: CFStringRef,
            host: CFStringRef,
        );
        fn CFPreferencesSynchronize(
            application: CFStringRef,
            user: CFStringRef,
            host: CFStringRef,
        ) -> u8;
        fn CFStringCreateWithBytes(
            alloc: *const c_void,
            bytes: *const u8,
            len: CFIndex,
            encoding: u32,
            is_external: u8,
        ) -> CFStringRef;
        fn CFArrayCreate(
            alloc: *const c_void,
            values: *const *const c_void,
            count: CFIndex,
            callbacks: *const CFArrayCallBacks,
        ) -> CFArrayRef;
        fn CFGetTypeID(cf: CFTypeRef) -> usize;
        fn CFArrayGetTypeID() -> usize;
        fn CFRelease(cf: CFTypeRef);
    }

    const KEY: &[u8] = b"AppleLanguages";

    /// Where the override lives. `Current` is the app's own domain; tests
    /// use a plist in a temporary directory (an absolute path is a valid
    /// CFPreferences application ID) so that no preference of the user's is
    /// touched.
    #[derive(Clone, Copy)]
    pub(super) enum Domain<'a> {
        Current,
        #[cfg_attr(
            not(test),
            allow(dead_code, reason = "only the tests use a plist of their own")
        )]
        Path(&'a str),
    }

    /// An owned CF object, released on drop.
    struct Owned(CFTypeRef);

    impl Owned {
        fn string(text: &[u8]) -> Option<Owned> {
            // SAFETY: `text` is valid for reads of `len` bytes; a null
            // allocator is the default one.
            let s = unsafe {
                CFStringCreateWithBytes(ptr::null(), text.as_ptr(), text.len() as CFIndex, UTF8, 0)
            };
            (!s.is_null()).then_some(Owned(s))
        }
    }

    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: a +1 object this value owns.
            unsafe { CFRelease(self.0) };
        }
    }

    /// The `application` argument of a domain: a constant for the current
    /// application, or a string made for a path (kept alive in the second
    /// field).
    fn application(domain: Domain<'_>) -> Option<(CFStringRef, Option<Owned>)> {
        match domain {
            // SAFETY: a CoreFoundation constant, initialised at load.
            Domain::Current => Some((unsafe { kCFPreferencesCurrentApplication }, None)),
            Domain::Path(path) => {
                let s = Owned::string(path.as_bytes())?;
                Some((s.0, Some(s)))
            }
        }
    }

    /// The tags of the `AppleLanguages` list in `application`/`user`/`host`,
    /// exactly that domain (`CFPreferencesCopyValue` does not fall back to
    /// the global one, unlike `CFPreferencesCopyAppValue`).
    fn read_list(application: CFStringRef, user: CFStringRef, host: CFStringRef) -> Vec<String> {
        let Some(key) = Owned::string(KEY) else {
            return Vec::new();
        };
        // SAFETY: valid CFStrings; returns a +1 value or null.
        let value = unsafe { CFPreferencesCopyValue(key.0, application, user, host) };
        if value.is_null() {
            return Vec::new();
        }
        let value = Owned(value);
        // SAFETY: `value` is a valid CF object.
        if unsafe { CFGetTypeID(value.0) != CFArrayGetTypeID() } {
            return Vec::new();
        }
        // SAFETY: `value` is a valid CFArray until it drops.
        unsafe { crate::system::imp::tags(value.0) }
    }

    pub(in crate::system) fn system_languages() -> Vec<String> {
        // SAFETY: CoreFoundation constants, initialised at load.
        let (application, user, host) = unsafe {
            (
                kCFPreferencesAnyApplication,
                kCFPreferencesCurrentUser,
                kCFPreferencesAnyHost,
            )
        };
        read_list(application, user, host)
    }

    pub(super) fn read_override(domain: Domain<'_>) -> Option<String> {
        let (application, _keep) = application(domain)?;
        // SAFETY: CoreFoundation constants, initialised at load.
        let (user, host) = unsafe { (kCFPreferencesCurrentUser, kCFPreferencesAnyHost) };
        read_list(application, user, host).into_iter().next()
    }

    pub(in crate::system) fn language_override() -> Option<String> {
        read_override(Domain::Current)
    }

    /// Writes (`Some`) or removes (None) the override and synchronizes the
    /// domain. Not logged here.
    pub(super) fn write_override(domain: Domain<'_>, tag: Option<&str>) -> Result<(), PrefError> {
        let value = match tag {
            Some(tag) => {
                language_tag(tag.as_bytes()).ok_or(PrefError::WriteFailed)?;
                Some(Owned::string(tag.as_bytes()).ok_or(PrefError::WriteFailed)?)
            }
            None => None,
        };
        let list = match &value {
            Some(tag) => {
                let items = [tag.0];
                // SAFETY: `items` holds one valid CFString; the type
                // callbacks retain it.
                let array = unsafe {
                    CFArrayCreate(
                        ptr::null(),
                        items.as_ptr(),
                        1,
                        &raw const kCFTypeArrayCallBacks,
                    )
                };
                if array.is_null() {
                    return Err(PrefError::WriteFailed);
                }
                Some(Owned(array))
            }
            None => None,
        };
        let key = Owned::string(KEY).ok_or(PrefError::WriteFailed)?;
        let (application, _keep) = application(domain).ok_or(PrefError::WriteFailed)?;
        // SAFETY: valid CFStrings and constants; a null value removes the
        // key.
        let synced = unsafe {
            let (user, host) = (kCFPreferencesCurrentUser, kCFPreferencesAnyHost);
            CFPreferencesSetValue(
                key.0,
                list.as_ref().map_or(ptr::null(), |l| l.0),
                application,
                user,
                host,
            );
            CFPreferencesSynchronize(application, user, host)
        };
        if synced == 0 {
            return Err(PrefError::WriteFailed);
        }
        Ok(())
    }

    fn write_logged(tag: Option<&str>) -> Result<(), PrefError> {
        let result = write_override(Domain::Current, tag);
        if let Err(error) = result {
            warn!(
                "the language preference was not saved; the interface language applies to this run only";
                kind = error.as_str(),
            );
        }
        result
    }

    pub(in crate::system) fn set_language_override(tag: Option<&str>, done: Done) {
        static QUEUE: OnceLock<Arc<Queue>> = OnceLock::new();
        let queue = QUEUE.get_or_init(|| Queue::new(Box::new(write_logged)));
        queue.submit(tag, done);
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::Done;

    pub(in crate::system) fn system_languages() -> Vec<String> {
        Vec::new()
    }

    pub(in crate::system) fn language_override() -> Option<String> {
        None
    }

    pub(in crate::system) fn set_language_override(_tag: Option<&str>, done: Done) {
        done(Ok(()));
    }
}
