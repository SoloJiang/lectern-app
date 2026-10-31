//! The built-in clipboard route of review notes (docs/design/review.md
//! 「审阅意见的发送途径」): the one place of this crate that calls
//! `platform`'s clipboard. The text counts as sent once it is on the
//! clipboard; where the user pastes it is not known here. Nothing is read
//! back and the text is never logged.
//!
//! The engine holds a [`Writer`], the system's one by default
//! ([`system`]), so that the tests put a fake in its place and never touch
//! the real clipboard.

use std::sync::Arc;

/// Why the text is not on the clipboard (the kinds of `platform`'s).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WriteError {
    /// There is no system clipboard here.
    Unsupported,
    /// The clipboard did not take the text.
    Rejected,
}

impl WriteError {
    /// The `error` field of the log's records.
    pub(crate) fn kind(self) -> &'static str {
        match self {
            WriteError::Unsupported => "unsupported",
            WriteError::Rejected => "rejected",
        }
    }
}

/// What a write calls when it is over, on any thread; it must not block.
pub type Done = Box<dyn FnOnce(Result<(), WriteError>) + Send>;

/// Puts a text on the clipboard. Any thread, does not block: `Done` is
/// called later (on the main thread on macOS), or at once.
pub type Writer = Arc<dyn Fn(Arc<str>, Done) + Send + Sync>;

/// The system clipboard. The unit tests of this crate get a writer that
/// refuses instead: no test reaches the real clipboard by accident.
pub(crate) fn system() -> Writer {
    #[cfg(test)]
    return Arc::new(|_, done| done(Err(WriteError::Unsupported)));
    #[cfg(not(test))]
    Arc::new(|text, done| {
        lectern_platform::clipboard::write_text(
            text,
            Box::new(move |result| {
                done(result.map_err(|e| match e {
                    lectern_platform::clipboard::ClipboardError::Unsupported => {
                        WriteError::Unsupported
                    }
                    lectern_platform::clipboard::ClipboardError::Rejected => WriteError::Rejected,
                }))
            }),
        )
    })
}
