//! `terminal/*` (docs/design/terminal.md「性能」). Step 2: the engine
//! without a PTY (`lectern_terminal::testing::Engine`) at 120×40. The
//! parsers' samples each feed a fresh terminal 1 MiB; the frames' samples
//! each build one frame after the output that dirtied its rows. Step 3: the
//! plugin in a `TestKernel` (`lectern_terminal::testing::Harness`) with a
//! real PTY running `/bin/cat` as the shell: `open`, `input` and the echo's
//! way from `input` to a new frame.

use std::cell::RefCell;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lectern_bench::{Bench, BenchError, Ctx, Measured, Value};
use lectern_services::terminal::{Frame, Input, Launch, OpenSpec, RunState, TerminalId, Terminals};
use lectern_terminal::testing::{self, Engine, Harness};

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

// ----------------------------------------------------------------- the PTY

/// The plugin with fakes for the projects and sessions, `/bin/cat` as the
/// shell and a fixed environment: no user shell, no login state, no agent.
/// Returns the harness and the (canonical) working directory.
fn plugin_harness(ctx: &Ctx) -> Result<(Harness, Arc<Path>), BenchError> {
    let project = ctx.scratch().join("terminal-project");
    std::fs::create_dir_all(&project)
        .map_err(|e| BenchError::new(format!("creating the project directory: {e}")))?;
    let project = std::fs::canonicalize(&project)
        .map_err(|e| BenchError::new(format!("canonicalizing the project directory: {e}")))?;
    let plugin = Harness::plugin()
        .with_login_env(vec![("PATH".into(), "/usr/bin:/bin".into())])
        .with_shell("/bin/cat");
    let harness = Harness::start(plugin);
    harness.projects.add(&project);
    Ok((harness, Arc::from(project.as_path())))
}

fn open_spec(cwd: &Arc<Path>) -> OpenSpec {
    OpenSpec {
        launch: Launch::Shell { cwd: cwd.clone() },
        size: Some(testing::size(COLS, ROWS)),
        theme: testing::theme(),
    }
}

/// Opens a shell terminal and waits until it runs.
fn running(harness: &Harness, cwd: &Arc<Path>) -> Result<TerminalId, BenchError> {
    let id = harness
        .terminals
        .open(open_spec(cwd))
        .map_err(|e| BenchError::new(format!("open refused: {e:?}")))?;
    harness
        .wait_for(Duration::from_secs(20), || {
            harness
                .terminals
                .info(id)
                .filter(|i| matches!(i.state, RunState::Running { .. }))
        })
        .ok_or_else(|| BenchError::new("the terminal did not start"))?;
    Ok(id)
}

pub const OPEN: Bench = Bench {
    id: "terminal/open",
    run: open,
};

/// A closed terminal when dropped, so the timed call is `open` alone and
/// the table never holds more than one terminal.
struct Opened(Arc<dyn Terminals>, TerminalId);

impl Drop for Opened {
    fn drop(&mut self) {
        self.0.close(self.1);
    }
}

/// The calling thread's part of `open`: the checks, the table entry and the
/// two threads' start. The terminal is closed again after each sample.
fn open(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (harness, cwd) = plugin_harness(ctx)?;
    let time = ctx.time_with_setup(
        // Give the threads of the last sample time to end.
        || std::thread::sleep(Duration::from_millis(2)),
        |()| {
            let id = harness.terminals.open(open_spec(&cwd)).expect("opens");
            Opened(harness.terminals.clone(), id)
        },
    );
    Ok(vec![Measured::new("terminal/open", Value::Time(time))])
}

pub const INPUT: Bench = Bench {
    id: "terminal/input",
    run: input,
};

/// One `input` of a character on a running terminal whose thread is asleep
/// again: the queue push and the wake byte.
fn input(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (harness, cwd) = plugin_harness(ctx)?;
    let id = running(&harness, &cwd)?;
    let text: Arc<str> = Arc::from("x");
    let time = ctx.time_with_setup(
        || std::thread::sleep(Duration::from_micros(200)),
        |()| harness.terminals.input(id, Input::Text(text.clone())),
    );
    Ok(vec![Measured::new("terminal/input", Value::Time(time))])
}

pub const ECHO_LATENCY: Bench = Bench {
    id: "terminal/echo_latency",
    run: echo_latency,
};

/// The number of `x` on the screen.
fn typed(frame: &Frame) -> usize {
    frame.rows.iter().map(|r| r.text.matches('x').count()).sum()
}

/// `Input::Text("x")` to the echo in a new frame, through the real PTY:
/// `cat` reads the terminal in line mode, so the echo is the line
/// discipline's, and every part of the path in this crate is in it (the
/// wake, the encoded write, the read, the parse, the frame). Each sample
/// starts after the screen was quiet for 12 ms, as after a pause in typing.
fn echo_latency(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (harness, cwd) = plugin_harness(ctx)?;
    let id = running(&harness, &cwd)?;
    let terminals = harness.terminals.clone();
    let _watch = terminals
        .watch(id)
        .ok_or_else(|| BenchError::new("no terminal to watch"))?;
    let frame = |terminals: &Arc<dyn Terminals>| terminals.frame(id).expect("a frame");
    let mut line = 0;
    let text: Arc<str> = Arc::from("x");
    let time = ctx.time_with_setup(
        || {
            // The line is erased (Ctrl-U) before it gets long.
            if line >= 60 {
                terminals.input(id, Input::Text(Arc::from("\u{15}")));
                let deadline = Instant::now() + Duration::from_secs(5);
                while typed(&frame(&terminals)) != 0 && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(1));
                }
                line = 0;
            }
            line += 1;
            std::thread::sleep(Duration::from_millis(12));
            let current = frame(&terminals);
            (typed(&current), current.generation)
        },
        |(before, mut generation)| {
            terminals.input(id, Input::Text(text.clone()));
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let current = frame(&terminals);
                if current.generation != generation {
                    generation = current.generation;
                    if typed(&current) > before {
                        return;
                    }
                }
                if Instant::now() >= deadline {
                    return;
                }
                std::hint::spin_loop();
            }
        },
    );
    Ok(vec![Measured::new(
        "terminal/echo_latency",
        Value::Time(time),
    )])
}
