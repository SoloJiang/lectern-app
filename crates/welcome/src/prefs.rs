//! The language preference the view reads and writes, behind a trait so that
//! the tests never touch the real user defaults (docs/design/welcome.md
//! 「数据流与线程」第 7 步). The only implementation outside the tests is
//! `PlatformPrefs`, which calls `platform::system` (architecture rule 6).

use lectern_platform::system::{self, PrefError};

/// What `set_language_override` calls when the write has been executed (or
/// replaced by a newer one): on `platform`'s queue thread.
pub(crate) type Done = Box<dyn FnOnce(Result<(), PrefError>) + Send>;

/// The three calls of `platform::system` the language picker uses.
pub(crate) trait LanguagePrefs: Send + Sync {
    /// The system's preferred languages, without the app's own override.
    fn system_languages(&self) -> Vec<String>;
    /// The saved override's tag, if any. A read of the in-memory preference.
    fn language_override(&self) -> Option<String>;
    /// Saves `tag` (`None` removes the override). Returns at once.
    fn set_language_override(&self, tag: Option<&str>, done: Done);
}

/// The real preference, through `platform`.
pub(crate) struct PlatformPrefs;

impl LanguagePrefs for PlatformPrefs {
    fn system_languages(&self) -> Vec<String> {
        system::system_languages()
    }

    fn language_override(&self) -> Option<String> {
        system::language_override()
    }

    fn set_language_override(&self, tag: Option<&str>, done: Done) {
        system::set_language_override(tag, done);
    }
}
