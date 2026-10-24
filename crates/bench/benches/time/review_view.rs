//! `review_view/*` (docs/design/review_view.md「性能」). Step 1:
//! `review_view/items_5000`, the pure item list over a snapshot of 50 files,
//! 20 turns and 5000 prepared rows. GPUI only builds on macOS in this
//! workspace, so this module is compiled there alone; the budget is
//! `macos_only` and skipped elsewhere.

use std::collections::HashMap;
use std::sync::Arc;

use lectern_bench::{Bench, BenchError, Ctx, Measured, Value};
use lectern_review_view::items::{self, BuildOptions, DiffMode};
use lectern_review_view::testing::snapshot_fixture;

pub const BENCHES: &[Bench] = &[ITEMS_5000];

/// `items::build` for `testing::snapshot_fixture(50, 20, 100)` with every
/// row prepared: the brief, the notices, 20 turns, 50 steps with their
/// hunks and 5000 rows. In process, no window.
pub const ITEMS_5000: Bench = Bench {
    id: "review_view/items_5000",
    run: items_5000,
};

fn items_5000(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (snapshot, rows) = snapshot_fixture(50, 20, 100);
    let rows: HashMap<_, _> = rows
        .into_iter()
        .map(|(key, rows)| (key, Arc::new(rows)))
        .collect();
    let context = HashMap::new();
    let known_lines = HashMap::new();
    let options = BuildOptions {
        mode: DiffMode::Unified,
        earlier_collapsed: true,
        noise_expanded: false,
        context: &context,
        known_lines: &known_lines,
    };
    let built = items::build(&snapshot, &rows, &options).len();
    if built < 5000 {
        return Err(BenchError::new(format!(
            "the fixture should give 5000 rows or more, the list has {built} items"
        )));
    }
    let time = ctx.time(|| items::build(&snapshot, &rows, &options));
    Ok(vec![Measured::new(
        "review_view/items_5000",
        Value::Time(time),
    )])
}
