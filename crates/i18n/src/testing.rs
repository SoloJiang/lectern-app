//! Test support (`test-support` feature): every string of the tables once,
//! for bench's `i18n/frame_strings` (docs/design/i18n.md「性能」).

use crate::table::{VARS, VarType};
use crate::{Args, FmtKey, Key, Strings};

/// Every message value and attribute of `strings`' language once, as the
/// text of a typical frame is asked for: `get` for those without variables,
/// `format` with sample arguments (numbers 3, strings "src/lib.rs") for the
/// others. Returns the total length of the text in bytes.
pub fn frame_strings(strings: &Strings) -> usize {
    let mut bytes = 0;
    for (index, vars) in VARS.iter().enumerate() {
        let index = u16::try_from(index).expect("build.rs checks the table fits a u16");
        if vars.is_empty() {
            bytes += strings.get(Key::new(index)).len();
            continue;
        }
        let mut args = Args::new();
        for (name, ty) in vars.iter() {
            args = match ty {
                VarType::Str => args.str(name, "src/lib.rs"),
                VarType::Num | VarType::Any => args.num(name, 3),
            };
        }
        bytes += strings.format(FmtKey::new(index), &args).len();
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Locale, keys};

    #[test]
    fn frame_strings_formats_every_slot_in_both_locales() {
        for locale in Locale::ALL {
            let strings = Strings::of(locale);
            // Every slot contributes, so the total is at least that of two
            // known ones.
            let known = strings.get(keys::common::OK).len()
                + strings
                    .format(keys::review::FILES_CHANGED, &Args::new().num("count", 3))
                    .len();
            assert!(frame_strings(&strings) > known, "{locale:?}");
        }
    }
}
