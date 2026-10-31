//! `mouse_*` and `wheel_*` (docs/design/terminal_view.md「鼠标与滚轮」):
//! pixels to cells, the click counts, the moves that are sent, the scrolling
//! of a drag that left the grid, and the wheel's whole lines and its sign.

use std::time::Duration;

use gpui::{
    Modifiers, MouseButton as GpuiButton, MouseDownEvent, MouseUpEvent, Pixels, Point, ScrollDelta,
    ScrollWheelEvent, TestAppContext, TouchPhase, point, px,
};
use lectern_services::terminal::{
    Input, Modes, Mods, MouseButton, MouseInput, MouseKind, ScrollInput,
};

use crate::input::{cell_at, drag_direction, wheel_lines};
use crate::testing::frame;
use crate::tests::support::{Opened, Rig};

const CELL_WIDTH: f32 = 7.2;
const LINE_HEIGHT: f32 = 16.0;
const INSET: f32 = 4.0;

/// The centre of a cell, in window coordinates.
fn centre(col: f32, row: f32) -> Point<Pixels> {
    point(
        px(INSET + (col + 0.5) * CELL_WIDTH),
        px(INSET + (row + 0.5) * LINE_HEIGHT),
    )
}

fn mouse(kind: MouseKind, button: Option<MouseButton>, col: u16, row: u16, clicks: u8) -> Input {
    Input::Mouse(MouseInput {
        kind,
        button,
        mods: Mods::default(),
        col,
        row,
        x: (f32::from(col) + 0.5) * CELL_WIDTH,
        y: (f32::from(row) + 0.5) * LINE_HEIGHT,
        clicks,
    })
}

fn rig_with(modes: Modes, cx: &mut TestAppContext) -> (Rig, Opened) {
    let rig = Rig::new(cx);
    let opened = rig.open(cx, Some(frame(&["$ hello"]).modes(modes).build()));
    rig.inputs();
    (rig, opened)
}

fn rig(cx: &mut TestAppContext) -> (Rig, Opened) {
    rig_with(Modes::default(), cx)
}

#[test]
fn mouse_pixels_to_cells() {
    let grid = gpui::Bounds {
        origin: point(px(4.0), px(4.0)),
        size: gpui::size(px(10.0 * CELL_WIDTH), px(3.0 * LINE_HEIGHT)),
    };
    let cell = gpui::size(px(CELL_WIDTH), px(LINE_HEIGHT));
    let at = |x: f32, y: f32| cell_at(point(px(x), px(y)), &grid, cell, 10, 3);
    assert_eq!(at(4.0, 4.0), (0, 0));
    assert_eq!(
        at(4.0 + CELL_WIDTH - 0.01, 4.0 + LINE_HEIGHT - 0.01),
        (0, 0)
    );
    assert_eq!(at(4.0 + CELL_WIDTH, 4.0 + LINE_HEIGHT), (1, 1));
    // Outside the grid the cell is clamped into it.
    assert_eq!(at(0.0, 0.0), (0, 0));
    assert_eq!(at(1000.0, 1000.0), (9, 2));
    assert_eq!(at(-50.0, 30.0), (0, 1));
}

#[test]
fn mouse_drag_direction_outside_the_grid() {
    let grid = gpui::Bounds {
        origin: point(px(4.0), px(4.0)),
        size: gpui::size(px(72.0), px(48.0)),
    };
    assert_eq!(drag_direction(point(px(10.0), px(1.0)), &grid), -1);
    assert_eq!(drag_direction(point(px(10.0), px(20.0)), &grid), 0);
    assert_eq!(drag_direction(point(px(10.0), px(60.0)), &grid), 1);
}

#[gpui::test]
fn mouse_down_and_up_send_the_cell(cx: &mut TestAppContext) {
    let (rig, opened) = rig(cx);
    let mut visual = opened.visual(cx);
    visual.simulate_mouse_down(centre(3.0, 2.0), GpuiButton::Left, Modifiers::none());
    visual.simulate_mouse_up(centre(3.0, 2.0), GpuiButton::Left, Modifiers::none());
    assert_eq!(
        rig.inputs(),
        vec![
            mouse(MouseKind::Down, Some(MouseButton::Left), 3, 2, 1),
            mouse(MouseKind::Up, Some(MouseButton::Left), 3, 2, 1),
        ]
    );
}

#[gpui::test]
fn mouse_clicks_are_counted(cx: &mut TestAppContext) {
    let (rig, opened) = rig(cx);
    let mut visual = opened.visual(cx);
    for clicks in [1usize, 2, 3] {
        visual.simulate_event(MouseDownEvent {
            position: centre(1.0, 0.0),
            modifiers: Modifiers::none(),
            button: GpuiButton::Left,
            click_count: clicks,
            first_mouse: false,
        });
        visual.simulate_event(MouseUpEvent {
            position: centre(1.0, 0.0),
            modifiers: Modifiers::none(),
            button: GpuiButton::Left,
            click_count: clicks,
        });
    }
    let downs: Vec<u8> = rig
        .inputs()
        .into_iter()
        .filter_map(|input| match input {
            Input::Mouse(MouseInput {
                kind: MouseKind::Down,
                clicks,
                ..
            }) => Some(clicks),
            _ => None,
        })
        .collect();
    assert_eq!(downs, vec![1, 2, 3]);
}

#[gpui::test]
fn mouse_modifiers_and_buttons(cx: &mut TestAppContext) {
    let (rig, opened) = rig(cx);
    let mut visual = opened.visual(cx);
    let shift = Modifiers {
        shift: true,
        ..Modifiers::none()
    };
    visual.simulate_mouse_down(centre(0.0, 0.0), GpuiButton::Right, shift);
    visual.simulate_mouse_up(centre(0.0, 0.0), GpuiButton::Right, shift);
    let inputs = rig.inputs();
    let Input::Mouse(down) = &inputs[0] else {
        panic!("{inputs:?}")
    };
    assert_eq!(down.button, Some(MouseButton::Right));
    assert!(down.mods.shift);
}

/// Without mouse tracking, a pointer moving over the grid sends nothing; with
/// it, one `Move` for each cell it enters.
#[gpui::test]
fn mouse_move_only_with_mouse_tracking(cx: &mut TestAppContext) {
    let (rig, opened) = rig(cx);
    let mut visual = opened.visual(cx);
    visual.simulate_mouse_move(centre(2.0, 1.0), None, Modifiers::none());
    visual.simulate_mouse_move(centre(3.0, 1.0), None, Modifiers::none());
    assert_eq!(rig.inputs(), Vec::<Input>::new());

    let (rig, opened) = rig_with(
        Modes {
            mouse_tracking: true,
            ..Modes::default()
        },
        cx,
    );
    let mut visual = opened.visual(cx);
    visual.simulate_mouse_move(centre(2.0, 1.0), None, Modifiers::none());
    visual.simulate_mouse_move(
        point(px(INSET + 2.9 * CELL_WIDTH), px(24.0)),
        None,
        Modifiers::none(),
    );
    visual.simulate_mouse_move(centre(3.0, 1.0), None, Modifiers::none());
    assert_eq!(
        rig.inputs(),
        vec![
            mouse(MouseKind::Move, None, 2, 1, 0),
            mouse(MouseKind::Move, None, 3, 1, 0),
        ],
        "one Move per cell"
    );
}

#[gpui::test]
fn mouse_drag_sends_a_drag_when_the_cell_changes(cx: &mut TestAppContext) {
    let (rig, opened) = rig(cx);
    let mut visual = opened.visual(cx);
    visual.simulate_mouse_down(centre(1.0, 0.0), GpuiButton::Left, Modifiers::none());
    visual.simulate_mouse_move(centre(1.0, 0.0), Some(GpuiButton::Left), Modifiers::none());
    visual.simulate_mouse_move(centre(4.0, 0.0), Some(GpuiButton::Left), Modifiers::none());
    visual.simulate_mouse_move(centre(4.0, 0.0), Some(GpuiButton::Left), Modifiers::none());
    visual.simulate_mouse_move(centre(5.0, 1.0), Some(GpuiButton::Left), Modifiers::none());
    visual.simulate_mouse_up(centre(5.0, 1.0), GpuiButton::Left, Modifiers::none());
    // A move after the release is not a drag (and, without tracking, nothing).
    visual.simulate_mouse_move(centre(6.0, 1.0), None, Modifiers::none());
    let left = Some(MouseButton::Left);
    assert_eq!(
        rig.inputs(),
        vec![
            mouse(MouseKind::Down, left, 1, 0, 1),
            mouse(MouseKind::Drag, left, 4, 0, 0),
            mouse(MouseKind::Drag, left, 5, 1, 0),
            mouse(MouseKind::Up, left, 5, 1, 1),
        ]
    );
}

fn scrolls(inputs: &[Input]) -> Vec<i32> {
    inputs
        .iter()
        .filter_map(|input| match input {
            Input::Scroll(ScrollInput { lines, .. }) => Some(*lines),
            _ => None,
        })
        .collect()
}

/// A drag above the grid scrolls it up a line every 50 ms with the drag at the
/// first row, until the pointer comes back or the button is released.
#[gpui::test]
fn mouse_drag_above_the_grid_scrolls_until_it_returns(cx: &mut TestAppContext) {
    let (rig, opened) = rig(cx);
    let mut visual = opened.visual(cx);
    visual.simulate_mouse_down(centre(2.0, 3.0), GpuiButton::Left, Modifiers::none());
    rig.inputs();
    visual.simulate_mouse_move(
        point(px(20.0), px(1.0)),
        Some(GpuiButton::Left),
        Modifiers::none(),
    );
    let first = rig.inputs();
    assert_eq!(
        scrolls(&first),
        Vec::<i32>::new(),
        "the first line comes after 50 ms"
    );
    for _ in 0..3 {
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
    }
    let ticks = rig.inputs();
    assert_eq!(scrolls(&ticks), vec![-1, -1, -1]);
    assert!(ticks.iter().any(|input| matches!(
        input,
        Input::Mouse(MouseInput {
            kind: MouseKind::Drag,
            row: 0,
            ..
        })
    )));
    // Back inside the grid: it stops.
    visual.simulate_mouse_move(centre(2.0, 2.0), Some(GpuiButton::Left), Modifiers::none());
    rig.inputs();
    cx.executor().advance_clock(Duration::from_millis(200));
    cx.run_until_parked();
    assert_eq!(scrolls(&rig.inputs()), Vec::<i32>::new());
}

#[gpui::test]
fn mouse_drag_below_the_grid_scrolls_down_and_the_release_stops_it(cx: &mut TestAppContext) {
    let (rig, opened) = rig(cx);
    let mut visual = opened.visual(cx);
    let (grid, bottom_row) = opened.pane.read_with(cx, |pane, _| {
        (
            pane.grid,
            pane.grid.size.height.as_f32() / LINE_HEIGHT - 1.0,
        )
    });
    visual.simulate_mouse_down(centre(2.0, 0.0), GpuiButton::Left, Modifiers::none());
    rig.inputs();
    let below = point(px(20.0), grid.origin.y + grid.size.height + px(2.0));
    visual.simulate_mouse_move(below, Some(GpuiButton::Left), Modifiers::none());
    cx.executor().advance_clock(Duration::from_millis(50));
    cx.run_until_parked();
    let inputs = rig.inputs();
    assert_eq!(scrolls(&inputs), vec![1]);
    let Some(Input::Scroll(scroll)) = inputs
        .iter()
        .find(|input| matches!(input, Input::Scroll(_)))
    else {
        panic!("{inputs:?}")
    };
    assert_eq!(f32::from(scroll.row), bottom_row.round());
    visual.simulate_mouse_up(below, GpuiButton::Left, Modifiers::none());
    rig.inputs();
    cx.executor().advance_clock(Duration::from_millis(200));
    cx.run_until_parked();
    assert_eq!(
        rig.inputs(),
        Vec::<Input>::new(),
        "released: no more scrolling"
    );
}

fn wheel(delta: ScrollDelta) -> ScrollWheelEvent {
    ScrollWheelEvent {
        position: centre(2.0, 1.0),
        delta,
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
    }
}

#[test]
fn wheel_sign_and_whole_lines() {
    let mut rest = 0.0;
    let line = px(LINE_HEIGHT);
    // GPUI's positive y shows earlier lines: the lines are negative.
    assert_eq!(
        wheel_lines(&mut rest, &ScrollDelta::Lines(point(0.0, 3.0)), line),
        -3
    );
    assert_eq!(
        wheel_lines(&mut rest, &ScrollDelta::Lines(point(0.0, -2.0)), line),
        2
    );
    // Pixels are lines of the cell height, the remainder carried over.
    let pixels = |y: f32| ScrollDelta::Pixels(point(px(0.0), px(y)));
    assert_eq!(wheel_lines(&mut rest, &pixels(10.0), line), 0);
    assert_eq!(wheel_lines(&mut rest, &pixels(10.0), line), -1);
    assert!((rest + 0.25).abs() < 1e-4, "rest {rest}");
    assert_eq!(wheel_lines(&mut rest, &pixels(-40.0), line), 2);
    assert_eq!(wheel_lines(&mut rest, &pixels(0.0), line), 0);
}

#[gpui::test]
fn wheel_sends_whole_lines_at_the_cell(cx: &mut TestAppContext) {
    let (rig, opened) = rig(cx);
    let mut visual = opened.visual(cx);
    visual.simulate_event(wheel(ScrollDelta::Lines(point(0.0, 3.0))));
    visual.simulate_event(wheel(ScrollDelta::Pixels(point(px(0.0), px(-24.0)))));
    visual.simulate_event(wheel(ScrollDelta::Pixels(point(px(0.0), px(4.0)))));
    let scroll = |lines| {
        Input::Scroll(ScrollInput {
            lines,
            col: 2,
            row: 1,
            mods: Mods::default(),
        })
    };
    // -24 px is 1.5 lines: one is sent, half a line stays; +4 px makes it
    // 0.25 line further the other way and sends nothing.
    assert_eq!(rig.inputs(), vec![scroll(-3), scroll(1)]);
}

#[gpui::test]
fn wheel_outside_the_grid_is_not_the_terminals(cx: &mut TestAppContext) {
    let (rig, opened) = rig(cx);
    let mut visual = opened.visual(cx);
    let mut event = wheel(ScrollDelta::Lines(point(0.0, 3.0)));
    event.position = point(px(5000.0), px(5000.0));
    visual.simulate_event(event);
    assert_eq!(rig.inputs(), Vec::<Input>::new());
}
