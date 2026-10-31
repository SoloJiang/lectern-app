//! `grid_size_*` and `resize_*` (docs/design/terminal_view.md「接口」
//! `grid_size`, 「数据流与线程」「尺寸」): the rounding of `cells`, the margin,
//! the least of 20 columns, and a `Resize` only when the size changed.

use gpui::{TestAppContext, px, size};
use lectern_services::terminal::{GridSize, Input};

use crate::testing::{frame, test_metrics};
use crate::tests::support::Rig;
use crate::{cells, grid_size};

fn grid(cols: u16, rows: u16, cell_width: u16, cell_height: u16) -> GridSize {
    GridSize {
        cols,
        rows,
        cell_width,
        cell_height,
    }
}

#[test]
fn grid_size_rounding() {
    let metrics = test_metrics();
    // (300 - 8) / 7.2 = 40.5 columns, (200 - 8) / 16 = 12 rows; the cell in
    // device pixels is 14.4 by 32 at 2x (14 by 32), 7.2 by 16 at 1x (7 by 16).
    assert_eq!(
        cells(size(px(300.0), px(200.0)), &metrics, 2.0),
        grid(40, 12, 14, 32)
    );
    assert_eq!(
        cells(size(px(300.0), px(200.0)), &metrics, 1.0),
        grid(40, 12, 7, 16)
    );
}

#[test]
fn grid_size_margin_is_four_points_on_each_side() {
    let metrics = test_metrics();
    // 30 columns fit exactly when the area is 30 cells and the margins.
    let width = 30.0 * 7.2 + 8.0;
    let height = 10.0 * 16.0 + 8.0;
    assert_eq!(
        cells(size(px(width), px(height)), &metrics, 1.0),
        grid(30, 10, 7, 16)
    );
    // One point less leaves one column and one row out.
    assert_eq!(
        cells(size(px(width - 1.0), px(height - 1.0)), &metrics, 1.0),
        grid(29, 9, 7, 16)
    );
}

#[test]
fn grid_size_has_at_least_20_columns_and_one_row() {
    let metrics = test_metrics();
    assert_eq!(
        cells(size(px(100.0), px(10.0)), &metrics, 1.0),
        grid(20, 1, 7, 16)
    );
    assert_eq!(
        cells(size(px(0.0), px(0.0)), &metrics, 1.0),
        grid(20, 1, 7, 16)
    );
}

#[gpui::test]
fn grid_size_reads_the_font_and_the_scale_of_the_window(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, None);
    let mut visual = opened.visual(cx);
    let (got, scale) = visual.update(|window, cx| {
        (
            grid_size(size(px(300.0), px(200.0)), window, cx),
            window.scale_factor(),
        )
    });
    assert_eq!(
        got,
        cells(size(px(300.0), px(200.0)), &test_metrics(), scale)
    );
}

/// The first layout sends the size once; the same size again sends nothing.
#[gpui::test]
fn resize_is_sent_once_for_one_size(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(frame(&["$"]).build()));
    let mut visual = opened.visual(cx);
    let (window_size, scale) =
        visual.update(|window, _| (window.viewport_size(), window.scale_factor()));
    let expected = cells(window_size, &test_metrics(), scale);
    let resizes: Vec<_> = rig
        .inputs()
        .into_iter()
        .filter(|input| matches!(input, Input::Resize(_)))
        .collect();
    assert_eq!(resizes, vec![Input::Resize(expected)]);
    for _ in 0..3 {
        opened.draw(cx);
    }
    assert_eq!(rig.inputs(), Vec::<Input>::new());
}

#[gpui::test]
fn resize_follows_the_window(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(frame(&["$"]).build()));
    rig.inputs();
    let mut visual = opened.visual(cx);
    let scale = visual.update(|window, _| window.scale_factor());
    visual.simulate_resize(size(px(400.0), px(300.0)));
    opened.draw(cx);
    let expected = cells(size(px(400.0), px(300.0)), &test_metrics(), scale);
    assert_eq!(rig.inputs(), vec![Input::Resize(expected)]);
    // A size that gives the same grid is not sent again.
    visual.simulate_resize(size(px(401.0), px(301.0)));
    opened.draw(cx);
    assert_eq!(rig.inputs(), Vec::<Input>::new());
}

/// An area of fewer than 20 columns: the grid has 20 and is clipped on the right.
#[gpui::test]
fn resize_in_a_narrow_area_sends_20_columns(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(frame(&["$"]).build()));
    rig.inputs();
    let visual = opened.visual(cx);
    visual.simulate_resize(size(px(100.0), px(200.0)));
    opened.draw(cx);
    let resizes = rig.inputs();
    let Some(Input::Resize(got)) = resizes.last() else {
        panic!("no Resize: {resizes:?}");
    };
    assert_eq!(got.cols, 20);
}
