use lectern_i18n::{Args, Locale, Strings, keys};

fn main() {
    let _ = Strings::of(Locale::En).format(keys::review::LOOKS_GOOD, &Args::new());
}
