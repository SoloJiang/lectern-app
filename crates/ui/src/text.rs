//! Interface text (docs/design/ui.md「文本」): controls accept only a message
//! from `lectern-i18n` or content marked as shown verbatim; a literal does
//! not compile. The current `Strings` are a GPUI global.

use std::sync::Arc;

use gpui::{App, SharedString};
use lectern_i18n::{Args, FmtKey, Key, Locale, Strings};
use lectern_logging::debug;

use crate::state::UiState;

/// Text a control accepts. There is no `From<&str>` and no
/// `From<SharedString>`: a literal cannot reach a control.
///
/// `Text::verbatim` takes only `Arc<str>` and `String`, the forms of the
/// domain snapshots; a literal does not compile:
///
/// ```compile_fail,E0277
/// let text = lectern_ui::Text::verbatim("literal");
/// ```
///
/// A message key is a `Key`, not a string:
///
/// ```compile_fail,E0308
/// use lectern_i18n::{Locale, Strings};
/// let strings = Strings::of(Locale::En);
/// let text = lectern_ui::Text::key(&strings, "review-looks-good");
/// ```
#[derive(Clone, Debug)]
pub struct Text {
    text: SharedString,
    key: Option<&'static str>,
}

impl Text {
    /// A message from the resource files (`Strings::get`).
    pub fn key(strings: &Strings, key: Key) -> Text {
        Text {
            text: SharedString::from(strings.get(key)),
            key: Some(key.name()),
        }
    }

    /// A message with arguments (`Strings::format`).
    pub fn fmt(strings: &Strings, key: FmtKey, args: &Args<'_>) -> Text {
        Text {
            text: SharedString::from(strings.format(key, args)),
            key: Some(key.name()),
        }
    }

    /// Content people and agents wrote, paths, commands, shortcuts: shown as
    /// is, never translated (ADR 0013). A literal is written
    /// `Text::verbatim(Arc::from("…"))`, visible at a glance in review.
    pub fn verbatim(content: impl Into<Verbatim>) -> Text {
        Text {
            text: content.into().0,
            key: None,
        }
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn shared(&self) -> SharedString {
        self.text.clone()
    }

    /// The Fluent message ID the text came from; None for verbatim text.
    pub fn message_id(&self) -> Option<&'static str> {
        self.key
    }
}

/// Verbatim content: `From<Arc<str>>` and `From<String>`, no `From<&str>`.
#[derive(Clone, Debug)]
pub struct Verbatim(SharedString);

impl From<Arc<str>> for Verbatim {
    fn from(content: Arc<str>) -> Verbatim {
        Verbatim(SharedString::from(content))
    }
}

impl From<String> for Verbatim {
    fn from(content: String) -> Verbatim {
        Verbatim(SharedString::from(content))
    }
}

/// The strings of the current interface language. `init` sets them; views
/// read them while rendering.
pub fn strings(cx: &App) -> Strings {
    cx.global::<UiState>().strings
}

/// Switches the interface language: replaces the global `Strings` and
/// refreshes every window. Writing the setting is the caller's job.
pub fn set_locale(locale: Locale, cx: &mut App) {
    let state = cx.global_mut::<UiState>();
    if state.strings.locale() == locale {
        return;
    }
    state.strings = Strings::of(locale);
    debug!("locale changed"; locale = locale.tag());
    cx.refresh_windows();
}
