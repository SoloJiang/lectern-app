//! `i18n/frame_strings` (docs/design/i18n.md「性能」): record only.

use std::hint::black_box;

use lectern_bench::{Bench, BenchError, Ctx, Measured, Value};
use lectern_i18n::{Locale, Strings, testing};

/// Every message of the en resources once; those with variables formatted
/// with sample arguments.
pub const FRAME_STRINGS: Bench = Bench {
    id: "i18n/frame_strings",
    run: frame_strings,
};

fn frame_strings(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let strings = Strings::of(Locale::En);
    let time = ctx.time(|| testing::frame_strings(black_box(&strings)));
    Ok(vec![Measured::new("i18n/frame_strings", Value::Time(time))])
}
