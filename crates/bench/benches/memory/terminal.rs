//! `terminal/idle_footprint` and `terminal/scrollback_footprint`
//! (docs/design/terminal.md「性能」): the resident growth of a child process
//! (this benchmark binary again, `CHILD`) that opens one 120×40 terminal,
//! writes output into it and builds a frame. libghostty-vt allocates its
//! pages outside Rust's allocator, which `CountingAlloc` does not see, so
//! the child reports its resident size before and after, as `cache/memory`
//! does (docs/design/bench.md「进程内基准」).

use std::process::{Command, ExitCode};

use lectern_bench::{Bench, BenchError, Ctx, Measured, Value};
use lectern_terminal::testing::{self, Engine};

use crate::cache::resident;

/// Set to `idle` or `scrollback`, it makes this binary the child.
pub const CHILD: &str = "LECTERN_BENCH_TERMINAL_FOOTPRINT";
/// The child's report: `FOOTPRINT <before> <after> <scroll total>`.
const LINE: &str = "FOOTPRINT ";
const COLS: u16 = 120;
const ROWS: u16 = 40;

/// The child: `lines` is the scrollback scenario's line count (the idle
/// scenario writes 4 KiB).
pub fn child(scenario: &str, lines: usize) -> ExitCode {
    let output = match scenario {
        "idle" => testing::plain_output(4 << 10, COLS),
        _ => testing::colored_lines(lines, COLS),
    };
    let run = || -> Result<String, String> {
        let before = resident()?;
        let mut e = Engine::new(testing::size(COLS, ROWS), testing::theme());
        e.feed(&output);
        let frame = e.frame();
        let after = resident()?;
        Ok(format!("{LINE}{before} {after} {}", frame.scroll.total))
    };
    match run() {
        Ok(line) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("terminal footprint child: {e}");
            ExitCode::FAILURE
        }
    }
}

pub const IDLE: Bench = Bench {
    id: "terminal/idle_footprint",
    run: idle,
};

fn idle(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (grown, _) = launch(ctx, "idle", 0)?;
    Ok(vec![Measured::new(
        "terminal/idle_footprint",
        Value::Bytes(grown),
    )])
}

pub const SCROLLBACK: Bench = Bench {
    id: "terminal/scrollback_footprint",
    run: scrollback,
};

fn scrollback(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let lines = ctx.scale(10_000, 500);
    let (grown, total) = launch(ctx, "scrollback", lines)?;
    // The scrollback limit is in bytes (terminal design「与设计的差异」):
    // the note says how many of the lines it kept.
    Ok(vec![
        Measured::new("terminal/scrollback_footprint", Value::Bytes(grown))
            .with_note(Some(format!("{lines} lines written, {total} rows kept"))),
    ])
}

/// Runs the child five times (once in smoke mode): the growth of each run
/// and the scroll total of the last.
fn launch(ctx: &Ctx, scenario: &str, lines: usize) -> Result<(Vec<u64>, u64), BenchError> {
    let exe = std::env::current_exe()?;
    let mut grown = Vec::new();
    let mut total = 0;
    for _ in 0..ctx.scale(5, 1) {
        let out = Command::new(&exe)
            .arg(lines.to_string())
            .env(CHILD, scenario)
            .output()?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let line = stdout.lines().find_map(|l| l.strip_prefix(LINE));
        let (Some(line), true) = (line, out.status.success()) else {
            return Err(BenchError::new(format!(
                "the child failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        };
        let values: Vec<u64> = line
            .split(' ')
            .map(|v| v.parse())
            .collect::<Result<_, _>>()?;
        let [before, after, rows] = values[..] else {
            return Err(BenchError::new(format!("the child printed {line:?}")));
        };
        grown.push(after.saturating_sub(before));
        total = rows;
    }
    Ok((grown, total))
}
