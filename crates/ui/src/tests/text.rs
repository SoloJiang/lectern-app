//! `text_*` (docs/design/ui.md「文本」): the forms `Text` accepts and the
//! message ID it carries. The `compile_fail` doctests on `Text` and
//! `contrib::ToolbarItems` cover the forms it refuses.

use std::sync::Arc;

use lectern_i18n::{Args, Locale, Strings, keys};

use crate::{Text, Verbatim};

#[test]
fn text_verbatim_accepts_arc_str_and_string() {
    let from_arc = Text::verbatim(Arc::<str>::from("src/lib.rs"));
    let from_string = Text::verbatim(String::from("src/lib.rs"));
    assert_eq!(from_arc.as_str(), "src/lib.rs");
    assert_eq!(from_string.as_str(), "src/lib.rs");
    assert_eq!(from_arc.shared(), from_string.shared());
    assert_eq!(from_arc.message_id(), None);
    let _: Verbatim = String::from("x").into();
}

#[test]
fn text_key_and_fmt_carry_the_message_id() {
    let en = Strings::of(Locale::En);
    let zh = Strings::of(Locale::ZhCn);
    let key = Text::key(&en, keys::review::LOOKS_GOOD);
    assert_eq!(key.as_str(), "Looks Good");
    assert_eq!(key.message_id(), Some("review-looks-good"));
    assert_eq!(
        Text::key(&zh, keys::review::LOOKS_GOOD).message_id(),
        Some("review-looks-good")
    );
    let fmt = Text::fmt(
        &en,
        keys::review::FILES_CHANGED,
        &Args::new().num("count", 3),
    );
    assert_eq!(fmt.as_str(), "3 files changed");
    assert_eq!(fmt.message_id(), Some("review-files-changed"));
    let cloned = fmt.clone();
    assert_eq!(cloned.as_str(), fmt.as_str());
    assert!(format!("{cloned:?}").contains("review-files-changed"));
}
