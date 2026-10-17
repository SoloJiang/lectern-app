/// Most arguments one `Args` holds. build/compile.rs rejects messages with
/// more variables (`MAX_VARS` there).
pub(crate) const MAX_ARGS: usize = 8;

/// An argument value.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Value<'a> {
    Num(f64),
    Str(&'a str),
}

/// Formatting arguments, passed by variable name.
///
/// A small inline array: building one does not allocate. Passing an argument
/// the message does not use is allowed; the first argument with a given name
/// wins.
///
/// ```
/// use lectern_i18n::{Args, Locale, Strings, keys};
///
/// let args = Args::new().num("reviewed", 2).num("total", 6);
/// assert_eq!(Strings::of(Locale::En).format(keys::review::PROGRESS, &args), "2/6 reviewed");
/// assert_eq!(Strings::of(Locale::ZhCn).format(keys::review::PROGRESS, &args), "已看 2/6");
/// ```
#[derive(Default)]
pub struct Args<'a> {
    entries: [Option<(&'a str, Value<'a>)>; MAX_ARGS],
    len: usize,
}

impl<'a> Args<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    /// A number. Takes part in plural and number selection; printed the way
    /// Rust prints an `f64` (`5`, `1.5`).
    pub fn num(self, name: &'a str, value: impl Into<f64>) -> Self {
        self.push(name, Value::Num(value.into()))
    }

    /// Text inserted as written, such as a path or a session title. The output
    /// wraps it in U+2068 FIRST STRONG ISOLATE and U+2069 POP DIRECTIONAL
    /// ISOLATE so that right-to-left text cannot reorder the surrounding
    /// punctuation.
    pub fn str(self, name: &'a str, value: &'a str) -> Self {
        self.push(name, Value::Str(value))
    }

    fn push(mut self, name: &'a str, value: Value<'a>) -> Self {
        if self.len < MAX_ARGS {
            self.entries[self.len] = Some((name, value));
            self.len += 1;
        } else {
            debug_assert!(false, "more than {MAX_ARGS} arguments; `{name}` is dropped");
        }
        self
    }

    pub(crate) fn get(&self, name: &str) -> Option<Value<'a>> {
        self.entries[..self.len]
            .iter()
            .flatten()
            .find(|(n, _)| *n == name)
            .map(|&(_, value)| value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_argument_with_a_name_wins() {
        let args = Args::new()
            .num("count", 1)
            .str("count", "x")
            .str("path", "a/b");
        assert!(matches!(args.get("count"), Some(Value::Num(n)) if n == 1.0));
        assert!(matches!(args.get("path"), Some(Value::Str("a/b"))));
        assert!(args.get("missing").is_none());
    }

    #[test]
    fn holds_max_args_without_allocating() {
        let names = ["a", "b", "c", "d", "e", "f", "g", "h"];
        let args = names
            .iter()
            .fold(Args::new(), |args, name| args.str(name, name));
        for name in names {
            assert!(args.get(name).is_some());
        }
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "more than 8 arguments")]
    fn too_many_arguments_fail_in_debug() {
        let names = ["a", "b", "c", "d", "e", "f", "g", "h", "i"];
        let _ = names
            .iter()
            .fold(Args::new(), |args, name| args.str(name, name));
    }
}
