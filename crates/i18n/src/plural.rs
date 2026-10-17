//! Hand-written CLDR cardinal plural rules for the supported languages.

use crate::Locale;
use crate::table::PluralCategory;

/// The plural category of `n` in `locale`.
///
/// - en: `one` when the integer part of |n| is 1 and there are no decimals
///   (CLDR `i = 1 and v = 0`), otherwise `other`.
/// - zh-CN: always `other`.
pub(crate) fn category(locale: Locale, n: f64) -> PluralCategory {
    match locale {
        Locale::En if n.abs() == 1.0 => PluralCategory::One,
        Locale::En | Locale::ZhCn => PluralCategory::Other,
    }
}

#[cfg(test)]
mod tests {
    use intl_pluralrules::{PluralCategory as Cldr, PluralRuleType, PluralRules};

    use super::*;

    fn cldr(tag: &str) -> PluralRules {
        PluralRules::create(
            tag.parse::<unic_langid::LanguageIdentifier>().unwrap(),
            PluralRuleType::CARDINAL,
        )
        .unwrap()
    }

    fn convert(category: Cldr) -> PluralCategory {
        match category {
            Cldr::ONE => PluralCategory::One,
            Cldr::OTHER => PluralCategory::Other,
            other => panic!("unexpected category {other:?}"),
        }
    }

    #[test]
    fn matches_intl_pluralrules() {
        let numbers = (0..=1000)
            .map(f64::from)
            .chain([0.5, 1.5, 2.25, 10.1, 100.75, 999.9, -1.0, -2.0, -1.5]);
        for (locale, tag) in [(Locale::En, "en"), (Locale::ZhCn, "zh")] {
            let rules = cldr(tag);
            for n in numbers.clone() {
                assert_eq!(
                    category(locale, n),
                    convert(rules.select(n).unwrap()),
                    "{tag} {n}"
                );
            }
        }
    }
}
