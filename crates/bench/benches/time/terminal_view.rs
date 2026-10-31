//! `terminal_view/*` (docs/design/terminal_view.md「性能」):
//! `terminal_view/snap_full`, the glyphs of a frame of 120 by 40 cells placed
//! on the grid from synthetic shaping and the draw list built (backgrounds,
//! the selection, the cursor, decorations, box characters). GPUI only builds
//! on macOS in this workspace, so this module is compiled there alone; the
//! budget is `macos_only` and skipped elsewhere. No window is opened.

use lectern_bench::{Bench, BenchError, Ctx, Measured, Value};
use lectern_terminal_view::testing::SnapFull;

pub const BENCHES: &[Bench] = &[SNAP_FULL];

/// `SnapFull::run`: every row of the frame is placed (nothing is in a cache)
/// and the draw list is built. In process, no window.
pub const SNAP_FULL: Bench = Bench {
    id: "terminal_view/snap_full",
    run: snap_full,
};

fn snap_full(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let fixture = SnapFull::new();
    let operations = fixture.run();
    // A frame of 120 by 40 cells with this much text has thousands of glyphs
    // and some rectangles; a fixture that built a handful would measure nothing.
    if operations < 2000 || fixture.cells_drawn() < 3000 {
        return Err(BenchError::new(format!(
            "the fixture should give 40 rows of about 100 clusters; it gave {} clusters and \
             {operations} drawing operations",
            fixture.cells_drawn()
        )));
    }
    let time = ctx.time(|| fixture.run());
    Ok(vec![Measured::new(
        "terminal_view/snap_full",
        Value::Time(time),
    )])
}
