use std::fmt::{self, Write as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::args::{Args, MAX_ARGS, Value};
use crate::table::{self, Part, Selector, Table, VarType};
use crate::{FmtKey, Key, Locale, keys, plural};

/// All interface strings of one language: a pointer to the static table
/// compiled into the binary. `Copy`, `Send + Sync`.
#[derive(Clone, Copy)]
pub struct Strings {
    locale: Locale,
    table: &'static Table,
}

impl Strings {
    /// The table of this language. Does not parse, allocate or read files;
    /// constant time.
    pub fn of(locale: Locale) -> Strings {
        let table = match locale {
            Locale::En => &table::EN,
            Locale::ZhCn => &table::ZH_CN,
        };
        Strings { locale, table }
    }

    pub fn locale(&self) -> Locale {
        self.locale
    }

    /// A message without variables.
    pub fn get(&self, key: Key) -> &'static str {
        match self.table[usize::from(key.index)] {
            [Part::Text(text)] => text,
            // build.rs compiles every message without variables to one Text.
            _ => unreachable!("{} is not plain text", table::NAMES[usize::from(key.index)]),
        }
    }

    /// A message with variables.
    ///
    /// A missing argument, or one of the wrong kind (a string where the
    /// message selects on a number, or the reverse), fails a `debug_assert!`.
    /// Release builds print `{$name}` in its place, take the default variant
    /// of any select on it, and log the problem once per key.
    pub fn format(&self, key: FmtKey, args: &Args<'_>) -> String {
        let index = usize::from(key.index);
        let (text, error) = self.format_checked(index, args);
        if let Some(error) = error {
            if first_report(index) {
                eprintln!("lectern-i18n: {}: {error}", table::NAMES[index]);
            }
            debug_assert!(false, "{}: {error}", table::NAMES[index]);
        }
        text
    }

    /// "just now", "5 min ago", "3 days ago" / "刚刚", "5 分钟前", "3 天前".
    /// The text comes from common.ftl; this only picks the message and unit.
    ///
    /// | Elapsed | Unit |
    /// |---|---|
    /// | under a minute | "just now" |
    /// | under an hour | minutes |
    /// | under a day | hours |
    /// | under 7 days | days |
    /// | under 30 days | weeks |
    /// | under 365 days | months of 30 days |
    /// | longer | years of 365 days |
    ///
    /// Counts are rounded down.
    pub fn relative_time(&self, elapsed: Duration) -> String {
        const MINUTE: u64 = 60;
        const HOUR: u64 = 60 * MINUTE;
        const DAY: u64 = 24 * HOUR;
        const WEEK: u64 = 7 * DAY;
        const MONTH: u64 = 30 * DAY;
        const YEAR: u64 = 365 * DAY;
        use keys::common::*;
        let secs = elapsed.as_secs();
        let (key, unit) = match secs {
            0..MINUTE => return self.get(TIME_JUST_NOW).to_owned(),
            MINUTE..HOUR => (TIME_MINUTES_AGO, MINUTE),
            HOUR..DAY => (TIME_HOURS_AGO, HOUR),
            DAY..WEEK => (TIME_DAYS_AGO, DAY),
            WEEK..MONTH => (TIME_WEEKS_AGO, WEEK),
            MONTH..YEAR => (TIME_MONTHS_AGO, MONTH),
            YEAR.. => (TIME_YEARS_AGO, YEAR),
        };
        // Exact below 2^53 seconds' worth of units, far beyond any real age.
        let count = (secs / unit) as f64;
        self.format(key, &Args::new().num("count", count))
    }

    /// Formats slot `index`, returning the first argument problem instead of
    /// reporting it.
    pub(crate) fn format_checked(
        &self,
        index: usize,
        args: &Args<'_>,
    ) -> (String, Option<ArgError>) {
        let vars = table::VARS[index];
        let mut values: [Option<Value<'_>>; MAX_ARGS] = [None; MAX_ARGS];
        let mut error = None;
        for (slot, &(name, ty)) in values.iter_mut().zip(vars) {
            match args.get(name) {
                Some(value) if accepts(ty, value) => *slot = Some(value),
                Some(_) => {
                    error.get_or_insert(ArgError::WrongType { name, expected: ty });
                }
                None => {
                    error.get_or_insert(ArgError::Missing { name });
                }
            }
        }
        let mut out = String::with_capacity(64);
        self.write(&mut out, self.table[index], vars, &values);
        (out, error)
    }

    fn write(
        &self,
        out: &mut String,
        parts: &[Part],
        vars: &[(&str, VarType)],
        values: &[Option<Value<'_>>],
    ) {
        for part in parts {
            match *part {
                Part::Text(text) => out.push_str(text),
                Part::Var(var) => match values[usize::from(var)] {
                    Some(Value::Num(n)) => {
                        let _ = write!(out, "{n}");
                    }
                    Some(Value::Str(s)) => {
                        out.push('\u{2068}');
                        out.push_str(s);
                        out.push('\u{2069}');
                    }
                    None => {
                        out.push_str("{$");
                        out.push_str(vars[usize::from(var)].0);
                        out.push('}');
                    }
                },
                Part::Select { var, default, arms } => {
                    let chosen = values[usize::from(var)]
                        .and_then(|value| {
                            arms.iter()
                                .find(|(selector, _)| self.matches(selector, value))
                        })
                        .unwrap_or(&arms[usize::from(default)]);
                    self.write(out, chosen.1, vars, values);
                }
            }
        }
    }

    /// Fluent's variant matching (fluent-bundle `FluentValue::matches`).
    fn matches(&self, selector: &Selector, value: Value<'_>) -> bool {
        match (selector, value) {
            (Selector::Plural(category), Value::Num(n)) => {
                plural::category(self.locale, n) == *category
            }
            (Selector::Plural(category), Value::Str(s)) => s == category.name(),
            (Selector::Exact(x), Value::Num(n)) => *x == n,
            (Selector::Str(k), Value::Str(s)) => *k == s,
            (Selector::Exact(_), Value::Str(_)) | (Selector::Str(_), Value::Num(_)) => false,
        }
    }
}

fn accepts(ty: VarType, value: Value<'_>) -> bool {
    matches!(
        (ty, value),
        (VarType::Any, _) | (VarType::Num, Value::Num(_)) | (VarType::Str, Value::Str(_))
    )
}

/// A caller error in the arguments to [`Strings::format`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ArgError {
    Missing {
        name: &'static str,
    },
    WrongType {
        name: &'static str,
        expected: VarType,
    },
}

impl fmt::Display for ArgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArgError::Missing { name } => write!(f, "missing argument ${name}"),
            ArgError::WrongType {
                name,
                expected: VarType::Num,
            } => write!(f, "${name} must be passed with Args::num"),
            ArgError::WrongType {
                name,
                expected: VarType::Str,
            } => write!(f, "${name} must be passed with Args::str"),
            ArgError::WrongType {
                name,
                expected: VarType::Any,
            } => write!(f, "${name} has the wrong type"),
        }
    }
}

/// One flag per slot, so a bad call in a render loop logs once per run.
static REPORTED: [AtomicBool; table::LEN] = [const { AtomicBool::new(false) }; table::LEN];

/// True the first time it is called for `index`.
fn first_report(index: usize) -> bool {
    !REPORTED[index].swap(true, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::keys::{common, review};

    fn both(f: impl Fn(Strings) -> String) -> String {
        Locale::ALL
            .iter()
            .map(|&l| format!("{:<5} {}", l.tag(), f(Strings::of(l))))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn strings_are_send_sync_copy() {
        fn check<T: Send + Sync + Copy>() {}
        check::<Strings>();
    }

    #[test]
    fn get_returns_static_text() {
        assert_eq!(
            Strings::of(Locale::En).get(review::LOOKS_GOOD),
            "Looks good"
        );
        assert_eq!(
            Strings::of(Locale::ZhCn).get(review::LOOKS_GOOD_TOOLTIP),
            "将所有文件标记为已看"
        );
        assert_eq!(Strings::of(Locale::ZhCn).locale(), Locale::ZhCn);
    }

    #[test]
    fn terms_are_expanded() {
        assert_eq!(
            Strings::of(Locale::En).get(keys::welcome::TITLE),
            "Welcome to Lectern"
        );
        assert_eq!(
            Strings::of(Locale::ZhCn).get(common::RESTART),
            "重新启动 Lectern"
        );
    }

    #[test]
    fn language_names_use_their_own_script() {
        for locale in Locale::ALL {
            let strings = Strings::of(locale);
            assert_eq!(strings.get(Locale::En.name_key()), "English");
            assert_eq!(strings.get(Locale::ZhCn.name_key()), "简体中文");
        }
    }

    #[test]
    fn plurals() {
        let text = [0.0, 1.0, 2.0, 5.0, 1.5]
            .iter()
            .map(|&n| {
                let files = both(|s| s.format(review::FILES_CHANGED, &Args::new().num("count", n)));
                let comments = both(|s| s.format(review::COMMENTS, &Args::new().num("count", n)));
                format!("count = {n}\n{files}\n{comments}")
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        insta::assert_snapshot!(text);
    }

    #[test]
    fn string_arguments_are_isolated() {
        let text = ["src/main.rs", "مجلد/ملف.rs", "תיקייה (2)"]
            .iter()
            .map(|path| {
                both(|s| {
                    s.format(review::REVIEWING, &Args::new().str("path", path))
                        .escape_unicode_isolates()
                })
            })
            .collect::<Vec<_>>()
            .join("\n");
        insta::assert_snapshot!(text);
    }

    #[test]
    fn numbers_are_not_isolated() {
        let args = Args::new().num("reviewed", 2).num("total", 6);
        assert_eq!(
            Strings::of(Locale::En).format(review::PROGRESS, &args),
            "2/6 reviewed"
        );
    }

    #[test]
    fn relative_time_intervals() {
        let cases: [(&str, u64); 20] = [
            ("0 s", 0),
            ("59 s", 59),
            ("1 min", 60),
            ("59 min 59 s", 3599),
            ("1 h", 3600),
            ("2 h", 7200),
            ("23 h 59 min", 86_399),
            ("1 d", 86_400),
            ("2 d", 2 * 86_400),
            ("6 d 23 h", 7 * 86_400 - 3600),
            ("7 d", 7 * 86_400),
            ("14 d", 14 * 86_400),
            ("29 d", 29 * 86_400),
            ("30 d", 30 * 86_400),
            ("60 d", 60 * 86_400),
            ("364 d", 364 * 86_400),
            ("365 d", 365 * 86_400),
            ("730 d", 730 * 86_400),
            ("10 y", 3650 * 86_400),
            ("u64::MAX s", u64::MAX),
        ];
        let text = cases
            .iter()
            .map(|&(label, secs)| {
                format!(
                    "{label}\n{}",
                    both(|s| s.relative_time(Duration::from_secs(secs)))
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        insta::assert_snapshot!(text);
    }

    #[test]
    fn selectors_match_like_fluent() {
        use crate::table::PluralCategory;
        static PARTS: &[Part] = &[Part::Select {
            var: 0,
            default: 3,
            arms: &[
                (Selector::Str("file"), &[Part::Text("File")]),
                (Selector::Exact(3.0), &[Part::Text("Three")]),
                (Selector::Plural(PluralCategory::One), &[Part::Text("One")]),
                (
                    Selector::Plural(PluralCategory::Other),
                    &[Part::Text("Other")],
                ),
            ],
        }];
        static VARS: &[(&str, VarType)] = &[("x", VarType::Any)];
        let run = |locale, value| {
            let mut out = String::new();
            Strings::of(locale).write(&mut out, PARTS, VARS, &[Some(value)]);
            out
        };
        assert_eq!(run(Locale::En, Value::Str("file")), "File");
        // A category name given as a string matches that category's key.
        assert_eq!(run(Locale::En, Value::Str("one")), "One");
        assert_eq!(run(Locale::En, Value::Str("folder")), "Other");
        // The first matching arm wins: 3 is `other` in en but matches [3] first.
        assert_eq!(run(Locale::En, Value::Num(3.0)), "Three");
        assert_eq!(run(Locale::En, Value::Num(1.0)), "One");
        assert_eq!(run(Locale::ZhCn, Value::Num(1.0)), "Other");
        assert_eq!(run(Locale::En, Value::Num(1.5)), "Other");
    }

    #[test]
    fn missing_argument_falls_back_in_release() {
        let en = Strings::of(Locale::En);
        let (text, error) =
            en.format_checked(usize::from(review::FILES_CHANGED.index), &Args::new());
        assert_eq!(text, "{$count} files changed");
        assert_eq!(error, Some(ArgError::Missing { name: "count" }));
    }

    #[test]
    fn wrong_argument_type_falls_back_in_release() {
        let en = Strings::of(Locale::En);
        let index = usize::from(review::FILES_CHANGED.index);
        let (text, error) = en.format_checked(index, &Args::new().str("count", "1"));
        assert_eq!(text, "{$count} files changed");
        assert_eq!(
            error,
            Some(ArgError::WrongType {
                name: "count",
                expected: VarType::Num
            })
        );
        assert_eq!(
            error.unwrap().to_string(),
            "$count must be passed with Args::num"
        );
    }

    #[test]
    fn any_typed_variable_takes_both_kinds() {
        let en = Strings::of(Locale::En);
        let index = usize::from(review::REVIEWING.index);
        assert_eq!(
            en.format_checked(index, &Args::new().num("path", 3)),
            ("Reviewing 3".to_owned(), None)
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "review-files-changed: missing argument $count")]
    fn missing_argument_fails_in_debug() {
        Strings::of(Locale::En).format(review::FILES_CHANGED, &Args::new());
    }

    #[test]
    fn errors_are_reported_once_per_key() {
        let index = usize::from(common::TIME_YEARS_AGO.index);
        assert!(first_report(index));
        assert!(!first_report(index));
        assert!(!first_report(index));
    }

    trait EscapeIsolates {
        fn escape_unicode_isolates(self) -> String;
    }

    impl EscapeIsolates for String {
        fn escape_unicode_isolates(self) -> String {
            self.replace('\u{2068}', "<FSI>")
                .replace('\u{2069}', "<PDI>")
        }
    }
}
