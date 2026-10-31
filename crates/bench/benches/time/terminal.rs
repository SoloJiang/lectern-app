//! `terminal/*` (docs/design/terminal.md「性能」), step 2: the engine
//! without a PTY (`lectern_terminal::testing::Engine`) at 120×40. The
//! parsers' samples each feed a fresh terminal 1 MiB; the frames' samples
//! each build one frame after the output that dirtied its rows.

use std::cell::RefCell;

use lectern_bench::{Bench, BenchError, Ctx, Measured, Value};
use lectern_terminal::testing::{self, Engine};

const COLS: u16 = 120;
const ROWS: u16 = 40;

fn engine() -> Engine {
    Engine::new(testing::size(COLS, ROWS), testing::theme())
}

pub const PARSE_PLAIN: Bench = Bench {
    id: "terminal/parse_plain",
    run: parse_plain,
};

fn parse_plain(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    parse(ctx, "terminal/parse_plain", testing::plain_output)
}

pub const PARSE_SGR: Bench = Bench {
    id: "terminal/parse_sgr",
    run: parse_sgr,
};

fn parse_sgr(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    parse(ctx, "terminal/parse_sgr", testing::sgr_output)
}

/// `vt_write` of about 1 MiB (64 KiB in smoke mode) into a new terminal.
fn parse(
    ctx: &mut Ctx,
    id: &str,
    output: fn(usize, u16) -> Vec<u8>,
) -> Result<Vec<Measured>, BenchError> {
    let bytes = output(ctx.scale(1 << 20, 64 << 10), COLS);
    let len = bytes.len();
    let time = ctx.time_with_setup(engine, |mut e| {
        e.feed(&bytes);
        e
    });
    Ok(vec![
        Measured::new(id, Value::Time(time)).with_note(Some(format!("{len} bytes, {COLS}×{ROWS}"))),
    ])
}

pub const FRAME_FULL: Bench = Bench {
    id: "terminal/frame_full",
    run: frame_full,
};

/// Every row of the screen redrawn, then one frame.
fn frame_full(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let e = RefCell::new(engine());
    e.borrow_mut().frame();
    let mut round = 0;
    let time = ctx.time_with_setup(
        || {
            round += 1;
            e.borrow_mut()
                .feed(&testing::full_screen(COLS, ROWS, round));
        },
        |()| e.borrow_mut().frame(),
    );
    Ok(vec![Measured::new(
        "terminal/frame_full",
        Value::Time(time),
    )])
}

pub const FRAME_ECHO: Bench = Bench {
    id: "terminal/frame_echo",
    run: frame_echo,
};

/// A full screen, then one typed character echoed on the cursor's row and
/// one frame: only that row is dirty.
fn frame_echo(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let e = RefCell::new(engine());
    {
        let mut e = e.borrow_mut();
        e.feed(&testing::full_screen(COLS, ROWS, 0));
        e.feed(b"\r\n$ ");
        e.frame();
    }
    let mut typed = 0;
    let time = ctx.time_with_setup(
        || {
            typed += 1;
            // Start the line again before it wraps, so no row scrolls.
            let echo: &[u8] = if typed % 100 == 0 {
                b"\r\x1b[K$ "
            } else {
                b"x"
            };
            e.borrow_mut().feed(echo);
        },
        |()| e.borrow_mut().frame(),
    );
    Ok(vec![Measured::new(
        "terminal/frame_echo",
        Value::Time(time),
    )])
}
