use lectern_i18n::{Locale, Strings, keys};

fn main() {
    let _ = Strings::of(Locale::En).get(keys::review::FILES_CHANGED);
}
