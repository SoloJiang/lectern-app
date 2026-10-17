use crate::{Key, keys};

/// An interface language. Adding one means a variant here, a directory under
/// `locales/`, and its plural rules in plural.rs and build/compile.rs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Locale {
    En,
    ZhCn,
}

impl Locale {
    pub const ALL: [Locale; 2] = [Locale::En, Locale::ZhCn];

    /// The BCP 47 tag: `"en"`, `"zh-CN"`.
    pub fn tag(self) -> &'static str {
        match self {
            Locale::En => "en",
            Locale::ZhCn => "zh-CN",
        }
    }

    /// The language's name in the language picker. Both locales name each
    /// language in its own script (`English`, `简体中文`).
    pub fn name_key(self) -> Key {
        match self {
            Locale::En => keys::common::LANGUAGE_EN,
            Locale::ZhCn => keys::common::LANGUAGE_ZH_CN,
        }
    }

    /// Walks the system's preferred languages in order (on macOS e.g.
    /// `["ja-JP", "zh-Hans-CN", "en-US"]`) and returns the first one that maps
    /// to a supported language, or `En` when none does. Pure; any thread.
    ///
    /// | Preferred language | Result |
    /// |---|---|
    /// | `zh-Hans*`; `zh`, `zh-CN`, `zh-SG`, `zh-MY` without a script | `ZhCn` |
    /// | `zh-Hant*`, `zh-TW`, `zh-HK`, `zh-MO` | skipped |
    /// | `en*` | `En` |
    /// | anything else | skipped |
    ///
    /// Subtags are case-insensitive and may be separated by `-` or `_`.
    ///
    /// ```
    /// use lectern_i18n::Locale;
    ///
    /// assert_eq!(Locale::negotiate(["ja-JP", "zh-Hans-CN", "en-US"]), Locale::ZhCn);
    /// assert_eq!(Locale::negotiate(["zh-Hant-TW", "en-GB"]), Locale::En);
    /// assert_eq!(Locale::negotiate([]), Locale::En);
    /// ```
    pub fn negotiate<'a>(preferred: impl IntoIterator<Item = &'a str>) -> Locale {
        preferred
            .into_iter()
            .find_map(match_tag)
            .unwrap_or(Locale::En)
    }
}

/// The locale a single language tag maps to, or `None` to skip it.
fn match_tag(tag: &str) -> Option<Locale> {
    let mut subtags = tag.split(['-', '_']);
    let language = subtags.next()?;
    if language.eq_ignore_ascii_case("en") {
        return Some(Locale::En);
    }
    if !language.eq_ignore_ascii_case("zh") {
        return None;
    }
    // Skip extended language subtags (zh-cmn-Hans), then look at the script
    // or, without one, the region. Anything after those does not matter.
    let next = subtags.find(|s| !(s.len() == 3 && s.bytes().all(|b| b.is_ascii_alphabetic())));
    match next {
        Some(script) if script.len() == 4 && script.bytes().all(|b| b.is_ascii_alphabetic()) => {
            script.eq_ignore_ascii_case("Hans").then_some(Locale::ZhCn)
        }
        Some(region)
            if region.len() == 2
                || (region.len() == 3 && region.bytes().all(|b| b.is_ascii_digit())) =>
        {
            ["CN", "SG", "MY"]
                .iter()
                .any(|r| region.eq_ignore_ascii_case(r))
                .then_some(Locale::ZhCn)
        }
        // No script and no region: `zh`, or `zh` with only variants or extensions.
        _ => Some(Locale::ZhCn),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the negotiation table in docs/design/i18n.md per block.
    #[test]
    fn simplified_chinese_tags() {
        for tag in [
            "zh-Hans",
            "zh-Hans-CN",
            "zh-Hans-SG",
            "zh-Hans-TW",
            "zh-hans-cn",
            "zh",
            "zh-CN",
            "zh-SG",
            "zh-MY",
            "zh_CN",
            "ZH-cn",
        ] {
            assert_eq!(Locale::negotiate([tag]), Locale::ZhCn, "{tag}");
        }
    }

    #[test]
    fn traditional_chinese_tags_are_skipped() {
        for tag in [
            "zh-Hant",
            "zh-Hant-TW",
            "zh-Hant-HK",
            "zh-Hant-CN",
            "zh-TW",
            "zh-HK",
            "zh-MO",
            "zh_TW",
        ] {
            // Skipped, not mapped: the next preference decides.
            assert_eq!(
                Locale::negotiate([tag, "zh-Hans-CN"]),
                Locale::ZhCn,
                "{tag}"
            );
            assert_eq!(Locale::negotiate([tag, "en-US"]), Locale::En, "{tag}");
            assert_eq!(Locale::negotiate([tag]), Locale::En, "{tag}");
        }
    }

    #[test]
    fn english_tags() {
        for tag in ["en", "en-US", "en-GB", "en-Latn-US", "EN_us"] {
            assert_eq!(Locale::negotiate([tag, "zh-Hans-CN"]), Locale::En, "{tag}");
        }
    }

    #[test]
    fn other_tags_are_skipped() {
        for tag in ["ja-JP", "fr", "de-DE", "eo", "ens", "", "zhx", "zh-Latn"] {
            assert_eq!(
                Locale::negotiate([tag, "zh-Hans-CN"]),
                Locale::ZhCn,
                "{tag:?}"
            );
            assert_eq!(Locale::negotiate([tag]), Locale::En, "{tag:?}");
        }
    }

    #[test]
    fn first_match_wins() {
        assert_eq!(Locale::negotiate(["ja-JP", "zh-Hans-CN"]), Locale::ZhCn);
        assert_eq!(
            Locale::negotiate(["ja-JP", "zh-Hans-CN", "en-US"]),
            Locale::ZhCn
        );
        assert_eq!(
            Locale::negotiate(["ja-JP", "en-US", "zh-Hans-CN"]),
            Locale::En
        );
    }

    #[test]
    fn empty_list_is_english() {
        assert_eq!(Locale::negotiate([]), Locale::En);
        assert_eq!(Locale::negotiate(Vec::<&str>::new()), Locale::En);
    }

    #[test]
    fn tags() {
        assert_eq!(Locale::ALL.map(Locale::tag), ["en", "zh-CN"]);
    }
}
