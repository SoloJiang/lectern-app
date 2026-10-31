//! From an area to a grid (docs/design/terminal_view.md「接口」`grid_size`,
//! 「数据结构」「格子与字形」): the 4pt margin, the cell size, the least of
//! 20 columns. `cells` is the pure part; `grid_size` reads the font metrics
//! and the scale of the window.

use gpui::{App, Pixels, Size, Window, px, size};
use lectern_services::terminal::GridSize;
use lectern_ui::{MonoMetrics, mono_metrics};

/// The margin on each of the four sides of the grid, in points.
pub(crate) const INSET: f32 = 4.0;

/// The fewest columns. An area that cannot hold them is painted with 20
/// columns and clipped on the right.
pub(crate) const MIN_COLS: u16 = 20;

/// The size of the grid in `area` (the logical size the view may use), for
/// the font `metrics` on a display of `scale` device pixels per point.
/// Columns at least `MIN_COLS`, rows at least 1. `cell_width` and
/// `cell_height` are the cell in device pixels, rounded.
pub fn cells(area: Size<Pixels>, metrics: &MonoMetrics, scale: f32) -> GridSize {
    let usable_width = (area.width.as_f32() - 2.0 * INSET).max(0.0);
    let usable_height = (area.height.as_f32() - 2.0 * INSET).max(0.0);
    let cell_width = metrics.cell_width.as_f32().max(f32::EPSILON);
    let cell_height = metrics.line_height.as_f32().max(f32::EPSILON);
    // `floor` of a quotient of two floats that is an integer in exact
    // arithmetic (7.2 * 10 / 7.2) must not fall to the integer below.
    let count = |usable: f32, cell: f32| ((usable / cell) + 1e-4).floor().max(0.0);
    let cols = count(usable_width, cell_width).min(f32::from(u16::MAX)) as u16;
    let rows = count(usable_height, cell_height).min(f32::from(u16::MAX)) as u16;
    GridSize {
        cols: cols.max(MIN_COLS),
        rows: rows.max(1),
        cell_width: device_pixels(cell_width, scale),
        cell_height: device_pixels(cell_height, scale),
    }
}

fn device_pixels(logical: f32, scale: f32) -> u16 {
    (logical * scale).round().clamp(1.0, f32::from(u16::MAX)) as u16
}

/// The grid for an area, as the host needs it before `Terminals::open`:
/// `area` is the logical size the view may use; the font is measured on the
/// first call and read from the cache afterwards.
pub fn grid_size(area: Size<Pixels>, window: &Window, cx: &mut App) -> GridSize {
    let metrics = mono_metrics(window, cx);
    cells(area, &metrics, window.scale_factor())
}

/// The size in points of a grid of `size` cells, without the margin.
pub(crate) fn extent(size_in_cells: GridSize, metrics: &MonoMetrics) -> Size<Pixels> {
    size(
        px(f32::from(size_in_cells.cols) * metrics.cell_width.as_f32()),
        px(f32::from(size_in_cells.rows) * metrics.line_height.as_f32()),
    )
}
